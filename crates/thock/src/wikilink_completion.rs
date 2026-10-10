//! Completions for internal references: typing `[[` in a vault note offers
//! the vault's other notes, fuzzy-filtered as the user keeps typing, so a
//! wiki-link can be added without remembering the exact file name. The
//! completions target what `markdown_conceal::resolve_wikilink_target`
//! resolves — the vault-relative path without its `.md` extension. Typing
//! `![` offers the vault's images folder the same way and writes the whole
//! `![alt](/images/…)` link (V39 §5.4). The provider delegates everything
//! else to the editor's regular project-backed completions.

use std::rc::Rc;

use editor::actions::ShowCompletions;
use editor::{CompletionProvider, Editor, EditorEvent, EditorMode};
use gpui::{App, Context, Entity, Task, Window};
use language::{Buffer, CodeLabel};
use project::{
    Completion, CompletionDisplayOptions, CompletionResponse, CompletionSource, Project,
};
use text::ToOffset as _;

use crate::vault::{Vault, VaultStatus};

gpui::actions!(
    thock,
    [
        /// Inserts a picture from the vault's images folder at the cursor.
        InsertImage
    ]
);

/// The picture formats a note may link and the editor will draw (V39 §6.1).
pub const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

pub fn init(cx: &mut App) {
    cx.observe_new(|editor: &mut Editor, _, cx| register(editor, cx))
        .detach();
}

/// Installs the provider on editors where wiki-links apply: a full, writable,
/// singleton editor over a `.md` file that lives under a Thock vault root —
/// the same gate as markdown conceal. Every other editor keeps its default
/// completions.
fn register(editor: &mut Editor, cx: &mut Context<Editor>) {
    if !editor.mode().is_full()
        || matches!(editor.mode(), EditorMode::Minimap { .. })
        || editor.read_only(cx)
    {
        return;
    }
    if install(editor, cx) {
        return;
    }
    // The buffer may acquire a qualifying file later — an untitled buffer
    // saved as `note.md` into a vault — so retry when the file handle changes.
    cx.subscribe(&cx.entity(), |editor, _, event: &EditorEvent, cx| {
        if matches!(event, EditorEvent::FileHandleChanged) {
            install(editor, cx);
        }
    })
    .detach();
}

fn install(editor: &mut Editor, cx: &mut Context<Editor>) -> bool {
    let Some(images_dir) = vault_markdown_file(editor, cx) else {
        return false;
    };
    install_provider(editor, images_dir, cx);
    true
}

/// Installs the provider and the insert-image action on an editor that
/// passed the vault gate. Split from `install` so tests can drive an editor
/// without a vault on the real filesystem.
fn install_provider(editor: &mut Editor, images_dir: String, cx: &mut Context<Editor>) {
    let provider = WikilinkCompletionProvider {
        project: editor.project().cloned(),
        images_dir,
    };
    editor.set_completion_provider(Some(Rc::new(provider)));
    let editor_handle = cx.weak_entity();
    editor
        .register_action::<InsertImage>(move |_, window, cx| {
            editor_handle
                .update(cx, |editor, cx| insert_image(editor, window, cx))
                .ok();
        })
        .detach();
}

/// Starts an image link at the cursor and opens its completions: the person
/// picks the picture from the list, as if they had typed `![` themselves.
fn insert_image(editor: &mut Editor, window: &mut Window, cx: &mut Context<Editor>) {
    if editor.read_only(cx) {
        return;
    }
    editor.handle_input("![", window, cx);
    editor.show_completions(&ShowCompletions, window, cx);
}

/// Whether the editor's buffer is a `.md` file under a Thock vault root,
/// returning the vault's images folder when it is.
fn vault_markdown_file(editor: &Editor, cx: &App) -> Option<String> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    if file.path().extension() != Some("md") {
        return None;
    }
    let file = project::File::from_dyn(Some(file))?;
    let vault_root = file.worktree.read(cx).abs_path();
    match Vault::detect(&vault_root) {
        VaultStatus::Valid(vault) => Some(vault.config.images.dir),
        _ => None,
    }
}

struct WikilinkCompletionProvider {
    /// The editor's original provider target, so LSP and snippet completions
    /// keep working everywhere a wiki-link isn't being typed.
    project: Option<Entity<Project>>,
    images_dir: String,
}

