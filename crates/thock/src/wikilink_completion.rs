//! Completions for internal references: typing `[[` in a vault note offers
//! the vault's other notes, fuzzy-filtered as the user keeps typing, so a
//! wiki-link can be added without remembering the exact file name. The
//! completions target what `markdown_conceal::resolve_wikilink_target`
//! resolves — the vault-relative path without its `.md` extension. Typing
//! `![` offers the vault's images folder the same way and writes the whole
//! `![alt](/images/…)` link (V39 §5.4). Typing `(@` offers the reminder
//! readings of what follows and writes the canonical token (V45 §8.1). The
//! provider delegates everything else to the editor's regular project-backed
//! completions.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use chrono::NaiveTime;
use editor::actions::ShowCompletions;
use editor::{CompletionProvider, Editor, EditorEvent, EditorMode};
use gpui::{App, Context, Entity, Task, Window};
use language::{Buffer, CodeLabel};
use project::{
    Completion, CompletionDisplayOptions, CompletionResponse, CompletionSource, Project,
};
use text::{Point, ToOffset as _, ToPoint as _};
use thock_sync_core::{DailyNotes, Due, token_text};
use util::ResultExt as _;

use crate::markdown_syntax;
use crate::reminder_calendar::PickReminderDate;
use crate::reminder_reading;
use crate::reminders::{ReminderClock, ReminderSettings, due_label, load_reminder_settings};
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
    let Some(note) = vault_markdown_file(editor, cx) else {
        return false;
    };
    let reminder_settings = Rc::new(RefCell::new(ReminderSettings::default()));
    install_provider(
        editor,
        note.images_dir,
        note.daily,
        reminder_settings.clone(),
        cx,
    );
    // The presets' time is the vault's; the defaults stand until it loads, and
    // when `.thock/reminders.toml` can't be read.
    if let Some(project) = editor.project() {
        let fs = project.read(cx).fs().clone();
        cx.spawn(async move |_, _| {
            if let Some(settings) = load_reminder_settings(&fs, &note.vault_root)
                .await
                .log_err()
            {
                *reminder_settings.borrow_mut() = settings;
            }
        })
        .detach();
    }
    true
}

