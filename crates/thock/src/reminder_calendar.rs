//! The reminder date picker (V45 §8.3): a calendar popover at the cursor of
//! a vault note that writes the line's reminder token. It is opened by
//! `thock::PickReminderDate`, from the palette, a binding, or the suggestion
//! list's *Pick a date…*, and unlike that list it takes focus.

use crate::reminders::{ReminderClock, ReminderSettings};
use crate::ui::calendar::{Calendar, add_days, add_months};
use crate::vault::{Vault, VaultStatus};
use chrono::{NaiveDate, NaiveTime, Timelike as _};
use editor::actions::ShowCompletions;
use editor::{Anchor, Editor, EditorEvent, EditorMode, MultiBufferSnapshot, ToPoint as _};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyContext,
    KeyDownEvent, MouseButton, Pixels, Point, Render, SharedString, WeakEntity, Window, actions,
    anchored, div, point, prelude::*, px,
};
use menu::{Cancel, Confirm, SelectNext, SelectPrevious};
use multi_buffer::MultiBufferRow;
use std::ops::Range;
use std::path::PathBuf;
use text::Point as BufferPoint;
use thock_sync_core::{Due, When, parse_line, place_token, token_text};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::ModalView;

actions!(
    thock,
    [
        /// Picks the date and time of this line's reminder from a calendar.
        PickReminderDate,
        /// Moves the calendar back one day.
        SelectPreviousCalendarDay,
        /// Moves the calendar forward one day.
        SelectNextCalendarDay,
        /// Moves the calendar back one month.
        SelectPreviousCalendarMonth,
        /// Moves the calendar forward one month.
        SelectNextCalendarMonth,
        /// Clears the reminder's time, so it is for the whole day.
        ClearReminderTime
    ]
);

const KEY_CONTEXT: &str = "ThockReminderCalendar";

pub fn init(cx: &mut App) {
    cx.observe_new(|editor: &mut Editor, _, cx| register(editor, cx))
        .detach();
}

/// Offers the picker on full, writable editors over a vault's Markdown
/// file, the same gate as the suggestion list.
fn register(editor: &mut Editor, cx: &mut Context<Editor>) {
    if !editor.mode().is_full()
        || matches!(editor.mode(), EditorMode::Minimap { .. })
        || editor.read_only(cx)
    {
        return;
    }
    let mut installed = install(editor, cx);
    if installed {
        return;
    }
    // An untitled buffer may later be saved as a note into a vault.
    cx.subscribe(&cx.entity(), move |editor, _, event: &EditorEvent, cx| {
        if matches!(event, EditorEvent::FileHandleChanged) && !installed {
            installed = install(editor, cx);
        }
    })
    .detach();
}

fn install(editor: &mut Editor, cx: &mut Context<Editor>) -> bool {
    let Some((_, vault_root)) = note_file(editor, cx) else {
        return false;
    };
    let VaultStatus::Valid(vault) = Vault::detect(&vault_root) else {
        return false;
    };
    let editor_handle = cx.weak_entity();
    editor
        .register_action::<PickReminderDate>(move |_, window, cx| {
            if let Some(editor) = editor_handle.upgrade() {
                pick_reminder_date(&editor, &vault, window, cx);
            }
        })
        .detach();
    true
}

/// The editor's Markdown file and the root of its worktree.
fn note_file(editor: &Editor, cx: &App) -> Option<(PathBuf, PathBuf)> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    if file.path().extension() != Some("md") {
        return None;
    }
    let root = project::File::from_dyn(Some(file))?
        .worktree
        .read(cx)
        .abs_path()
        .to_path_buf();
    Some((file.as_local()?.abs_path(cx), root))
}

/// Where the picked reminder goes, captured when the picker opens.
#[derive(Clone)]
struct Target {
    editor: WeakEntity<Editor>,
    cursor: Anchor,
    /// The day of the daily note being edited, which lets a token on that
    /// day drop its date (§4.1).
    daily_day: Option<NaiveDate>,
    /// Whether the cursor sits in a `(@…` being typed, so `esc` goes back
    /// to the suggestion list.
    from_suggestions: bool,
}

/// What the picker starts on, read from the cursor's line.
struct Opening {
    target: Target,
    date: Option<NaiveDate>,
    time: Option<NaiveTime>,
    position: Option<Point<Pixels>>,
}

