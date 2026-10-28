//! Completions for internal references: typing `[[` in a vault note offers
//! the vault's other notes, fuzzy-filtered as the user keeps typing, so a
//! wiki-link can be added without remembering the exact file name. The
//! completions target what `markdown_conceal::resolve_wikilink_target`
//! resolves — the vault-relative path without its `.md` extension. Typing
//! `![` offers the vault's images folder the same way and writes the whole
//! `![alt](/images/…)` link (V39 §5.4). Typing `(@` offers the reminder
//! readings of what follows and writes the canonical token (V45 §8.1).
//! Typing `/` at the start of a line or after a space offers formatting by
//! name — headings, lists, quotes, emphasis, links — so a note can be shaped
//! without knowing Markdown (V49 §5). The provider delegates everything else
//! to the editor's regular project-backed completions.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{NaiveDateTime, NaiveTime};
use editor::actions::ShowCompletions;
use editor::{CompletionProvider, Editor, EditorEvent, EditorMode};
use gpui::{App, Context, Entity, Task, WeakEntity, Window};
use language::{Buffer, CodeLabel};
use multi_buffer::{MultiBufferOffset, ToOffset as _};
use project::{
    Completion, CompletionDisplayOptions, CompletionGroup, CompletionResponse, CompletionSource,
    Project,
};
use text::{Point, ToOffset as _, ToPoint as _};
use thock_sync_core::{DailyNotes, Due, token_text};
use util::ResultExt as _;

use crate::markdown_conceal::record_slash_pick;
use crate::markdown_syntax::{self, Block};
use crate::reminder_calendar::PickReminderDate;
use crate::reminder_reading;
use crate::reminders::{ReminderClock, ReminderSettings, due_label, load_reminder_settings};
use crate::vault::{Vault, VaultStatus};