/// Installs the provider and the insert-image action on an editor that
/// passed the vault gate. Split from `install` so tests can drive an editor
/// without a vault on the real filesystem.
fn install_provider(
    editor: &mut Editor,
    images_dir: String,
    daily: DailyNotes,
    reminder_settings: Rc<RefCell<ReminderSettings>>,
    cx: &mut Context<Editor>,
) {
    let provider = WikilinkCompletionProvider {
        project: editor.project().cloned(),
        images_dir,
        daily,
        reminder_settings,
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

/// What the provider needs to know about the vault a note is in.
struct VaultNote {
    images_dir: String,
    daily: DailyNotes,
    vault_root: PathBuf,
}

/// Whether the editor's buffer is a `.md` file under a Thock vault root,
/// returning what the provider needs from the vault when it is.
fn vault_markdown_file(editor: &Editor, cx: &App) -> Option<VaultNote> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    if file.path().extension() != Some("md") {
        return None;
    }
    let file = project::File::from_dyn(Some(file))?;
    let vault_root = file.worktree.read(cx).abs_path();
    match Vault::detect(&vault_root) {
        VaultStatus::Valid(vault) => Some(VaultNote {
            images_dir: vault.config.images.dir,
            daily: DailyNotes {
                dir: vault.config.daily.dir,
                filename: vault.config.daily.filename,
            },
            vault_root: vault_root.to_path_buf(),
        }),
        _ => None,
    }
}

struct WikilinkCompletionProvider {
    /// The editor's original provider target, so LSP and snippet completions
    /// keep working everywhere a wiki-link isn't being typed.
    project: Option<Entity<Project>>,
    images_dir: String,
    /// Where the vault's daily notes are, so a token in a daily note can
    /// drop its date (V45 §4.1).
    daily: DailyNotes,
    /// The vault's `.thock/reminders.toml`, once loaded.
    reminder_settings: Rc<RefCell<ReminderSettings>>,
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
        let tomorrow_at = self.reminder_settings.borrow().tomorrow_at;
        if let Some(response) =
            reminder_response(buffer, buffer_position, &self.daily, tomorrow_at, cx)
        {
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
            || reminder_query_offset(snapshot, position).is_some()
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

/// The reminder suggestions for the `(@…` under construction at
/// `buffer_position` (V45 §8.1), or `None` when the cursor isn't in one. A
/// query with no reading gets no items, so the list closes and `enter` breaks
/// the line.
fn reminder_response(
    buffer: &Entity<Buffer>,
    buffer_position: text::Anchor,
    daily: &DailyNotes,
    tomorrow_at: NaiveTime,
    cx: &Context<Editor>,
) -> Option<CompletionResponse> {
    let buffer = buffer.read(cx);
    let query_start = reminder_query_offset(buffer, buffer_position)?;
    let cursor = buffer_position.to_offset(buffer);
    let query: String = buffer.text_for_range(query_start..cursor).collect();
    // Markdown autocloses the `(`; the token replaces that `)` too.
    let closers = usize::from(buffer.chars_at(cursor).next() == Some(')'));
    let home_day = project::File::from_dyn(buffer.file())
        .and_then(|file| daily.day_of(file.path.as_unix_str()));
    let now = ReminderClock::global(cx).now();
    let today = now.date();

    let mut suggestions: Vec<(String, Due)> = Vec::new();
    if query.trim().is_empty() {
        for preset in reminder_reading::presets(now, tomorrow_at) {
            let reading = preset.reading;
            let label = match reading.time {
                Some(time) => format!("{} {}", preset.label, time.format("%H:%M")),
                None => preset.label.to_string(),
            };
            suggestions.push((label, Due::new(reading.date, reading.time)));
        }
    } else {
        let readings = reminder_reading::read(&query, now);
        for (index, reading) in readings.readings.iter().enumerate() {
            let due = Due::new(reading.date, reading.time);
            let mut label = due_label(due, today);
            if reading.already_past {
                label.push_str(" · already past");
            }
            if index == 0 && !readings.ignored.is_empty() {
                let ignored: Vec<String> = readings
                    .ignored
                    .iter()
                    .map(|word| format!("\"{word}\""))
                    .collect();
                label.push_str(&format!(" · ignores {}", ignored.join(", ")));
            }
            suggestions.push((label, due));
        }
    }

    let display_options = CompletionDisplayOptions {
        dynamic_width: true,
    };
    // An empty response still counts as the provider's answer, so fallback
    // word completions don't fill the list it closes.
    if suggestions.is_empty() {
        return Some(CompletionResponse {
            completions: Vec::new(),
            display_options,
            is_incomplete: true,
        });
    }

    let token_range =
        buffer.anchor_before(query_start - TOKEN_OPEN.len())..buffer.anchor_after(cursor + closers);
    // The readings already answer the query, so the menu's own fuzzy filter
    // must keep them all (`dec 2` would hide *Wed 2 Dec*): matching from the
    // cursor gives it an empty query, and the incomplete response asks again
    // on every keystroke.
    let cursor_anchor = buffer.anchor_after(cursor);
    let completion = |label: String, new_text: String, replace_range| Completion {
        replace_range,
        new_text,
        label: CodeLabel::new(label, 0..0, Vec::new()),
        documentation: None,
        source: CompletionSource::Custom,
        icon_path: None,
        icon_color: None,
        match_start: Some(cursor_anchor),
        snippet_deduplication_key: None,
        insert_text_mode: None,
        confirm: None,
        group: None,
    };
    let mut completions: Vec<Completion> = suggestions
        .into_iter()
        .map(|(label, due)| completion(label, token_text(due, home_day), token_range.clone()))
        .collect();
    completions.push(Completion {
        // The calendar reads the typed `(@…` itself and replaces it.
        confirm: Some(Arc::new(|_, window, cx| {
            window.dispatch_action(Box::new(PickReminderDate), cx);
            false
        })),
        ..completion(
            "Pick a date…".to_string(),
            String::new(),
            cursor_anchor..cursor_anchor,
        )
    });

    Some(CompletionResponse {
        completions,
        display_options,
        is_incomplete: true,
    })
}

const TOKEN_OPEN: &str = "(@";

/// The buffer offset where the reminder query starts — just after an open
/// `(@` before the cursor on its line — when that `(@` is in prose and the
/// line is not a calendar line (V45 §4.2).
fn reminder_query_offset(buffer: &Buffer, position: text::Anchor) -> Option<usize> {
    let query_start = query_offset(buffer, position, reminder_query_start)?;
    let row = position.to_point(buffer).row;
    let line: String = buffer
        .text_for_range(Point::new(row, 0)..Point::new(row, buffer.line_len(row)))
        .collect();
    if line.contains("<!--gcal:") {
        return None;
    }
    markdown_syntax::is_prose_at(&buffer.text(), query_start - TOKEN_OPEN.len())
        .then_some(query_start)
}

/// Where the reminder query starts inside `line_prefix`: just after the
/// rightmost `(@` with no `)` between it and the cursor. An escaped `\(@` is
/// text.
fn reminder_query_start(line_prefix: &str) -> Option<usize> {
    let open = line_prefix.rfind(TOKEN_OPEN)?;
    if line_prefix[open..].contains(')') || line_prefix[..open].ends_with('\\') {
        return None;
    }
    Some(open + TOKEN_OPEN.len())
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
    use super::*;
    use chrono::NaiveDate;
    use editor::code_context_menus::CodeContextMenu;
    use fs::FakeFs;
    use gpui::{BorrowAppContext as _, KeyBinding, TestAppContext, VisualTestContext};
    use project::Project;
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use workspace::{MultiWorkspace, Workspace};

    #[test]
    fn an_open_reminder_token_starts_a_query() {
        assert_eq!(reminder_query_start("Call (@"), Some(7));
        assert_eq!(reminder_query_start("Call (@dec 2"), Some(7));
        assert_eq!(reminder_query_start("(@a) then (@to"), Some(12));
        assert_eq!(reminder_query_start("Call (@2026-12-02)"), None);
        assert_eq!(reminder_query_start("Call \\(@dec"), None);
        assert_eq!(reminder_query_start("Call (dec"), None);
    }

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

    fn keymap_bindings(content: &str, cx: &App) -> Vec<KeyBinding> {
        match KeymapFile::load(content, cx) {
            KeymapFileLoadResult::Success { key_bindings }
            | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
            KeymapFileLoadResult::JsonParseFailure { error } => panic!("bad keymap: {error}"),
        }
    }

    struct Setup {
        workspace: Entity<Workspace>,
        editor: Entity<Editor>,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    /// A vault whose note `open` holds `text`, open in a workspace with the
    /// cursor at the end of `text`, on Wednesday 21 October 2026 at 14:05.
    async fn setup(cx: &mut TestAppContext, open: &str, text: &str) -> Setup {
        setup_with(cx, open, text, None).await
    }

    /// `setup`, with `reminders_toml` as the vault's `.thock/reminders.toml`.
    async fn setup_with(
        cx: &mut TestAppContext,
        open: &str,
        text: &str,
        reminders_toml: Option<&str>,
    ) -> Setup {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
            cx.bind_keys(keymap_bindings(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            ));
            let now = NaiveDate::from_ymd_opt(2026, 10, 21)
                .unwrap()
                .and_time(NaiveTime::from_hms_opt(14, 5, 0).unwrap());
            crate::reminders::TestClock::install(now, cx);
            init(cx);
            crate::reminder_calendar::init(cx);
        });
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        let fs = FakeFs::new(cx.executor());
        let mut tree = json!({ ".thock": { "config.toml": "schema = 1\n" } });
        if let Some(reminders_toml) = reminders_toml {
            tree[".thock"]["reminders.toml"] = json!(reminders_toml);
        }
        let (folder, file) = open.split_once('/').unwrap();
        tree[folder][file] = json!(text);
        fs.insert_tree(&root, tree).await;
        let project = Project::test(fs, [root.as_path()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        let project_path = project
            .read_with(&mut cx, |project, cx| {
                project.find_project_path(root.join(open), cx)
            })
            .unwrap();
        let editor = workspace
            .update_in(&mut cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.move_to_end(&Default::default(), window, cx);
        });
        cx.run_until_parked();
        Setup {
            workspace,
            editor,
            cx,
            _vault_dir: vault_dir,
        }
    }

    /// Types `input` the way Markdown's autoclose leaves it: an opening `(`
    /// gets its `)` and the cursor stays inside.
    fn type_text(setup: &mut Setup, input: &str) {
        for character in input.chars() {
            setup.cx.simulate_input(&character.to_string());
            if character == '(' {
                setup.editor.update_in(&mut setup.cx, |editor, window, cx| {
                    editor.insert(")", window, cx);
                    editor.move_left(&Default::default(), window, cx);
                });
            }
        }
        setup.cx.run_until_parked();
    }

    fn labels(setup: &mut Setup) -> Vec<String> {
        setup.editor.read_with(&setup.cx, |editor, _| {
            match editor.context_menu().borrow().as_ref() {
                Some(CodeContextMenu::Completions(menu)) => {
                    let completions = menu.completions.borrow();
                    menu.entries
                        .borrow()
                        .iter()
                        .filter_map(|entry| entry.as_match())
                        .map(|found| completions[found.candidate_id].label.text.clone())
                        .collect()
                }
                _ => Vec::new(),
            }
        })
    }

    fn text(setup: &mut Setup) -> String {
        setup
            .editor
            .read_with(&setup.cx, |editor, cx| editor.text(cx))
    }

    #[gpui::test]
    async fn an_open_token_offers_the_presets(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Renew passport ").await;
        type_text(&mut setup, "(@");
        assert_eq!(
            labels(&mut setup),
            [
                "Later today 17:00",
                "Tomorrow 09:00",
                "Monday 09:00",
                "Pick a date…"
            ]
        );
    }

    #[gpui::test]
    async fn the_presets_ring_at_the_vaults_tomorrow_time(cx: &mut TestAppContext) {
        let mut setup = setup_with(
            cx,
            "projects/house.md",
            "Renew passport ",
            Some("[ring]\ntomorrow_at = \"08:30\"\n"),
        )
        .await;
        type_text(&mut setup, "(@");
        assert_eq!(
            labels(&mut setup),
            [
                "Later today 17:00",
                "Tomorrow 08:30",
                "Monday 08:30",
                "Pick a date…"
            ]
        );
    }

    #[gpui::test]
    async fn fallback_words_never_fill_a_closed_list(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "dentistry dentists\nCall ").await;
        setup.cx.update(|_, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.project.all_languages.defaults.completions =
                        Some(settings::CompletionSettingsContent {
                            words: Some(settings::WordsCompletionMode::Fallback),
                            words_min_length: Some(0),
                            ..Default::default()
                        });
                });
            });
        });
        type_text(&mut setup, "dent");
        assert!(
            labels(&mut setup).contains(&"dentistry".to_string()),
            "fallback word completions are on: {:?}",
            labels(&mut setup)
        );
        setup.cx.simulate_keystrokes("escape");
        type_text(&mut setup, " (@dentist");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
    }

    #[gpui::test]
    async fn every_reading_survives_the_filter_and_tab_writes_the_date(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Renew passport ").await;
        type_text(&mut setup, "(@dec 2");
        assert_eq!(labels(&mut setup), ["Wed 2 Dec", "Pick a date…"]);
        setup.cx.simulate_keystrokes("tab");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), "Renew passport (@2026-12-02)");
    }

    #[gpui::test]
    async fn readings_name_what_they_ignore_and_what_has_passed(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Standup ").await;
        type_text(&mut setup, "(@foo today 9am");
        assert_eq!(
            labels(&mut setup),
            [
                "Tomorrow 09:00 · already past · ignores \"foo\"",
                "Pick a date…"
            ]
        );
    }

    #[gpui::test]
    async fn a_bare_hour_offers_both_sides_of_noon(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Standup ").await;
        type_text(&mut setup, "(@10");
        assert_eq!(
            labels(&mut setup),
            ["Today 22:00", "Tomorrow 10:00", "Pick a date…"]
        );
    }

    #[gpui::test]
    async fn a_token_in_its_own_daily_note_drops_the_date(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "daily/2026-10-22.md", "- [ ] Dentist ").await;
        type_text(&mut setup, "(@tomorrow 10:00");
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), "- [ ] Dentist (@10:00)");
    }

    #[gpui::test]
    async fn a_query_with_no_reading_closes_the_list(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Call ").await;
        type_text(&mut setup, "(@dentist");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), "Call (@dentist\n)");
    }

    /// Puts the cursor at `column` of the first line.
    fn move_cursor(setup: &mut Setup, column: usize) {
        setup.editor.update_in(&mut setup.cx, |editor, window, cx| {
            let point = Point::new(0, column as u32);
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([point..point])
            });
        });
    }

    #[gpui::test]
    async fn no_list_in_inline_code(cx: &mut TestAppContext) {
        let line = "Call `(@to`";
        let mut setup = setup(cx, "projects/house.md", line).await;
        move_cursor(&mut setup, line.len() - 1);
        type_text(&mut setup, "m");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
    }

    #[gpui::test]
    async fn no_list_on_a_calendar_line(cx: &mut TestAppContext) {
        let line = "- [ ] 10:00 Sync () <!--gcal:abc-->";
        let mut setup = setup(cx, "daily/2026-10-21.md", line).await;
        move_cursor(&mut setup, line.find("()").unwrap() + 1);
        type_text(&mut setup, "@tom");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
    }

    #[gpui::test]
    async fn pick_a_date_opens_the_calendar(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Renew passport ").await;
        type_text(&mut setup, "(@dec 2");
        setup.cx.simulate_keystrokes("down enter");
        setup.cx.run_until_parked();
        let calendar = setup.workspace.read_with(&setup.cx, |workspace, cx| {
            workspace.active_modal::<crate::reminder_calendar::ReminderCalendar>(cx)
        });
        assert!(calendar.is_some(), "Pick a date… opens the calendar");
        assert_eq!(text(&mut setup), "Renew passport (@dec 2)");
    }
}