impl CompletionProvider for WikilinkCompletionProvider {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: text::Anchor,
        trigger: editor::CompletionContext,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<anyhow::Result<Vec<CompletionResponse>>> {
        if let Some(response) = wikilink_response(buffer, buffer_position, cx) {
            return Task::ready(Ok(vec![response]));
        }
        if let Some(response) = image_response(buffer, buffer_position, &self.images_dir, cx) {
            return Task::ready(Ok(vec![response]));
        }
        match &self.project {
            Some(project) => project.completions(buffer, buffer_position, trigger, window, cx),
            None => Task::ready(Ok(Vec::new())),
        }
    }

    fn resolve_completions(
        &self,
        buffer: Entity<Buffer>,
        completion_indices: Vec<usize>,
        completions: Rc<std::cell::RefCell<Box<[Completion]>>>,
        cx: &mut Context<Editor>,
    ) -> Task<anyhow::Result<bool>> {
        match &self.project {
            Some(project) => {
                project.resolve_completions(buffer, completion_indices, completions, cx)
            }
            None => Task::ready(Ok(false)),
        }
    }

    fn apply_additional_edits_for_completion(
        &self,
        buffer: Entity<Buffer>,
        completions: Rc<std::cell::RefCell<Box<[Completion]>>>,
        completion_index: usize,
        push_to_history: bool,
        all_commit_ranges: Vec<std::ops::Range<language::Anchor>>,
        cx: &mut Context<Editor>,
    ) -> Task<anyhow::Result<Option<language::Transaction>>> {
        match &self.project {
            Some(project) => project.apply_additional_edits_for_completion(
                buffer,
                completions,
                completion_index,
                push_to_history,
                all_commit_ranges,
                cx,
            ),
            None => Task::ready(Ok(None)),
        }
    }

    fn is_completion_trigger(
        &self,
        buffer: &Entity<Buffer>,
        position: language::Anchor,
        text: &str,
        trigger_in_words: bool,
        cx: &mut Context<Editor>,
    ) -> bool {
        let snapshot = buffer.read(cx);
        if wikilink_query_offset(snapshot, position).is_some()
            || image_query_offset(snapshot, position).is_some()
        {
            return true;
        }
        self.project.as_ref().is_some_and(|project| {
            project.is_completion_trigger(buffer, position, text, trigger_in_words, cx)
        })
    }

    fn show_snippets(&self) -> bool {
        self.project.is_some()
    }
}

/// The vault-note completions for the wiki-link under construction at
/// `buffer_position`, or `None` when the cursor isn't inside one.
fn wikilink_response(
    buffer: &Entity<Buffer>,
    buffer_position: text::Anchor,
    cx: &Context<Editor>,
) -> Option<CompletionResponse> {
    let buffer = buffer.read(cx);
    let query_start = wikilink_query_offset(buffer, buffer_position)?;
    let cursor = buffer_position.to_offset(buffer);

    // Autoclose usually has the `]]` (or a single `]`) already sitting after
    // the cursor; consuming it and re-adding it in `new_text` leaves the
    // cursor after the finished link either way.
    let following: String = buffer.chars_at(cursor).take(2).collect();
    let closers = closing_bracket_len(&following);

    let file = project::File::from_dyn(buffer.file())?;
    let worktree = file.worktree.read(cx);
    let own_path = file.path.clone();
    let mut targets: Vec<String> = worktree
        .files(false, 0)
        .filter(|entry| entry.path.as_ref() != own_path.as_ref())
        .filter_map(|entry| {
            entry
                .path
                .as_unix_str()
                .strip_suffix(".md")
                .map(str::to_string)
        })
        .collect();
    targets.sort();

    let replace_range = buffer.anchor_before(query_start)..buffer.anchor_after(cursor + closers);
    let match_start = buffer.anchor_before(query_start);
    let completions = targets
        .into_iter()
        .map(|target| Completion {
            replace_range: replace_range.clone(),
            new_text: format!("{target}]]"),
            label: CodeLabel::plain(target, None),
            documentation: None,
            source: CompletionSource::Custom,
            icon_path: None,
            icon_color: None,
            // Everything typed after the `[[` is the fuzzy query, so the
            // menu keeps filtering across spaces and slashes.
            match_start: Some(match_start),
            snippet_deduplication_key: None,
            insert_text_mode: None,
            confirm: None,
            group: None,
        })
        .collect();

    Some(CompletionResponse {
        completions,
        display_options: CompletionDisplayOptions {
            dynamic_width: true,
        },
        is_incomplete: false,
    })
}