gpui::actions!(
    thock,
    [
        /// Inserts a picture from the vault's images folder at the cursor.
        InsertImage,
        /// Chooses a heading, list, quote or style to insert at the cursor.
        InsertFormatting
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
    if let Some(project) = editor.project().cloned() {
        let observed = crate::reminder_service::observe_settings(&project, cx, {
            let reminder_settings = reminder_settings.clone();
            move |_, settings, _| *reminder_settings.borrow_mut() = settings.clone()
        });
        if let Some(settings) = observed {
            *reminder_settings.borrow_mut() = settings;
            return true;
        }
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
    let editor_handle = cx.weak_entity();
    editor
        .register_action::<InsertFormatting>(move |_, window, cx| {
            editor_handle
                .update(cx, |editor, cx| insert_formatting(editor, window, cx))
                .ok();
        })
        .detach();
}

/// Types the `/` that opens the formatting menu, with a space before it when
/// the cursor follows a word, and opens the menu (V49 §5.5).
fn insert_formatting(editor: &mut Editor, window: &mut Window, cx: &mut Context<Editor>) {
    if editor.read_only(cx) {
        return;
    }
    let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
    let head = editor.selections.newest_anchor().head();
    let follows_word = buffer_snapshot
        .reversed_chars_at(head.to_offset(&buffer_snapshot))
        .next()
        .is_some_and(|character| !character.is_whitespace());
    editor.handle_input(if follows_word { " /" } else { "/" }, window, cx);
    editor.show_completions(&ShowCompletions, window, cx);
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
        if let Some(response) = slash_response(buffer, buffer_position, cx) {
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
            || slash_query_offset(snapshot, position).is_some()
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

/// What a `/` menu entry does (V49 §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    /// Rewrites the line's block prefix.
    Block(Block),
    Divider,
    CodeBlock,
    Table,
    /// Inserts a pair of the same delimiter with the cursor between.
    Wrap(&'static str),
    Link,
    /// Types the opening of another completion, which then takes over.
    Trigger(&'static str),
    Date,
    Time,
}

struct FormatEntry {
    label: &'static str,
    aliases: &'static [&'static str],
    group: &'static str,
    format: Format,
}

const fn entry(
    label: &'static str,
    aliases: &'static [&'static str],
    group: &'static str,
    format: Format,
) -> FormatEntry {
    FormatEntry {
        label,
        aliases,
        group,
        format,
    }
}

const BLOCKS: &str = "Blocks";
const INLINE: &str = "Inline";
const THOCK: &str = "Thock";

const FORMAT_ENTRIES: &[FormatEntry] = &[
    entry(
        "Task",
        &["todo", "checkbox"],
        BLOCKS,
        Format::Block(Block::Task),
    ),
    entry("Bullet", &["list"], BLOCKS, Format::Block(Block::Bullet)),
    entry("Numbered", &["ol"], BLOCKS, Format::Block(Block::Numbered)),
    entry(
        "Heading 1",
        &["h1", "title"],
        BLOCKS,
        Format::Block(Block::Heading(1)),
    ),
    entry(
        "Heading 2",
        &["h2"],
        BLOCKS,
        Format::Block(Block::Heading(2)),
    ),
    entry(
        "Heading 3",
        &["h3"],
        BLOCKS,
        Format::Block(Block::Heading(3)),
    ),
    entry(
        "Quote",
        &["blockquote"],
        BLOCKS,
        Format::Block(Block::Quote),
    ),
    entry("Divider", &["hr", "rule", "line"], BLOCKS, Format::Divider),
    entry("Code block", &["fence"], BLOCKS, Format::CodeBlock),
    entry("Table", &[], BLOCKS, Format::Table),
    entry("Bold", &["b"], INLINE, Format::Wrap("**")),
    entry("Italic", &["i"], INLINE, Format::Wrap("_")),
    entry("Strikethrough", &["s"], INLINE, Format::Wrap("~~")),
    entry("Code", &[], INLINE, Format::Wrap("`")),
    entry("Link", &["url"], INLINE, Format::Link),
    entry(
        "Link to note",
        &["wiki", "page"],
        THOCK,
        Format::Trigger("[["),
    ),
    entry("Image", &["picture"], THOCK, Format::Trigger("![")),
    entry("Reminder", &["remind"], THOCK, Format::Trigger(TOKEN_OPEN)),
    entry("Today's date", &["date"], THOCK, Format::Date),
    entry("Time now", &["time"], THOCK, Format::Time),
];

/// The entries `query` picks out, best first: an exact label or alias, then
/// one it starts, then a word of a label it starts, then any label or alias
/// it is a subsequence of. Catalog order breaks ties.
fn matching_entries(query: &str) -> Vec<&'static FormatEntry> {
    let query = query.to_lowercase();
    let rank = |candidate: &str| -> Option<u8> {
        let candidate = candidate.to_lowercase();
        if candidate == query {
            Some(0)
        } else if candidate.starts_with(&query) {
            Some(1)
        } else if candidate.split(' ').any(|word| word.starts_with(&query)) {
            Some(2)
        } else {
            let mut characters = candidate.chars();
            query
                .chars()
                .all(|wanted| characters.any(|character| character == wanted))
                .then_some(3)
        }
    };
    let mut ranked: Vec<(u8, &FormatEntry)> = FORMAT_ENTRIES
        .iter()
        .filter_map(|entry| {
            std::iter::once(entry.label)
                .chain(entry.aliases.iter().copied())
                .filter_map(rank)
                .min()
                .map(|rank| (rank, entry))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

/// A pick's edit, line-relative: the range it replaces, the text it puts
/// there, and where in that text the cursor lands.
#[derive(Debug, PartialEq, Eq)]
struct FormatEdit {
    range: std::ops::Range<usize>,
    text: String,
    cursor: usize,
}

/// What picking `format` does to `line`, whose `/query` runs from byte
/// `slash` to the cursor at byte `cursor` (V49 §5.4). The `/query` goes
/// first; a block entry also takes the space it was typed after, so
/// `- buy milk /h2` becomes `## buy milk`.
fn format_edit(
    line: &str,
    slash: usize,
    cursor: usize,
    format: Format,
    now: NaiveDateTime,
) -> FormatEdit {
    let query = slash..cursor;
    let inline = |text: String, at: usize| FormatEdit {
        range: query.clone(),
        cursor: at,
        text,
    };
    match format {
        Format::Wrap(delimiter) => inline(delimiter.repeat(2), delimiter.len()),
        Format::Link => inline("[]()".to_string(), 1),
        Format::Trigger(open) => inline(open.to_string(), open.len()),
        Format::Date => {
            let date = now.format("%Y-%m-%d").to_string();
            let at = date.len();
            inline(date, at)
        }
        Format::Time => {
            let time = now.format("%H:%M").to_string();
            let at = time.len();
            inline(time, at)
        }
        Format::Block(block) => {
            let (base, at) = without_query(line, slash, cursor);
            let (text, cursor) = markdown_syntax::apply_block(&base, block, at);
            FormatEdit {
                range: 0..line.len(),
                text,
                cursor,
            }
        }
        Format::Divider | Format::CodeBlock | Format::Table => {
            let (base, _) = without_query(line, slash, cursor);
            let indent = &base[..base.len() - base.trim_start_matches([' ', '\t']).len()];
            let (lines, cursor_line, cursor_column): (&[&str], usize, usize) = match format {
                Format::Divider => (&["___"], 0, 3),
                Format::CodeBlock => (&["```", "", "```"], 1, 0),
                _ => (&["|  |  |", "| --- | --- |", "|  |  |"], 0, 2),
            };
            let mut text = String::new();
            let mut cursor = 0;
            if !base.trim().is_empty() {
                text.push_str(&base);
                text.push('\n');
            }
            for (index, block_line) in lines.iter().enumerate() {
                if index > 0 {
                    text.push('\n');
                }
                if index == cursor_line {
                    cursor = text.len() + indent.len() + cursor_column;
                }
                text.push_str(indent);
                text.push_str(block_line);
            }
            FormatEdit {
                range: 0..line.len(),
                text,
                cursor,
            }
        }
    }
}

/// `line` with its `/query` removed, and the space before it when that space
/// would be left dangling, with the cursor's place in what remains.
fn without_query(line: &str, slash: usize, cursor: usize) -> (String, usize) {
    let before = &line[..slash];
    let after = &line[cursor..];
    let dangling = before.ends_with([' ', '\t'])
        && !before.trim().is_empty()
        && (after.is_empty() || after.starts_with([' ', '\t']));
    let before = if dangling {
        &before[..before.len() - 1]
    } else {
        before
    };
    (format!("{before}{after}"), before.len())
}

/// The buffer offset of the `/` opening the formatting query under
/// `position`, when one is being typed there.
fn slash_query_offset(buffer: &Buffer, position: text::Anchor) -> Option<usize> {
    let cursor = position.to_offset(buffer);
    let row = position.to_point(buffer).row;
    // Fences and front matter depend only on the lines above, so the text up
    // to the end of this line is enough.
    let line_end = Point::new(row, buffer.line_len(row));
    let above: String = buffer.text_for_range(Point::zero()..line_end).collect();
    markdown_syntax::slash_trigger(&above, cursor)
}

/// The formatting menu for the `/query` under `buffer_position` (V49 §5),
/// or `None` when no query is being typed. A query nothing matches gets an
/// empty answer, which closes the menu and leaves the text as typed.
fn slash_response(
    buffer: &Entity<Buffer>,
    buffer_position: text::Anchor,
    cx: &Context<Editor>,
) -> Option<CompletionResponse> {
    let editor = cx.weak_entity();
    let buffer = buffer.read(cx);
    let slash = slash_query_offset(buffer, buffer_position)?;
    let cursor = buffer_position.to_offset(buffer);
    let row = buffer_position.to_point(buffer).row;
    let line_start = buffer.point_to_offset(Point::new(row, 0));
    let line: String = buffer
        .text_for_range(Point::new(row, 0)..Point::new(row, buffer.line_len(row)))
        .collect();
    let query: String = buffer.text_for_range(slash + 1..cursor).collect();
    let now = ReminderClock::global(cx).now();
    let entries = matching_entries(&query);
    // Matching from the cursor hands the menu an empty query, so it doesn't
    // refilter what the aliases already matched.
    let cursor_anchor = buffer.anchor_after(cursor);
    // Group headers only make sense in catalog order.
    let grouped = query.is_empty();
    let completions = entries
        .into_iter()
        .map(|entry| {
            let edit = format_edit(
                &line,
                slash - line_start,
                cursor - line_start,
                entry.format,
                now,
            );
            let start = buffer.anchor_before(line_start + edit.range.start);
            let replace_range = start..buffer.anchor_after(line_start + edit.range.end);
            let reopen = matches!(entry.format, Format::Trigger(_));
            let cursor_in_text = (edit.cursor < edit.text.len()).then_some(edit.cursor);
            let editor = editor.clone();
            Completion {
                replace_range,
                new_text: edit.text,
                // No filter text, so the menu keeps the ranking rather than
                // sorting by label.
                label: CodeLabel::new(entry.label.to_string(), 0..0, Vec::new()),
                documentation: None,
                source: CompletionSource::Custom,
                icon_path: None,
                icon_color: None,
                match_start: Some(cursor_anchor),
                snippet_deduplication_key: None,
                insert_text_mode: None,
                confirm: Some(Arc::new(move |_, window, cx| {
                    record_slash_pick(cx);
                    if let Some(offset) = cursor_in_text {
                        place_cursor(editor.clone(), start, offset, window, cx);
                    }
                    reopen
                })),
                group: grouped.then(|| CompletionGroup {
                    key: entry.group.into(),
                    label: Some(entry.group.into()),
                }),
            }
        })
        .collect();
    Some(CompletionResponse {
        completions,
        display_options: CompletionDisplayOptions {
            dynamic_width: true,
        },
        is_incomplete: true,
    })
}

/// Puts the cursor `offset` bytes past `start` once the pick's edit has
/// landed. The editor is mid-update while a completion confirms, so this
/// waits for the next turn.
fn place_cursor(
    editor: WeakEntity<Editor>,
    start: text::Anchor,
    offset: usize,
    window: &mut Window,
    cx: &mut App,
) {
    window.defer(cx, move |window, cx| {
        editor
            .update(cx, |editor, cx| {
                let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
                    return;
                };
                let position = MultiBufferOffset(start.to_offset(buffer.read(cx)) + offset);
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([position..position])
                });
            })
            .log_err();
    });
}

const TOKEN_OPEN: &str = "(@";

/// The buffer offset where the reminder query starts — just after an open
/// `(@` before the cursor on its line — when that `(@` is in prose and the
/// line is not a calendar line (V45 §4.2).
fn reminder_query_offset(buffer: &Buffer, position: text::Anchor) -> Option<usize> {
    let query_start = query_offset(buffer, position, reminder_query_start)?;
    let row = position.to_point(buffer).row;
    let line_end = Point::new(row, buffer.line_len(row));
    let line: String = buffer
        .text_for_range(Point::new(row, 0)..line_end)
        .collect();
    if line.contains("<!--gcal:") {
        return None;
    }
    // Whether a line is in a fence or front matter depends only on the lines
    // above it, so the text up to the end of this line is enough.
    let above: String = buffer.text_for_range(Point::zero()..line_end).collect();
    markdown_syntax::is_prose_at(&above, query_start - TOKEN_OPEN.len()).then_some(query_start)
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
    async fn no_list_inside_a_fence_opened_above(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "```\nCall ").await;
        type_text(&mut setup, "(@tom");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
    }

    #[gpui::test]
    async fn a_fence_below_the_line_changes_nothing(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Call \n```\nnever closed").await;
        move_cursor(&mut setup, "Call ".len());
        type_text(&mut setup, "(@tom");
        assert!(
            !labels(&mut setup).is_empty(),
            "a fence below the line changes nothing"
        );
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

    fn matching_labels(query: &str) -> Vec<&'static str> {
        matching_entries(query)
            .into_iter()
            .map(|entry| entry.label)
            .collect()
    }

    #[test]
    fn an_empty_query_offers_the_whole_catalog_in_order() {
        let labels = matching_labels("");
        assert_eq!(labels.len(), FORMAT_ENTRIES.len());
        assert_eq!(labels.first(), Some(&"Task"));
        assert_eq!(labels.last(), Some(&"Time now"));
    }

    #[test]
    fn exact_aliases_and_prefixes_rank_first() {
        assert_eq!(matching_labels("h2"), ["Heading 2"]);
        assert_eq!(matching_labels("todo").first(), Some(&"Task"));
        assert_eq!(matching_labels("quo").first(), Some(&"Quote"));
        assert_eq!(matching_labels("b").first(), Some(&"Bold"));
        assert_eq!(matching_labels("I").first(), Some(&"Italic"));
        assert_eq!(matching_labels("block").first(), Some(&"Quote"));
        assert_eq!(matching_labels("wiki"), ["Link to note"]);
        assert!(matching_labels("zzz").is_empty());
    }

    fn noon() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 28)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(14, 5, 0).unwrap())
    }

    /// Applies the pick of `format` to `line`, whose `/query` ends the
    /// line, returning the line with `‸` where the cursor lands.
    fn pick(line: &str, format: Format) -> String {
        let slash = line.rfind('/').unwrap();
        let edit = format_edit(line, slash, line.len(), format, noon());
        let mut result = line.to_string();
        let cursor = edit.range.start + edit.cursor;
        result.replace_range(edit.range, &edit.text);
        result.insert(cursor, '‸');
        result
    }

    #[test]
    fn a_block_pick_rewrites_the_prefix() {
        assert_eq!(
            pick("- buy milk /h2", Format::Block(Block::Heading(2))),
            "## buy milk‸"
        );
        assert_eq!(
            pick("## Notes /quote", Format::Block(Block::Quote)),
            "> Notes‸"
        );
        assert_eq!(pick("/todo", Format::Block(Block::Task)), "- [ ] ‸");
        assert_eq!(pick("  /list", Format::Block(Block::Bullet)), "  - ‸");
        assert_eq!(
            pick("- [x] done /ol", Format::Block(Block::Numbered)),
            "1. done‸"
        );
    }

    #[test]
    fn a_block_pick_mid_line_keeps_the_text_around_it() {
        let line = "- buy /h1 milk";
        let edit = format_edit(line, 6, 9, Format::Block(Block::Heading(1)), noon());
        assert_eq!(edit.range, 0..line.len());
        assert_eq!(edit.text, "# buy milk");
        assert_eq!(edit.cursor, "# buy".len());
    }

    #[test]
    fn block_inserts_take_an_empty_line_or_go_below() {
        assert_eq!(pick("/hr", Format::Divider), "___‸");
        assert_eq!(pick("/fence", Format::CodeBlock), "```\n‸\n```");
        assert_eq!(
            pick("/table", Format::Table),
            "| ‸ |  |\n| --- | --- |\n|  |  |"
        );
        assert_eq!(pick("notes /hr", Format::Divider), "notes\n___‸");
        assert_eq!(
            pick("  code /fence", Format::CodeBlock),
            "  code\n  ```\n  ‸\n  ```"
        );
    }

    #[test]
    fn inline_picks_insert_at_the_slash() {
        assert_eq!(pick("Call /b", Format::Wrap("**")), "Call **‸**");
        assert_eq!(pick("Call /i", Format::Wrap("_")), "Call _‸_");
        assert_eq!(pick("Call /s", Format::Wrap("~~")), "Call ~~‸~~");
        assert_eq!(pick("Call /code", Format::Wrap("`")), "Call `‸`");
        assert_eq!(pick("Call /url", Format::Link), "Call [‸]()");
        assert_eq!(pick("see /wiki", Format::Trigger("[[")), "see [[‸");
        assert_eq!(pick("On /date", Format::Date), "On 2026-10-28‸");
        assert_eq!(pick("At /time", Format::Time), "At 14:05‸");
    }

    #[gpui::test]
    async fn slash_h2_and_enter_rewrites_the_line(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "- buy milk ").await;
        type_text(&mut setup, "/h2");
        assert_eq!(labels(&mut setup), ["Heading 2"]);
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), "## buy milk");
        type_text(&mut setup, "!");
        assert_eq!(text(&mut setup), "## buy milk!");
    }

    #[gpui::test]
    async fn a_slash_offers_the_catalog_with_its_groups(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "").await;
        type_text(&mut setup, "/");
        let offered = labels(&mut setup);
        assert_eq!(offered.len(), FORMAT_ENTRIES.len());
        assert_eq!(offered.first().map(String::as_str), Some("Task"));
    }

    #[gpui::test]
    async fn a_space_or_a_slash_in_prose_leaves_no_menu(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Call ").await;
        type_text(&mut setup, "/");
        assert!(!labels(&mut setup).is_empty());
        type_text(&mut setup, " ");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
        type_text(&mut setup, "and/or");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
        assert_eq!(text(&mut setup), "Call / and/or");
    }

    #[gpui::test]
    async fn a_query_nothing_matches_closes_the_menu(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "").await;
        type_text(&mut setup, "/usr");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), "/usr\n");
    }

    #[gpui::test]
    async fn escape_keeps_the_slash(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Call ").await;
        type_text(&mut setup, "/");
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert_eq!(labels(&mut setup), Vec::<String>::new());
        assert_eq!(text(&mut setup), "Call /");
    }

    #[gpui::test]
    async fn an_inline_pick_puts_the_cursor_inside_the_pair(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Call ").await;
        type_text(&mut setup, "/bold");
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        type_text(&mut setup, "now");
        assert_eq!(text(&mut setup), "Call **now**");
    }

    #[gpui::test]
    async fn a_code_block_pick_puts_the_cursor_inside_the_fence(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "").await;
        type_text(&mut setup, "/fence");
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        type_text(&mut setup, "x");
        assert_eq!(text(&mut setup), "```\nx\n```");
    }

    #[gpui::test]
    async fn link_to_note_opens_the_wikilink_completion(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "see ").await;
        let (fs, root) = setup.workspace.read_with(&setup.cx, |workspace, cx| {
            let project = workspace.project().read(cx);
            let root = project.worktrees(cx).next().unwrap().read(cx).abs_path();
            (project.fs().clone(), root)
        });
        fs.create_file(&root.join("projects/garden.md"), Default::default())
            .await
            .unwrap();
        setup.cx.run_until_parked();
        type_text(&mut setup, "/wiki");
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), "see [[");
        assert_eq!(labels(&mut setup), ["projects/garden"]);
    }

    #[gpui::test]
    async fn nothing_opens_in_code(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "```\n").await;
        type_text(&mut setup, "/");
        assert_eq!(labels(&mut setup), Vec::<String>::new());
    }

    #[gpui::test]
    async fn insert_formatting_types_the_slash_and_opens_the_menu(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "Call").await;
        setup.cx.dispatch_action(InsertFormatting);
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), "Call /");
        assert!(!labels(&mut setup).is_empty());
    }

    #[gpui::test]
    async fn a_pick_counts_toward_retiring_the_hint(cx: &mut TestAppContext) {
        let mut setup = setup(cx, "projects/house.md", "").await;
        setup
            .cx
            .update(|_, cx| cx.set_global(db::AppDatabase::test_new()));
        setup
            .cx
            .update(|_, cx| crate::markdown_conceal::load_slash_hint(cx));
        setup.cx.run_until_parked();
        type_text(&mut setup, "/h1");
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        let counts = setup
            .cx
            .update(|_, cx| crate::markdown_conceal::slash_hint_counts(cx));
        assert_eq!(counts.map(|counts| counts.picks), Some(1));
    }
}