fn pick_reminder_date(editor: &Entity<Editor>, vault: &Vault, window: &mut Window, cx: &mut App) {
    let Some(workspace) = editor.read(cx).workspace() else {
        return;
    };
    let daily_day = note_file(editor.read(cx), cx).and_then(|(path, _)| note_day(vault, &path));
    let Some(opening) = editor.update(cx, |editor, cx| read_opening(editor, daily_day, window, cx))
    else {
        return;
    };
    let today = ReminderClock::global(cx).today();
    workspace.update(cx, |workspace, cx| {
        workspace.toggle_modal(window, cx, |_, cx| {
            ReminderCalendar::new(opening, today, cx)
        });
    });
}

/// The day of the daily note at `path`, the token's home day (§4.1).
fn note_day(vault: &Vault, path: &std::path::Path) -> Option<NaiveDate> {
    let relative = path
        .strip_prefix(&vault.root)
        .ok()?
        .to_str()?
        .replace('\\', "/");
    ReminderSettings::default()
        .reminder_config(vault)
        .daily
        .day_of(&relative)
}

fn read_opening(
    editor: &mut Editor,
    daily_day: Option<NaiveDate>,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<Opening> {
    let cursor = editor.selections.newest_anchor().head();
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let (line, column) = cursor_line(&snapshot, cursor);
    // A calendar line belongs to calendar sync (§4.2).
    if line.contains("<!--gcal:") {
        return None;
    }
    let reminder = parse_line(&line, daily_day);
    let date = reminder
        .as_ref()
        .and_then(|reminder| reminder.due)
        .map(|due| due.date);
    let time = reminder
        .and_then(|reminder| reminder.token)
        .and_then(|token| match token.when {
            When::DateTime(_, time) | When::Time(time) => Some(time),
            When::Date(_) => None,
        });
    Some(Opening {
        target: Target {
            editor: cx.weak_entity(),
            cursor,
            daily_day,
            from_suggestions: open_fragment(&line, column).is_some(),
        },
        date,
        time,
        position: cursor_position(editor, window, cx),
    })
}

/// The cursor's line and its byte column in that line.
fn cursor_line(snapshot: &MultiBufferSnapshot, cursor: Anchor) -> (String, usize) {
    let point = cursor.to_point(snapshot);
    let row = MultiBufferRow(point.row);
    let line = snapshot
        .text_for_range(
            BufferPoint::new(point.row, 0)..BufferPoint::new(point.row, snapshot.line_len(row)),
        )
        .collect();
    (line, point.column as usize)
}

/// The window position just under the cursor, where the popover opens.
fn cursor_position(
    editor: &mut Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<Point<Pixels>> {
    let bounds = *editor.last_bounds()?;
    let snapshot = editor.snapshot(window, cx);
    let head = editor
        .selections
        .newest_display(&snapshot.display_snapshot)
        .head();
    let style = editor.style(cx).clone();
    let rem_size = window.rem_size();
    let font_id = window.text_system().resolve_font(&style.text.font());
    let font_size = style.text.font_size.to_pixels(rem_size);
    let line_height = style.text.line_height_in_pixels(rem_size);
    let gutter = snapshot
        .gutter_dimensions(font_id, font_size, &style, window, cx)
        .full_width();
    let caret = editor.display_to_pixel_point(head, &snapshot, window, cx)?;
    Some(bounds.origin + point(gutter + caret.x, caret.y + line_height))
}

/// Writes the picked reminder on the target's line. Every token the picker
/// writes goes through here.
fn write_token(
    editor: &mut Editor,
    target: &Target,
    due: Due,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let (line, column) = cursor_line(&snapshot, target.cursor);
    let row = target.cursor.to_point(&snapshot).row;
    let Some((edited, after)) = edited_line(&line, column, due, target.daily_day) else {
        return;
    };
    let (range, text) = changed_span(&line, &edited);
    let start = BufferPoint::new(row, range.start as u32);
    let end = BufferPoint::new(row, range.end as u32);
    let after = BufferPoint::new(row, after as u32);
    editor.transact(window, cx, |editor, window, cx| {
        editor.edit([(start..end, text)], cx);
        editor.change_selections(Default::default(), window, cx, |selections| {
            selections.select_ranges([after..after])
        });
    });
}

/// `line` with the reminder on it, and the column just past its token. A
/// `(@…` being typed around the cursor becomes the token; otherwise the sync
/// core rewrites the line's token or writes one before its trailing comments.
/// `None` on a calendar line.
fn edited_line(
    line: &str,
    cursor: usize,
    due: Due,
    home_day: Option<NaiveDate>,
) -> Option<(String, usize)> {
    if let Some(range) = open_fragment(line, cursor) {
        let token = token_text(due, home_day);
        let after = range.start + token.len();
        let mut edited = line.to_string();
        edited.replace_range(range, &token);
        return Some((edited, after));
    }
    let edited = place_token(line, due, home_day)?;
    let after = parse_line(&edited, home_day)
        .and_then(|reminder| reminder.token)
        .map_or(edited.len(), |token| token.range.end);
    Some((edited, after))
}

/// The bytes of `old` that `new` replaces, and what replaces them, so the
/// edit leaves the rest of the line (and anchors in it) alone.
fn changed_span<'a>(old: &str, new: &'a str) -> (Range<usize>, &'a str) {
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, old_char), new_char)| old_char != new_char)
        .map_or(old.len().min(new.len()), |((index, _), _)| index);
    let suffix = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(old_char, new_char)| old_char == new_char)
        .map(|(old_char, _)| old_char.len_utf8())
        .sum::<usize>();
    (prefix..old.len() - suffix, &new[prefix..new.len() - suffix])
}