/// The images-folder completions for the image link under construction at
/// `buffer_position`, or `None` when the cursor isn't inside one. Newest
/// pictures first: their names start with the day they were taken.
fn image_response(
    buffer: &Entity<Buffer>,
    buffer_position: text::Anchor,
    images_dir: &str,
    cx: &Context<Editor>,
) -> Option<CompletionResponse> {
    let buffer = buffer.read(cx);
    let query_start = image_query_offset(buffer, buffer_position)?;
    let cursor = buffer_position.to_offset(buffer);
    let following: String = buffer.chars_at(cursor).take(2).collect();
    let closers = closing_bracket_len(&following);

    let file = project::File::from_dyn(buffer.file())?;
    let worktree = file.worktree.read(cx);
    let prefix = format!("{images_dir}/");
    let mut images: Vec<String> = worktree
        .files(false, 0)
        .filter(|entry| {
            entry
                .path
                .extension()
                .is_some_and(|extension| is_image_extension(extension))
        })
        .map(|entry| entry.path.as_unix_str().to_string())
        .filter(|path| path.starts_with(&prefix))
        .collect();
    images.sort_by(|a, b| b.cmp(a));

    let replace_range = buffer.anchor_before(query_start)..buffer.anchor_after(cursor + closers);
    let match_start = buffer.anchor_before(query_start);
    let completions = images
        .into_iter()
        .map(|path| {
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            Completion {
                replace_range: replace_range.clone(),
                new_text: format!("{}](/{path})", image_slug(&name)),
                label: CodeLabel::plain(name, None),
                documentation: None,
                source: CompletionSource::Custom,
                icon_path: None,
                icon_color: None,
                match_start: Some(match_start),
                snippet_deduplication_key: None,
                insert_text_mode: None,
                confirm: None,
                group: None,
            }
        })
        .collect();

    Some(CompletionResponse {
        completions,
        display_options: CompletionDisplayOptions {
            dynamic_width: true,
        },
        is_incomplete: false,
    })
}

fn is_image_extension(extension: &str) -> bool {
    IMAGE_EXTENSIONS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(extension))
}

/// The alt text an inserted link starts with: the picture's name without
/// the date and time the capture gave it, or the whole stem when it has none.
fn image_slug(name: &str) -> &str {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let bytes = stem.as_bytes();
    let dated = bytes.len() > 16
        && bytes[..10]
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 => *byte == b'-',
                _ => byte.is_ascii_digit(),
            })
        && bytes[10] == b'-'
        && bytes[11..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'-';
    if dated { &stem[16..] } else { stem }
}

/// The buffer offset where the wiki-link target under construction starts —
/// just after the rightmost unclosed `[[` before the cursor on its line.
fn wikilink_query_offset(buffer: &Buffer, position: text::Anchor) -> Option<usize> {
    query_offset(buffer, position, wikilink_query_start)
}

/// The buffer offset where the image query under construction starts — just
/// after the rightmost unclosed `![` before the cursor on its line.
fn image_query_offset(buffer: &Buffer, position: text::Anchor) -> Option<usize> {
    query_offset(buffer, position, image_query_start)
}

fn query_offset(
    buffer: &Buffer,
    position: text::Anchor,
    start_in: fn(&str) -> Option<usize>,
) -> Option<usize> {
    let cursor = position.to_offset(buffer);
    let mut prefix: Vec<char> = Vec::new();
    for character in buffer.reversed_chars_at(cursor) {
        if character == '\n' || prefix.len() >= 512 {
            break;
        }
        prefix.push(character);
    }
    prefix.reverse();
    let line_prefix: String = prefix.into_iter().collect();
    let start_in_prefix = start_in(&line_prefix)?;
    Some(cursor - (line_prefix.len() - start_in_prefix))
}