/// The `(@…` the cursor is in, from `(@` to its closing parenthesis (the one
/// Markdown autocloses), when no `)` comes between `(@` and the cursor.
fn open_fragment(line: &str, cursor: usize) -> Option<Range<usize>> {
    let before = line.get(..cursor)?;
    let start = before.rfind("(@")?;
    if before[start..].contains(')') {
        return None;
    }
    let end = line[cursor..]
        .find(')')
        .map_or(cursor, |close| cursor + close + 1);
    Some(start..end)
}

/// The digits typed for the time: `9` is 09:00, `930` is 09:30 and `1430`
/// is 14:30. Digits that could not become a time are refused.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TimeEntry {
    digits: String,
}

impl TimeEntry {
    fn from_time(time: Option<NaiveTime>) -> Self {
        Self {
            digits: time
                .map(|time| format!("{:02}{:02}", time.hour(), time.minute()))
                .unwrap_or_default(),
        }
    }

    fn push(&mut self, digit: char) -> bool {
        let mut digits = self.digits.clone();
        digits.push(digit);
        if !could_be_time(&digits) {
            return false;
        }
        self.digits = digits;
        true
    }

    fn clear(&mut self) {
        self.digits.clear();
    }

    /// `Ok(None)` when no time was typed, `Err` while the digits are not yet
    /// a time (`24` waits for a third digit).
    fn time(&self) -> Result<Option<NaiveTime>, ()> {
        if self.digits.is_empty() {
            return Ok(None);
        }
        time_from_digits(&self.digits).map(Some).ok_or(())
    }
}

fn time_from_digits(digits: &str) -> Option<NaiveTime> {
    if !digits.chars().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let (hour, minute) = match digits.len() {
        1 | 2 => (digits, "0"),
        3 => digits.split_at(1),
        4 => digits.split_at(2),
        _ => return None,
    };
    NaiveTime::from_hms_opt(hour.parse().ok()?, minute.parse().ok()?, 0)
}

fn could_be_time(digits: &str) -> bool {
    time_from_digits(digits).is_some()
        || (digits.len() < 4 && ('0'..='9').any(|next| could_be_time(&format!("{digits}{next}"))))
}

/// The calendar popover. It lives in the workspace's modal layer, drawn bare
/// so it can sit at the cursor rather than at the top of the window.
pub struct ReminderCalendar {
    focus_handle: FocusHandle,
    target: Target,
    selected: NaiveDate,
    today: NaiveDate,
    time: TimeEntry,
    position: Option<Point<Pixels>>,
}