/// Where the image query starts inside `line_prefix`: the byte offset just
/// after the rightmost `![` that is still open. `None` once a bracket or a
/// parenthesis follows it (the link is past its alt text), or when it opens
/// an `![[embed]]`, which is not a picture.
fn image_query_start(line_prefix: &str) -> Option<usize> {
    let open = line_prefix.rfind("![")?;
    let query = &line_prefix[open + 2..];
    if query.contains(['[', ']', '(', ')']) {
        return None;
    }
    Some(open + 2)
}

/// Where the target starts inside `line_prefix` (the line's text up to the
/// cursor): the byte offset just after the rightmost `[[` that is still open.
/// `None` once the link is closed, aliased with `|`, or not there at all.
fn wikilink_query_start(line_prefix: &str) -> Option<usize> {
    let open = line_prefix.rfind("[[")?;
    let target = &line_prefix[open + 2..];
    if target.contains('[') || target.contains(']') || target.contains('|') {
        return None;
    }
    Some(open + 2)
}

/// How many of the (at most two) characters after the cursor are `]`s that
/// belong to the link being completed.
fn closing_bracket_len(following: &str) -> usize {
    following
        .chars()
        .take(2)
        .take_while(|character| *character == ']')
        .count()
}

#[cfg(test)]
mod tests {
    use super::{closing_bracket_len, image_query_start, image_slug, wikilink_query_start};

    #[test]
    fn a_freshly_opened_image_link_starts_an_empty_query() {
        assert_eq!(image_query_start("see !["), Some(6));
        assert_eq!(image_query_start("!["), Some(2));
        assert_eq!(image_query_start("![white"), Some(2));
    }

    #[test]
    fn text_past_the_alt_or_outside_an_image_link_is_not_a_query() {
        assert_eq!(image_query_start("plain text"), None);
        assert_eq!(image_query_start("[[note"), None);
        assert_eq!(image_query_start("![alt]"), None);
        assert_eq!(image_query_start("![alt](/images/a"), None);
        assert_eq!(image_query_start("![[embed"), None);
        assert_eq!(image_query_start("![done](a.png) then"), None);
    }

    #[test]
    fn the_inserted_alt_drops_the_capture_stamp() {
        assert_eq!(image_slug("2026-10-10-0931-whiteboard.jpg"), "whiteboard");
        assert_eq!(
            image_slug("2026-10-10-0931-whiteboard-2.jpg"),
            "whiteboard-2"
        );
        assert_eq!(image_slug("receipt.png"), "receipt");
        assert_eq!(image_slug("2026-10-10-0931-.png"), "2026-10-10-0931-");
        assert_eq!(image_slug("noext"), "noext");
    }

    #[test]
    fn a_freshly_opened_wikilink_starts_an_empty_query() {
        assert_eq!(wikilink_query_start("see [["), Some(6));
        assert_eq!(wikilink_query_start("[["), Some(2));
    }

    #[test]
    fn the_query_runs_from_the_brackets_to_the_cursor() {
        assert_eq!(wikilink_query_start("see [[week pla"), Some(6));
        assert_eq!(wikilink_query_start("an ![[embedded no"), Some(6));
        assert_eq!(wikilink_query_start("[[a]] then [[b"), Some(13));
    }

    #[test]
    fn text_outside_an_open_wikilink_is_not_a_query() {
        assert_eq!(wikilink_query_start("plain text"), None);
        assert_eq!(wikilink_query_start("closed [[link]]"), None);
        assert_eq!(wikilink_query_start("[markdown](link)"), None);
        assert_eq!(wikilink_query_start(""), None);
    }

    #[test]
    fn an_aliased_or_nested_bracket_ends_the_query() {
        assert_eq!(wikilink_query_start("[[target|alias"), None);
        assert_eq!(wikilink_query_start("[[a[b"), None);
    }

    #[test]
    fn existing_closing_brackets_are_consumed_not_duplicated() {
        assert_eq!(closing_bracket_len("]]"), 2);
        assert_eq!(closing_bracket_len("]] and more"), 2);
        assert_eq!(closing_bracket_len("]"), 1);
        assert_eq!(closing_bracket_len("] word"), 1);
        assert_eq!(closing_bracket_len("word"), 0);
        assert_eq!(closing_bracket_len(""), 0);
    }
}