impl ReminderCalendar {
    fn new(opening: Opening, today: NaiveDate, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            selected: opening.date.unwrap_or(today),
            time: TimeEntry::from_time(opening.time),
            target: opening.target,
            today,
            position: opening.position,
        }
    }

    fn move_to(&mut self, date: NaiveDate, cx: &mut Context<Self>) {
        self.selected = date;
        cx.notify();
    }

    fn select_previous_day(
        &mut self,
        _: &SelectPreviousCalendarDay,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(add_days(self.selected, -1), cx);
    }

    fn select_next_day(
        &mut self,
        _: &SelectNextCalendarDay,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(add_days(self.selected, 1), cx);
    }

    fn select_previous_week(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(add_days(self.selected, -7), cx);
    }

    fn select_next_week(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(add_days(self.selected, 7), cx);
    }

    fn select_previous_month(
        &mut self,
        _: &SelectPreviousCalendarMonth,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(add_months(self.selected, -1), cx);
    }

    fn select_next_month(
        &mut self,
        _: &SelectNextCalendarMonth,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(add_months(self.selected, 1), cx);
    }

    fn clear_time(&mut self, _: &ClearReminderTime, _: &mut Window, cx: &mut Context<Self>) {
        self.time.clear();
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let mut chars = keystroke.key.chars();
        let (Some(digit), None) = (chars.next(), chars.next()) else {
            return;
        };
        if digit.is_ascii_digit() {
            self.time.push(digit);
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Closes the popover and gives focus back to the note. Drawn bare, the
    /// popover is outside the modal layer's focus, so the layer can't.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.target.editor.upgrade() {
            editor.focus_handle(cx).focus(window, cx);
        }
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        // Digits that are not yet a time keep the picker open.
        let Ok(time) = self.time.time() else {
            return;
        };
        let due = Due::new(self.selected, time);
        let target = self.target.clone();
        self.dismiss(window, cx);
        target
            .editor
            .update(cx, |editor, cx| {
                write_token(editor, &target, due, window, cx)
            })
            .log_err();
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
        if self.target.from_suggestions {
            let editor = self.target.editor.clone();
            window.defer(cx, move |window, cx| {
                editor
                    .update(cx, |editor, cx| {
                        editor.show_completions(&ShowCompletions, window, cx)
                    })
                    .log_err();
            });
        }
    }

    fn time_label(&self) -> SharedString {
        match self.time.time() {
            Ok(Some(time)) => time.format("%H:%M").to_string().into(),
            Ok(None) => "All day".into(),
            Err(()) => format!("{}…", self.time.digits).into(),
        }
    }
}

impl EventEmitter<DismissEvent> for ReminderCalendar {}

impl Focusable for ReminderCalendar {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for ReminderCalendar {
    fn render_bare(&self) -> bool {
        true
    }
}

impl Render for ReminderCalendar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add(KEY_CONTEXT);
        key_context.add("menu");
        let position = self
            .position
            .unwrap_or_else(|| point(window.viewport_size().width / 2. - px(130.), px(80.)));
        let entity = cx.entity().downgrade();
        let calendar = Calendar::new("thock-reminder-calendar", self.selected, self.today)
            .on_select(move |date, _, cx| {
                entity
                    .update(cx, |this, cx| this.move_to(date, cx))
                    .log_err();
            });
        let card = v_flex()
            .id("thock-reminder-calendar-card")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_previous_day))
            .on_action(cx.listener(Self::select_next_day))
            .on_action(cx.listener(Self::select_previous_week))
            .on_action(cx.listener(Self::select_next_week))
            .on_action(cx.listener(Self::select_previous_month))
            .on_action(cx.listener(Self::select_next_month))
            .on_action(cx.listener(Self::clear_time))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_key_down(cx.listener(Self::key_down))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .elevation_3(cx)
            .p_2()
            .gap_2()
            .child(calendar)
            .child(
                h_flex()
                    .justify_between()
                    .px_1()
                    .child(
                        Label::new("Time")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(self.time_label()).size(LabelSize::Small)),
            )
            .child(
                Label::new("Type a time · ⌫ clears · ⏎ sets")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            );
        div()
            .absolute()
            .inset_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
            )
            .child(
                anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use project::Project;
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use workspace::{MultiWorkspace, Workspace};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    #[test]
    fn the_edit_takes_a_typed_query_then_the_lines_token_then_the_end() {
        let due = Due::new(date(2026, 12, 2), None);
        let line = "Call (@dec) <!-- x -->";
        assert_eq!(
            edited_line(line, "Call (@dec".len(), due, None),
            Some(("Call (@2026-12-02) <!-- x -->".to_string(), 18))
        );
        let line = "Renew (@2026-11-01 10:00) soon";
        assert_eq!(
            edited_line(line, 0, due, None),
            Some(("Renew (@2026-12-02) soon".to_string(), 19))
        );
        let line = "Call the plumber <!--rem:3f9a1c0d4e21-->";
        assert_eq!(
            edited_line(line, 4, due, None),
            Some((
                "Call the plumber (@2026-12-02) <!--rem:3f9a1c0d4e21-->".to_string(),
                30
            ))
        );
        assert_eq!(edited_line("Standup <!--gcal:abc-->", 0, due, None), None);
    }

    #[test]
    fn the_changed_span_is_only_what_differs() {
        assert_eq!(changed_span("Call it", "Call it (@x)"), (7..7, " (@x)"));
        assert_eq!(
            changed_span("a (@2026-11-01) b", "a (@2026-12-02) b"),
            (10..14, "2-02")
        );
        assert_eq!(changed_span("same", "same"), (4..4, ""));
        assert_eq!(changed_span("é (@x)", "é (@y)"), (5..6, "y"));
    }

    #[test]
    fn typed_digits_become_a_time() {
        let mut entry = TimeEntry::default();
        assert_eq!(entry.time(), Ok(None));
        entry.push('9');
        assert_eq!(entry.time(), Ok(Some(time(9, 0))));
        entry.push('3');
        entry.push('0');
        assert_eq!(entry.time(), Ok(Some(time(9, 30))));

        let mut entry = TimeEntry::default();
        for digit in "1430".chars() {
            assert!(entry.push(digit));
        }
        assert_eq!(entry.time(), Ok(Some(time(14, 30))));
        assert!(!entry.push('5'), "a fifth digit is refused");

        let mut entry = TimeEntry::default();
        entry.push('2');
        entry.push('4');
        assert_eq!(entry.time(), Err(()), "24 waits for 2:4x");
        assert!(entry.push('5'));
        assert_eq!(entry.time(), Ok(Some(time(2, 45))));

        let mut entry = TimeEntry::default();
        entry.push('9');
        assert!(!entry.push('9'), "99 can never be a time");
        entry.clear();
        assert_eq!(entry.time(), Ok(None));
        assert_eq!(TimeEntry::from_time(Some(time(9, 5))).digits, "0905");
    }

    fn keymap_bindings(content: &str, cx: &App) -> Vec<KeyBinding> {
        match KeymapFile::load(content, cx) {
            KeymapFileLoadResult::Success { key_bindings }
            | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
            KeymapFileLoadResult::JsonParseFailure { error } => panic!("bad keymap: {error}"),
        }
    }

    fn init_test(cx: &mut TestAppContext, vim: bool) {
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
            if vim {
                cx.bind_keys(keymap_bindings(
                    include_str!("../../../assets/keymaps/vim.json"),
                    cx,
                ));
            }
            init(cx);
            crate::reminders::TestClock::install(today().and_time(time(10, 0)), cx);
        });
    }

    struct Setup {
        workspace: Entity<Workspace>,
        editor: Entity<Editor>,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    /// A vault whose note `open` holds `text`, open in a workspace with the
    /// cursor at `cursor` (row, column).
    async fn setup(
        cx: &mut TestAppContext,
        vim: bool,
        open: &str,
        text: &str,
        cursor: (u32, u32),
    ) -> Setup {
        init_test(cx, vim);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        let fs = FakeFs::new(cx.executor());
        let mut tree = json!({ ".thock": { "config.toml": "schema = 1\n" } });
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
            let point = BufferPoint::new(cursor.0, cursor.1);
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([point..point])
            });
        });
        cx.run_until_parked();
        Setup {
            workspace,
            editor,
            cx,
            _vault_dir: vault_dir,
        }
    }

    fn calendar(setup: &mut Setup) -> Option<Entity<ReminderCalendar>> {
        setup
            .workspace
            .read_with(&setup.cx, |workspace, cx| workspace.active_modal(cx))
    }

    fn selected(setup: &mut Setup) -> NaiveDate {
        let calendar = calendar(setup).expect("the calendar is open");
        calendar.read_with(&setup.cx, |calendar, _| calendar.selected)
    }

    fn text(setup: &mut Setup) -> String {
        setup
            .editor
            .read_with(&setup.cx, |editor, cx| editor.text(cx))
    }

    fn calendar_focused(setup: &mut Setup) -> bool {
        let calendar = calendar(setup).expect("the calendar is open");
        setup
            .cx
            .update(|window, cx| calendar.read(cx).focus_handle.is_focused(window))
    }

    fn editor_focused(setup: &mut Setup) -> bool {
        let editor = setup.editor.clone();
        setup
            .cx
            .update(|window, cx| editor.focus_handle(cx).is_focused(window))
    }

    fn today() -> NaiveDate {
        date(2026, 10, 21)
    }

    const PICK: &str = "ctrl-alt-shift-d";

    #[gpui::test]
    async fn moving_and_typing_a_time_writes_the_token(cx: &mut TestAppContext) {
        let line = "- [ ] Call the plumber";
        let mut setup = setup(cx, false, "projects/house.md", line, (0, line.len() as u32)).await;
        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        assert!(calendar_focused(&mut setup), "the calendar takes focus");
        assert_eq!(selected(&mut setup), today());

        setup.cx.simulate_keystrokes("right right left down");
        assert_eq!(selected(&mut setup), add_days(today(), 8));
        setup.cx.simulate_keystrokes("up ] [ ]");
        let expected = add_months(add_days(today(), 1), 1);
        assert_eq!(selected(&mut setup), expected);

        setup.cx.simulate_keystrokes("1 4 3 0 enter");
        setup.cx.run_until_parked();
        assert!(calendar(&mut setup).is_none(), "enter closes the calendar");
        assert_eq!(
            text(&mut setup),
            format!("{line} (@{} 14:30)", expected.format("%Y-%m-%d"))
        );
        assert!(editor_focused(&mut setup));
    }

    #[gpui::test]
    async fn the_calendar_starts_on_the_lines_token(cx: &mut TestAppContext) {
        let line = "- [ ] Renew passport (@2026-12-02 10:20) <!--rem:3f9a1c0d4e21-->";
        let mut setup = setup(cx, false, "projects/house.md", line, (0, 0)).await;
        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        assert_eq!(selected(&mut setup), date(2026, 12, 2));
        let digits = calendar(&mut setup)
            .unwrap()
            .read_with(&setup.cx, |calendar, _| calendar.time.digits.clone());
        assert_eq!(digits, "1020");

        setup.cx.simulate_keystrokes("backspace ] enter");
        setup.cx.run_until_parked();
        assert_eq!(
            text(&mut setup),
            "- [ ] Renew passport (@2027-01-02) <!--rem:3f9a1c0d4e21-->",
            "the token is replaced, its time cleared, the id kept"
        );
    }

    #[gpui::test]
    async fn a_time_in_todays_daily_note_drops_the_date(cx: &mut TestAppContext) {
        let note = format!("daily/{}.md", today().format("%Y-%m-%d"));
        let line = "- [ ] Water the plants";
        let mut setup = setup(cx, false, &note, line, (0, line.len() as u32)).await;
        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("1 0 enter");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), format!("{line} (@10:00)"));
    }

    #[gpui::test]
    async fn escape_returns_focus_to_the_note(cx: &mut TestAppContext) {
        let line = "- [ ] Call the plumber";
        let mut setup = setup(cx, false, "projects/house.md", line, (0, 3)).await;
        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("right 9");
        assert!(calendar_focused(&mut setup));
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(calendar(&mut setup).is_none());
        assert!(editor_focused(&mut setup));
        assert_eq!(text(&mut setup), line, "nothing is written");
    }

    #[gpui::test]
    async fn picking_from_a_typed_query_replaces_it(cx: &mut TestAppContext) {
        let line = "Call mum (@sat)";
        let mut setup = setup(cx, false, "projects/house.md", line, (0, 14)).await;
        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert_eq!(text(&mut setup), line, "escape leaves the query as typed");
        assert!(editor_focused(&mut setup));

        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(
            text(&mut setup),
            format!("Call mum (@{})", today().format("%Y-%m-%d"))
        );
    }

    #[gpui::test]
    async fn vim_motions_move_the_day_and_week(cx: &mut TestAppContext) {
        let line = "Dentist (@2026-10-21)";
        let mut setup = setup(cx, true, "projects/house.md", line, (0, 0)).await;
        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("l l h j");
        assert_eq!(selected(&mut setup), date(2026, 10, 29));
        setup.cx.simulate_keystrokes("k k");
        assert_eq!(selected(&mut setup), date(2026, 10, 15));
    }

    #[gpui::test]
    async fn the_calendar_does_not_open_on_a_calendar_line(cx: &mut TestAppContext) {
        let line = "- 10:00 - 11:00 Standup <!--gcal:abc-->";
        let mut setup = setup(cx, false, "projects/house.md", line, (0, 0)).await;
        setup.cx.simulate_keystrokes(PICK);
        setup.cx.run_until_parked();
        assert!(calendar(&mut setup).is_none());
    }
}
