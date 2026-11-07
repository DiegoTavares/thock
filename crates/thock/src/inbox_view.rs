//! The Inbox view (spec `v41-inbox-desk-view.md`): a centre-pane tab with the
//! waiting items on the left, the selected note rendered on the right, and one
//! key per decision, drawn as a bar of buttons above the note. A decision is V40 §5's three writes —
//! the task line, the move into `archives/inbox/`, the triage-log line — so a
//! desk keystroke and a phone swipe leave the same traces, and `u` reverses the
//! last one. Suggested destinations come from the service; here a suggestion
//! only ever decides what `space` will do.

use anyhow::{Context as _, Result};
use chrono::{Local, Utc};
use editor::{Editor, EditorEvent};
use fs::Fs;
use gpui::{
    Action, Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, ImageSource, KeyContext, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Point, Rems, Resource, ScrollHandle, ScrollWheelEvent,
    SharedString, Subscription, Task, TaskExt as _, UniformListScrollHandle, WeakEntity, Window,
    actions, anchored, deferred, div, hsla, point, prelude::*, px, relative, rems,
    transparent_black,
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use menu::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::Project;
use serde::{Deserialize, Serialize};
use settings::SettingsStore;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use thock_sync_core::{Operation, Write, apply, archive_path, inbox_task_line, triage_log_line};
use ui::{
    ActiveTheme as _, Button, ButtonCommon as _, ButtonLike, ButtonSize, ButtonStyle,
    Clickable as _, Color, ContextMenu, FixedWidth as _, HeadlineSize, Icon, IconButton, IconName,
    IconSize, KeyBinding, Label, LabelCommon as _, LabelSize, StyledTypography as _, TextSize,
    Tooltip, h_flex, v_flex,
};
use util::ResultExt as _;
use util::paths::PathStyle;
use util::rel_path::RelPath;
#[cfg(test)]
use workspace::MultiWorkspace;
use workspace::item::{Item, ItemEvent, WeakItemHandle};
use workspace::notifications::NotificationId;
use workspace::{OpenOptions, OpenVisible, Toast, Workspace};

use crate::backlog::{self, DEFAULT_BACKLOG, SectionKind};
use crate::inbox::{InboxItem, TRIAGE_LOG_PATH, sanitize_title};
use crate::inbox_email::{
    EmailBlock, EmailField, EmailMessage, EmailThread, FoldKind, drop_restated_subject, full_title,
    initials, parse_thread, quiet_system_lines, snippet, split_fields, ticket_prefix,
};
use crate::inbox_service::{
    self, InboxEntry, InboxEvent, InboxService, MoveOutcome, SuggestionAvailability, TriageInbox,
    move_note,
};
use crate::inbox_suggest::{DecisionRecord, Destination, Tier, format_cost, tier};
use crate::markdown_conceal::vault_image_path;
use crate::notes::{EnsureNoteOutcome, NoteKind, ensure_note};
use crate::ui::sectioned_list::{SectionedListDelegate, sectioned_list};
use crate::ui::split::Split;
use crate::vault::{VAULT_MARKER_DIR, Vault};

pub const INBOX_VIEW_KEY: &str = "ThockInboxView";
const LAYOUT_PATH: &str = "state/inbox/layout.json";
const DEFAULT_LIST_FRACTION: f32 = 0.32;
const READER_LINE: Pixels = px(40.);
// The spacing and type scale of §4.2.
const PANE_HEADER_HEIGHT: Pixels = px(52.);
/// A row's slot: a two-line title and the meta line, with `ROW_GAP` above and
/// below the band, so a row holds together and the next one starts apart.
const ROW_HEIGHT: Pixels = px(76.);
const ROW_GAP: Pixels = px(2.);
const FOOTER_HEIGHT: Pixels = px(36.);
const BAR_BUTTON_HEIGHT: Pixels = px(60.);
const READER_MAX_WIDTH: Pixels = px(960.);
/// Holds `space or shift-s`, the widest keycaps a button carries.
const DESTINATION_MIN_WIDTH: Pixels = px(136.);
const BAR_GAP: Pixels = px(8.);
const FIELD_MIN_WIDTH: Pixels = px(300.);
const FIELD_KEY_WIDTH: Pixels = px(104.);
const SENDER_BADGE_SIZE: Pixels = px(28.);
/// Holds `13d`, so a suggestion beside it doesn't drift between rows.
const AGE_COLUMN_WIDTH: Pixels = px(32.);
const ROW_LINE_HEIGHT: f32 = 1.25;
const MARK_BADGE_SIZE: Rems = rems(0.75);
/// Hangs in the row's left padding, so a mark moves no text.
const MARK_BADGE_INSET: Rems = rems(-1.);
const FOCUS_RAIL_WIDTH: Pixels = px(3.);
const TITLE_SIZE: HeadlineSize = HeadlineSize::Large;
const TITLE_LINE_HEIGHT: f32 = 1.3;
/// Kept whatever it says, so the caption and the marks don't move the bar.
const CAPTION_SLOT_HEIGHT: Pixels = px(18.);
const KEYCAP_HEIGHT: Rems = rems(1.125);
const KEYCAP_TEXT_SIZE: Rems = rems(0.75);
const FILTER_BOX_HEIGHT: Pixels = px(30.);
/// A quiet button's own padding, given back so its words end on the column.
const ACTION_PADDING: Pixels = px(4.);
/// Reader widths, in rems, where the header's sender stops fitting: under the
/// first it needs the room of the glyph and the link's long name, past the
/// second there's room for an address.
const HEADER_TIGHT_WIDTH: f32 = 35.5;
const HEADER_ROOMY_WIDTH: f32 = 44.;
/// Holds `shift-n`, so the sheet's words start together close to their keys.
const KEYS_COLUMN_WIDTH: Rems = rems(3.75);
/// Below this a group of the keys sheet wraps under the one before it.
const KEYS_GROUP_MIN_WIDTH: Rems = rems(13.5);
/// A list longer than this draws its first items and a count of the rest
/// until the folds open.
const MAX_FIELD_LIST_ITEMS: usize = 3;
/// Past this a field's old value is more to read than it tells.
const MAX_PREVIOUS_VALUE_LENGTH: usize = 32;
const EARLIER_CUE_HEIGHT: Pixels = px(28.);
/// Where a revealed card's top sits in the reader: one gap under the earlier
/// cue, so the card before ends behind it.
const REVEAL_CLEARANCE: Pixels = px(36.);
/// A card's header, which a landing keeps on screen.
const CARD_HEADER_HEIGHT: Pixels = px(44.);
/// A fill on a dark ground reads weaker than the same fill on a light one.
/// Past this the accent label on it drops below 4.5:1.
const FILLED_OPACITY_DARK: f32 = 0.3;
const FILLED_OPACITY_LIGHT: f32 = 0.24;
/// A suggested button's outline while its keys are off.
const DIMMED_OUTLINE_OPACITY: f32 = 0.4;
/// One step under the note's text.
const SYSTEM_LINE_SIZE: Rems = rems(0.875);
const SYSTEM_LINE_HEIGHT: Rems = rems(1.1);
/// Thock Light's muted text sits nearer its text than Dark's. This keeps a
/// visible step and still reads at 4.5:1 on a card.
const SYSTEM_LINE_OPACITY_LIGHT: f32 = 0.9;
/// Spread round the wheel, past the reds that read as an error and the
/// accent's amber. Shared by both themes, so a person's colour doesn't change
/// with the appearance; only the lightness does.
const SENDER_HUES: [f32; 6] = [212., 268., 176., 134., 322., 88.];

actions!(
    thock,
    [
        /// Opens the Inbox: what's waiting, one key per decision.
        OpenInbox,
        /// Files the selected inbox item where its suggestion points; does
        /// nothing when there is none.
        AcceptInboxSuggestion,
        /// Adds the selected inbox item to today's Day planner as a task and
        /// archives it.
        SendInboxItemToToday,
        /// Adds the selected inbox item to the Backlog's Soon list and
        /// archives it.
        SendInboxItemToSoon,
        /// Adds the selected inbox item to the Backlog's Someday list and
        /// archives it.
        SendInboxItemToSomeday,
        /// Archives the selected inbox item without adding a task.
        ArchiveInboxItem,
        /// Opens the selected inbox item in the editor.
        OpenInboxItem,
        /// Opens the selected inbox item's link in the browser: the email in
        /// Gmail, or the page it was saved from.
        OpenInboxLink,
        /// Puts the last inbox decision back the way it was.
        UndoInboxMove,
        /// Marks or unmarks the selected inbox item, so the next decision
        /// applies to every marked item.
        MarkInboxItem,
        /// Asks for a suggested destination again for the selected or marked
        /// inbox items.
        SuggestInboxDestinations,
        /// Filters the inbox by title, sender or source.
        FilterInbox,
        /// Moves between the inbox list and the note on the right.
        FocusInboxReader,
        /// Shows what can be done with the selected inbox item.
        ShowInboxItemMenu,
        /// Shows the inbox keys.
        ShowInboxKeys,
        /// Scrolls the inbox note down a little.
        ScrollInboxReaderDown,
        /// Scrolls the inbox note up a little.
        ScrollInboxReaderUp,
        /// Scrolls the inbox note down half a page.
        ScrollInboxReaderPageDown,
        /// Scrolls the inbox note up half a page.
        ScrollInboxReaderPageUp,
        /// Jumps to the next message of the inbox email.
        NextInboxMessage,
        /// Jumps to the previous message of the inbox email.
        PreviousInboxMessage,
        /// Shows or hides what the inbox email folded away: older messages,
        /// repeated text, quotes, signatures and field tables.
        ToggleInboxFolds,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &OpenInbox, window, cx| {
            InboxView::open(workspace, window, cx);
        });
    })
    .detach();
}

/// `.thock/state/inbox/layout.json` (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Layout {
    list_width: f32,
}

/// The task line a decision appended, enough to take it back.
#[derive(Debug, Clone, PartialEq)]
struct TaskWrite {
    path: PathBuf,
    line: String,
}

/// One filed item of the last decision (§6.2).
#[derive(Debug, Clone, PartialEq)]
struct FiledItem {
    stem: String,
    destination: Destination,
    inbox_path: PathBuf,
    archive_path: PathBuf,
    /// Whether the move happened; a note the phone archived meanwhile is
    /// never moved back by undo.
    moved: bool,
    task: Option<TaskWrite>,
    log_line: String,
    at: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Decision {
    items: Vec<FiledItem>,
}

/// What a decision learned while filing one item, for the notices.
#[derive(Debug, Clone, PartialEq)]
enum FilingNote {
    Filed,
    /// A file at both paths: nothing written, the row stays (§6.1).
    AlreadyArchived,
    /// Nothing at the source: the ritual or the phone got there first.
    AlreadyGone,
}

/// The rows a keystroke asked to file, each with its destination.
type FilingPlan = Vec<(InboxEntry, Destination)>;

/// The selected note as the reader shows it, rebuilt when its bytes change.
#[derive(Default)]
struct ReaderNote {
    hash: Option<String>,
    /// How an email reads; other notes render whole.
    thread: Option<Rc<EmailThread>>,
    /// The whole subject, when the sender's system cut it short.
    title: Option<String>,
    /// By message and block, built as they come into view.
    markdown: HashMap<(usize, usize), Entity<Markdown>>,
    folds_open: bool,
    /// The message open while the folds are closed; a note opens on its
    /// newest.
    current_message: usize,
    /// The current message moved or the note changed, so the reader follows
    /// once the new layout is in the frame.
    reveal_current: bool,
    /// What the earlier cue says, as of the last painted frame.
    messages_above: usize,
}

/// What every block of the reader is drawn with.
struct ReaderStyle {
    body: MarkdownStyle,
    folded: MarkdownStyle,
    /// A ticketing system's own lines, under the person's words.
    system: MarkdownStyle,
    images: Rc<dyn Fn(&str) -> Option<ImageSource>>,
}

/// The one toast a decision shows; undo dismisses it by this id.
struct InboxDecisionToast;

pub struct InboxView {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    service: Option<Entity<InboxService>>,
    focus_handle: FocusHandle,
    reader_focus: FocusHandle,
    filter_editor: Entity<Editor>,
    filter: String,
    /// The rows as shown: new items first, then carried over, filtered.
    visible: Vec<InboxEntry>,
    new_count: usize,
    /// Kept by stem so it survives refresh (§4.2); the position is the
    /// fallback when the stem leaves the list.
    selected: Option<String>,
    selected_position: Option<usize>,
    /// A stem to select as soon as the list shows it again: undo puts a
    /// note back before the folder event re-lists it.
    pending_selection: Option<String>,
    marked: HashSet<String>,
    scroll: UniformListScrollHandle,
    reader_scroll: ScrollHandle,
    reader_note: ReaderNote,
    header_fit: HeaderFit,
    list_fraction: f32,
    layout_save: Option<Task<()>>,
    last_decision: Option<Decision>,
    in_flight: bool,
    /// Decisions made while one was still writing, in order; `space`
    /// `space` `space` must not drop the middle one.
    queued: std::collections::VecDeque<(FilingPlan, &'static str)>,
    /// Stems being filed or waiting to be. They leave the list at the
    /// keystroke so a decision feels immediate, and come back if it fails.
    filing: HashSet<String>,
    /// Stems filed whose note the service still lists because the folder
    /// event hasn't landed yet; hidden until it does.
    departed: HashSet<String>,
    status: Option<SharedString>,
    filed_today: usize,
    plus_banner_dismissed: bool,
    keys_sheet: bool,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    /// The tab that was active when the view opened; `escape` returns to it.
    previous_item: Option<Box<dyn WeakItemHandle>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ItemEvent> for InboxView {}

impl Focusable for InboxView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for InboxView {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        match self
            .service
            .as_ref()
            .map(|service| self.count_for_tab(service, _cx))
        {
            Some(count) if count > 0 => format!("Inbox · {count}").into(),
            _ => "Inbox".into(),
        }
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Envelope))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        None
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn fills_center(&self) -> bool {
        true
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl InboxView {
    /// `thock::OpenInbox`: one tab per workspace, opened in the active pane
    /// or focused where it already is (§4.1).
    pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let existing = workspace.items_of_type::<InboxView>(cx).next();
        if let Some(existing) = existing {
            workspace.activate_item(&existing, true, true, window, cx);
            return;
        }
        let previous_item = workspace.active_item(cx).map(|item| item.downgrade_item());
        let project = workspace.project().clone();
        let workspace_handle = workspace.weak_handle();
        let view = cx.new(|cx| {
            let mut view = InboxView::new(workspace_handle, project, window, cx);
            view.previous_item = previous_item;
            view
        });
        workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
    }

    pub fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let service = inbox_service::service_for_project(&project, cx);
        let filter_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Filter by title, sender, source", window, cx);
            editor
        });
        let mut subscriptions = vec![cx.subscribe_in(
            &filter_editor,
            window,
            |this, editor, event: &EditorEvent, _window, cx| {
                if let EditorEvent::BufferEdited = event {
                    this.filter = editor.read(cx).text(cx).trim().to_lowercase();
                    this.refresh_visible(cx);
                }
            },
        )];
        if let Some(service) = &service {
            subscriptions.push(cx.subscribe(
                service,
                |this, _, event: &InboxEvent, cx| match event {
                    InboxEvent::ItemsChanged => this.items_changed(cx),
                    InboxEvent::SuggestionsChanged => cx.notify(),
                },
            ));
            service.update(cx, |service, cx| service.view_opened(cx));
            let service = service.clone();
            subscriptions.push(cx.on_release(move |_, cx| {
                service.update(cx, |service, cx| service.view_closed(cx));
            }));
        }
        let focus_handle = cx.focus_handle();
        let reader_focus = cx.focus_handle();
        // The selection highlight and the reader's edge follow focus.
        for handle in [&focus_handle, &reader_focus] {
            subscriptions.push(cx.on_focus_in(handle, window, |_, _, cx| cx.notify()));
            subscriptions.push(cx.on_focus_out(handle, window, |_, _, _, cx| cx.notify()));
        }
        let mut this = Self {
            workspace,
            project,
            service,
            focus_handle,
            reader_focus,
            filter_editor,
            filter: String::new(),
            visible: Vec::new(),
            new_count: 0,
            selected: None,
            selected_position: None,
            pending_selection: None,
            marked: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
            reader_scroll: ScrollHandle::new(),
            reader_note: ReaderNote::default(),
            header_fit: HeaderFit::default(),
            list_fraction: DEFAULT_LIST_FRACTION,
            layout_save: None,
            last_decision: None,
            in_flight: false,
            queued: std::collections::VecDeque::new(),
            filing: HashSet::new(),
            departed: HashSet::new(),
            status: None,
            filed_today: 0,
            plus_banner_dismissed: false,
            keys_sheet: false,
            context_menu: None,
            previous_item: None,
            _subscriptions: subscriptions,
        };
        this.refresh_visible(cx);
        this.load_layout(cx);
        this
    }

    fn vault(&self, cx: &App) -> Option<Vault> {
        self.service
            .as_ref()
            .and_then(|service| service.read(cx).vault().cloned())
    }

    fn count_for_tab(&self, service: &Entity<InboxService>, cx: &App) -> usize {
        service.read(cx).queue_depth()
    }

    fn fs(&self, cx: &App) -> Arc<dyn Fs> {
        self.project.read(cx).fs().clone()
    }

    // ----- Rows -----

    /// The folder changed on disk: marks clear (§4.3), then the rows refresh.
    fn items_changed(&mut self, cx: &mut Context<Self>) {
        // A mark is a decision about particular bytes: it survives a refresh
        // only while the row reads the same, so a row the phone or an editor
        // changed underneath is never batch-filed on an old look.
        let before: std::collections::HashMap<String, String> = self
            .visible
            .iter()
            .map(|entry| (entry.item.stem.clone(), entry.hash.clone()))
            .collect();
        self.refresh_visible(cx);
        let unchanged: HashSet<&str> = self
            .visible
            .iter()
            .filter(|entry| before.get(&entry.item.stem) == Some(&entry.hash))
            .map(|entry| entry.item.stem.as_str())
            .collect();
        self.marked.retain(|stem| unchanged.contains(stem.as_str()));
    }

    /// Re-reads the service's items into the two sections and keeps the
    /// selection by stem (or by position when the stem left). Marks on rows
    /// the filter hides are kept for when it clears.
    fn refresh_visible(&mut self, cx: &mut Context<Self>) {
        let items: Vec<InboxEntry> = self
            .service
            .as_ref()
            .map(|service| service.read(cx).items().to_vec())
            .unwrap_or_default();
        let all_stems: HashSet<String> =
            items.iter().map(|entry| entry.item.stem.clone()).collect();
        self.marked.retain(|stem| all_stems.contains(stem));
        self.departed.retain(|stem| all_stems.contains(stem));
        let leaving = |entry: &InboxEntry| {
            self.filing.contains(&entry.item.stem) || self.departed.contains(&entry.item.stem)
        };
        let matches = |entry: &InboxEntry| {
            if self.filter.is_empty() {
                return true;
            }
            let item = &entry.item;
            [
                Some(item.title.as_str()),
                item.from.as_deref(),
                item.source.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|field| field.to_lowercase().contains(&self.filter))
        };
        let (new, carried): (Vec<InboxEntry>, Vec<InboxEntry>) = items
            .into_iter()
            .filter(|entry| matches(entry) && !leaving(entry))
            .partition(|entry| entry.is_new);
        self.new_count = new.len();
        self.visible = new.into_iter().chain(carried).collect();
        let stems: HashSet<&str> = self
            .visible
            .iter()
            .map(|entry| entry.item.stem.as_str())
            .collect();
        if let Some(pending) = &self.pending_selection
            && stems.contains(pending.as_str())
        {
            self.selected = self.pending_selection.take();
        }
        let still_there = self
            .selected
            .as_deref()
            .is_some_and(|stem| stems.contains(stem));
        if !still_there {
            let position = match self.selected_position {
                Some(position) if !self.visible.is_empty() => {
                    Some(position.min(self.visible.len() - 1))
                }
                _ => (!self.visible.is_empty()).then_some(0),
            };
            self.selected = position.and_then(|position| {
                self.visible
                    .get(position)
                    .map(|entry| entry.item.stem.clone())
            });
        }
        self.selected_position = self.selected_index();
        cx.notify();
    }

    fn selected_index(&self) -> Option<usize> {
        let stem = self.selected.as_deref()?;
        self.visible
            .iter()
            .position(|entry| entry.item.stem == stem)
    }

    fn selected_entry(&self) -> Option<&InboxEntry> {
        self.selected_index()
            .and_then(|index| self.visible.get(index))
    }

    fn entry_at(&self, section: usize, index: usize) -> Option<&InboxEntry> {
        let offset = if section == 0 { 0 } else { self.new_count };
        self.visible.get(offset + index)
    }

    fn position_of(&self, section: usize, index: usize) -> usize {
        if section == 0 {
            index
        } else {
            self.new_count + index
        }
    }

    fn section_of(&self, position: usize) -> (usize, usize) {
        if position < self.new_count {
            (0, position)
        } else {
            (1, position - self.new_count)
        }
    }

    fn select_position(&mut self, position: Option<usize>, cx: &mut Context<Self>) {
        self.selected = position.and_then(|position| {
            self.visible
                .get(position)
                .map(|entry| entry.item.stem.clone())
        });
        self.selected_position = self.selected_index();
        if let Some(position) = self.selected_position {
            let (section, index) = self.section_of(position);
            if let Some(row) = self.layout().row_of(section, index) {
                self.scroll
                    .scroll_to_item(row, gpui::ScrollStrategy::Nearest);
            }
        }
        cx.notify();
    }

    fn step_selection(&mut self, steps: isize, cx: &mut Context<Self>) {
        let layout = self.layout();
        let from = self
            .selected_index()
            .map(|position| self.section_of(position));
        let target = layout
            .step(from, steps)
            .map(|(section, index)| self.position_of(section, index));
        self.select_position(target, cx);
    }

    /// The items a verb applies to: every marked row in list order, else
    /// the selected row (§4.3).
    fn targets(&self) -> Vec<InboxEntry> {
        if self.marked.is_empty() {
            self.selected_entry().cloned().into_iter().collect()
        } else {
            self.visible
                .iter()
                .filter(|entry| self.marked.contains(&entry.item.stem))
                .cloned()
                .collect()
        }
    }

    /// The selected note's link. Only web links open from here: the note is
    /// untrusted input, and any other scheme could reach an app on this
    /// machine.
    fn selected_link(&self) -> Option<String> {
        let item = &self.selected_entry()?.item;
        item.link
            .clone()
            .or_else(|| item.url.clone())
            .filter(|link| link.starts_with("https://") || link.starts_with("http://"))
    }

    fn suggestion_for<'a>(
        &self,
        entry: &InboxEntry,
        cx: &'a App,
    ) -> Option<&'a crate::inbox_suggest::Suggestion> {
        self.service
            .as_ref()
            .and_then(|service| service.read(cx).suggestion_for(&entry.hash))
    }

    /// The destination `space` takes for an entry: a suggestion at or
    /// above the outlined tier (§7.4).
    fn suggested_destination(&self, entry: &InboxEntry, cx: &App) -> Option<Destination> {
        self.bar_state(entry, cx)
            .suggested
            .map(|(destination, _)| destination)
    }

    fn availability(&self, cx: &App) -> Option<SuggestionAvailability> {
        self.service
            .as_ref()
            .map(|service| service.read(cx).suggestion_availability(cx))
    }

    // ----- Actions -----

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(1, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(-1, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        let first = (!self.visible.is_empty()).then_some(0);
        self.select_position(first, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        let last = self.visible.len().checked_sub(1);
        self.select_position(last, cx);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_editor.focus_handle(cx).is_focused(window) {
            self.focus_handle.focus(window, cx);
            return;
        }
        self.open_selected(window, cx);
    }

    /// `escape`: clears marks or the filter, leaves the reader, or returns
    /// to the tab that was active before (§5).
    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.keys_sheet {
            self.keys_sheet = false;
            cx.notify();
            return;
        }
        if self.filter_editor.focus_handle(cx).is_focused(window) || !self.filter.is_empty() {
            self.filter_editor
                .update(cx, |editor, cx| editor.clear(window, cx));
            self.filter.clear();
            self.refresh_visible(cx);
            self.focus_handle.focus(window, cx);
            return;
        }
        if self.reader_focus.is_focused(window) {
            self.focus_handle.focus(window, cx);
            cx.notify();
            return;
        }
        if !self.marked.is_empty() {
            self.marked.clear();
            cx.notify();
            return;
        }
        let workspace = self.workspace.clone();
        let previous = self.previous_item.as_ref().map(|item| item.boxed_clone());
        // Activating another item deactivates this one, which reaches back
        // into the view, so the workspace must not be entered from inside
        // this update.
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    if let Some(item) = previous.and_then(|item| item.upgrade()) {
                        workspace.activate_item(item.as_ref(), true, true, window, cx);
                    }
                })
                .log_err();
        });
    }

    fn accept_suggestion(
        &mut self,
        _: &AcceptInboxSuggestion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.marked.is_empty() {
            // Each marked row goes where its own chip points; rows without
            // one are skipped and stay marked (§4.3).
            let plan: FilingPlan = self
                .targets()
                .into_iter()
                .filter_map(|entry| {
                    let destination = self.suggested_destination(&entry, cx)?;
                    Some((entry, destination))
                })
                .collect();
            self.file(plan, "space", window, cx);
            return;
        }
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        // Without a suggestion `space` does nothing: opening the note is
        // `o`'s job, and a key that sometimes files and sometimes opens is a
        // surprise.
        if let Some(destination) = self.suggested_destination(&entry, cx) {
            self.file(vec![(entry, destination)], "space", window, cx);
        }
    }

    fn plan_for(&self, destination: Destination) -> FilingPlan {
        self.targets()
            .into_iter()
            .map(|entry| (entry, destination))
            .collect()
    }

    fn send_to_today(
        &mut self,
        _: &SendInboxItemToToday,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let plan = self.plan_for(Destination::Today);
        self.file(plan, "t", window, cx);
    }

    fn send_to_soon(
        &mut self,
        _: &SendInboxItemToSoon,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let plan = self.plan_for(Destination::Soon);
        self.file(plan, "s", window, cx);
    }

    fn send_to_someday(
        &mut self,
        _: &SendInboxItemToSomeday,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let plan = self.plan_for(Destination::Someday);
        self.file(plan, "S", window, cx);
    }

    fn archive(&mut self, _: &ArchiveInboxItem, window: &mut Window, cx: &mut Context<Self>) {
        let plan = self.plan_for(Destination::Archive);
        self.file(plan, "e", window, cx);
    }

    fn open_item(&mut self, _: &OpenInboxItem, window: &mut Window, cx: &mut Context<Self>) {
        self.open_selected(window, cx);
    }

    fn open_link(&mut self, _: &OpenInboxLink, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(link) = self.selected_link() {
            cx.open_url(&link);
        }
    }

    fn undo(&mut self, _: &UndoInboxMove, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_last(window, cx);
    }

    fn mark(&mut self, _: &MarkInboxItem, _: &mut Window, cx: &mut Context<Self>) {
        let Some(stem) = self.selected.clone() else {
            return;
        };
        if !self.marked.remove(&stem) {
            self.marked.insert(stem);
        }
        cx.notify();
    }

    fn suggest(&mut self, _: &SuggestInboxDestinations, _: &mut Window, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        let hashes: Vec<String> = if self.marked.is_empty() && self.selected.is_none() {
            self.visible
                .iter()
                .filter(|entry| self.suggested_destination(entry, cx).is_none())
                .map(|entry| entry.hash.clone())
                .collect()
        } else {
            self.targets().into_iter().map(|entry| entry.hash).collect()
        };
        service.update(cx, |service, cx| service.request_suggestions(&hashes, cx));
    }

    fn filter(&mut self, _: &FilterInbox, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_editor.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn focus_reader(&mut self, _: &FocusInboxReader, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader_focus.is_focused(window) {
            self.focus_handle.focus(window, cx);
        } else {
            self.reader_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn show_menu(&mut self, _: &ShowInboxItemMenu, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_entry().is_none() {
            return;
        }
        let bounds = self.scroll.0.borrow().base_handle.bounds();
        let position = point(bounds.left() + px(24.), bounds.top() + px(24.));
        self.deploy_context_menu(position, window, cx);
    }

    fn show_keys(&mut self, _: &ShowInboxKeys, _: &mut Window, cx: &mut Context<Self>) {
        self.keys_sheet = !self.keys_sheet;
        cx.notify();
    }

    fn scroll_reader(&mut self, amount: Pixels, cx: &mut Context<Self>) {
        let offset = self.reader_scroll.offset();
        let max = self.reader_scroll.max_offset();
        let y = (offset.y - amount).clamp(-max.y, px(0.));
        self.reader_scroll.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn scroll_reader_down(
        &mut self,
        _: &ScrollInboxReaderDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_reader(READER_LINE, cx);
    }

    fn scroll_reader_up(
        &mut self,
        _: &ScrollInboxReaderUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_reader(-READER_LINE, cx);
    }

    fn scroll_reader_page_down(
        &mut self,
        _: &ScrollInboxReaderPageDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let half = self.reader_scroll.bounds().size.height / 2.;
        self.scroll_reader(half, cx);
    }

    fn scroll_reader_page_up(
        &mut self,
        _: &ScrollInboxReaderPageUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let half = self.reader_scroll.bounds().size.height / 2.;
        self.scroll_reader(-half, cx);
    }

    /// Opens a message of the thread and brings it into view.
    fn open_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.reader_note.current_message = index;
        self.reader_note.reveal_current = true;
        cx.notify();
    }

    fn step_message(&mut self, forward: bool, cx: &mut Context<Self>) {
        let last = self
            .reader_note
            .thread
            .as_ref()
            .map_or(0, |thread| thread.messages.len().saturating_sub(1));
        let current = self.reader_note.current_message;
        let target = if forward {
            (current + 1).min(last)
        } else {
            current.saturating_sub(1)
        };
        if target != current {
            self.open_message(target, cx);
        }
    }

    fn next_message(&mut self, _: &NextInboxMessage, _: &mut Window, cx: &mut Context<Self>) {
        self.step_message(true, cx);
    }

    fn previous_message(
        &mut self,
        _: &PreviousInboxMessage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_message(false, cx);
    }

    /// Brings the current card to the top of the reader, under the earlier
    /// cue. A body takes its height once parsed, so while any is still parsing
    /// this runs again on the frame that lands it.
    fn reveal_current_message(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self
            .reader_note
            .thread
            .as_ref()
            .filter(|thread| thread.messages.len() > 1)
        else {
            return;
        };
        let preamble = usize::from(!thread.preamble.is_empty());
        let current = self.reader_note.current_message;
        let viewport = self.reader_scroll.bounds();
        let landings: Option<Vec<Pixels>> = (0..=current)
            .map(|index| {
                let card = self.reader_scroll.bounds_for_item(index + preamble)?;
                Some((card.top() - viewport.top() - REVEAL_CLEARANCE).max(px(0.)))
            })
            .collect();
        let parsing = self
            .reader_note
            .markdown
            .values()
            .any(|markdown| markdown.read(cx).is_parsing());
        self.reader_note.reveal_current = parsing;
        let Some(landings) = landings else {
            return;
        };
        let farthest = self.reader_scroll.max_offset().y;
        let scrolled = landing(&landings, farthest, viewport.size.height);
        let offset = self.reader_scroll.offset();
        if offset.y != -scrolled {
            self.reader_scroll.set_offset(point(offset.x, -scrolled));
            // Looked at again on the next frame, until the layout holds still.
            self.reader_note.reveal_current = true;
            cx.notify();
        }
    }

    fn toggle_folds(&mut self, _: &ToggleInboxFolds, _: &mut Window, cx: &mut Context<Self>) {
        self.reader_note.folds_open = !self.reader_note.folds_open;
        self.reader_note.reveal_current = true;
        cx.notify();
    }

    fn triage(&mut self, _: &TriageInbox, window: &mut Window, cx: &mut Context<Self>) {
        // The ritual takes what is left; the workspace handler runs it.
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            if let Some(workspace) = workspace.upgrade() {
                workspace
                    .read(cx)
                    .focus_handle(cx)
                    .dispatch_action(&TriageInbox, window, cx);
            }
        });
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let Some(vault) = self.vault(cx) else {
            return;
        };
        let path = vault.root.join(&entry.item.path);
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace
                        .open_abs_path(
                            path,
                            OpenOptions {
                                visible: Some(OpenVisible::All),
                                ..Default::default()
                            },
                            window,
                            cx,
                        )
                        .detach_and_log_err(cx);
                })
                .log_err();
        });
    }

    fn deploy_context_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.focus_handle.clone();
        let view = cx.weak_entity();
        let suggested = self
            .selected_entry()
            .and_then(|entry| self.suggested_destination(entry, cx));
        let menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let file = |destination: Destination| {
                let view = view.clone();
                move |window: &mut Window, cx: &mut App| {
                    view.update(cx, |this, cx| {
                        let plan = this.plan_for(destination);
                        this.file(plan, "menu", window, cx);
                    })
                    .log_err();
                }
            };
            let menu = menu
                .context(focus)
                .when_some(suggested, |menu, destination| {
                    menu.entry(
                        format!("Take suggestion · {}", destination.label()),
                        Some(AcceptInboxSuggestion.boxed_clone()),
                        file(destination),
                    )
                    .separator()
                });
            menu.entry(
                "Today",
                Some(SendInboxItemToToday.boxed_clone()),
                file(Destination::Today),
            )
            .entry(
                "Soon",
                Some(SendInboxItemToSoon.boxed_clone()),
                file(Destination::Soon),
            )
            .entry(
                "Someday",
                Some(SendInboxItemToSomeday.boxed_clone()),
                file(Destination::Someday),
            )
            .entry(
                "Archive",
                Some(ArchiveInboxItem.boxed_clone()),
                file(Destination::Archive),
            )
            .separator()
            .action("Open note", OpenInboxItem.boxed_clone())
            .action("Mark", MarkInboxItem.boxed_clone())
            .action("Suggest again", SuggestInboxDestinations.boxed_clone())
            .action("Undo last decision", UndoInboxMove.boxed_clone())
        });
        window.focus(&menu.focus_handle(cx), cx);
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
                this.context_menu.take();
                this.focus_handle.focus(window, cx);
                cx.notify();
            });
        self.context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    // ----- Writes (§6) -----

    /// Files each row of `plan`: V40 §5's writes per item, in list order,
    /// each awaited before the next; then the decision log, the selection
    /// move and the toast with Undo. A plan arriving while one is still
    /// writing waits its turn instead of being dropped.
    fn file(
        &mut self,
        plan: FilingPlan,
        via: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if plan.is_empty() {
            return;
        }
        let Some(vault) = self.vault(cx) else {
            self.status = Some("This workspace isn't a Thock vault.".into());
            cx.notify();
            return;
        };
        self.move_selection_past(&plan, cx);
        if self.in_flight {
            self.queued.push_back((plan, via));
            return;
        }
        // What the chip said, for the decision log; only when a chip was
        // shown at all (§7.6).
        let suggestions_on = self.availability(cx) == Some(SuggestionAvailability::On);
        let suggestions: Vec<Option<(String, f64)>> = plan
            .iter()
            .map(|(entry, _)| {
                suggestions_on
                    .then(|| self.suggestion_for(entry, cx))
                    .flatten()
                    .map(|suggestion| (suggestion.choice.clone(), suggestion.confidence))
            })
            .collect();
        self.in_flight = true;
        self.status = None;
        cx.notify();

        let project = self.project.clone();
        let fs = self.fs(cx);
        cx.spawn_in(window, async move |this, cx| {
            let mut filed = Vec::new();
            let mut filed_suggestions = Vec::new();
            let mut notes = Vec::new();
            let mut failure: Option<anyhow::Error> = None;
            for ((entry, destination), suggestion) in plan.iter().zip(suggestions) {
                match file_one(&project, &fs, &vault, entry, *destination, cx).await {
                    Ok((Some(item), note)) => {
                        filed.push(item);
                        filed_suggestions.push(suggestion);
                        notes.push(note);
                    }
                    Ok((None, note)) => notes.push(note),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.in_flight = false;
                for (entry, _) in &plan {
                    this.filing.remove(&entry.item.stem);
                }
                for item in filed.iter().filter(|item| item.moved) {
                    this.departed.insert(item.stem.clone());
                }
                this.refresh_visible(cx);
                if !filed.is_empty() {
                    this.filed_today += filed.len();
                    for item in &filed {
                        this.marked.remove(&item.stem);
                    }
                    this.record_decisions(&filed, &filed_suggestions, via, cx);
                    this.reconcile_gmail_archive(cx);
                    this.show_undo_toast(&filed, window, cx);
                    this.last_decision = Some(Decision { items: filed });
                } else if this.queued.is_empty() {
                    // Nothing left and nothing waiting: the row the keystroke
                    // meant is still there, so the selection goes back to it.
                    let stem = plan.first().map(|(entry, _)| entry.item.stem.clone());
                    if stem.as_deref().is_some_and(|stem| {
                        this.visible.iter().any(|entry| entry.item.stem == stem)
                    }) {
                        this.pending_selection = None;
                        this.selected = stem;
                        this.selected_position = this.selected_index();
                    }
                }
                if notes.contains(&FilingNote::AlreadyArchived) {
                    this.status = Some("Already archived · open the folder".into());
                }
                if let Some(error) = failure {
                    this.status = Some(format!("Couldn't file the item: {error:#}").into());
                }
                cx.notify();
                this.run_queued(window, cx);
            })
            .log_err();
        })
        .detach();
    }

    /// Moves the selection to where it lands once `plan`'s rows leave: the
    /// first row after the last target that is not itself being filed, else
    /// the last such row before the first. Done at the keystroke, so the
    /// next key addresses the next row while this one is still writing.
    fn move_selection_past(&mut self, plan: &FilingPlan, cx: &mut Context<Self>) {
        for (entry, _) in plan {
            self.filing.insert(entry.item.stem.clone());
        }
        let is_target = |entry: &InboxEntry| {
            plan.iter()
                .any(|(target, _)| target.item.stem == entry.item.stem)
        };
        let free = |entry: &InboxEntry| !self.filing.contains(&entry.item.stem);
        let first = self.visible.iter().position(|entry| is_target(entry));
        let last = self.visible.iter().rposition(|entry| is_target(entry));
        let landing = last
            .and_then(|last| self.visible[last + 1..].iter().find(|entry| free(entry)))
            .or_else(|| {
                first.and_then(|first| self.visible[..first].iter().rfind(|entry| free(entry)))
            })
            .map(|entry| entry.item.stem.clone());
        if landing.is_some() {
            self.selected = landing.clone();
            self.pending_selection = landing;
        }
        self.refresh_visible(cx);
    }

    /// Runs the next waiting plan, minus rows that have left or were just
    /// filed.
    fn run_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((plan, via)) = self.queued.pop_front() else {
            return;
        };
        let listed: HashSet<String> = self
            .service
            .as_ref()
            .map(|service| {
                service
                    .read(cx)
                    .items()
                    .iter()
                    .map(|entry| entry.item.stem.clone())
                    .collect()
            })
            .unwrap_or_default();
        let just_filed: HashSet<&str> = self
            .last_decision
            .iter()
            .flat_map(|decision| decision.items.iter())
            .map(|item| item.stem.as_str())
            .collect();
        let (plan, dropped): (FilingPlan, FilingPlan) = plan.into_iter().partition(|(entry, _)| {
            listed.contains(&entry.item.stem)
                && !self.departed.contains(&entry.item.stem)
                && !just_filed.contains(entry.item.stem.as_str())
        });
        for (entry, _) in dropped {
            self.filing.remove(&entry.item.stem);
        }
        if plan.is_empty() {
            self.run_queued(window, cx);
            return;
        }
        self.file(plan, via, window, cx);
    }

    fn record_decisions(
        &mut self,
        filed: &[FiledItem],
        suggestions: &[Option<(String, f64)>],
        via: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(service) = self.service.clone() else {
            return;
        };
        service.update(cx, |service, cx| {
            for (item, suggestion) in filed.iter().zip(suggestions) {
                service.record_decision(
                    &DecisionRecord {
                        stem: item.stem.clone(),
                        suggested: suggestion.as_ref().map(|(choice, _)| choice.clone()),
                        confidence: suggestion.as_ref().map(|(_, confidence)| *confidence),
                        chosen: item.destination.key().to_string(),
                        via: via.to_string(),
                        at: item.at.clone(),
                        undone: false,
                    },
                    cx,
                );
            }
        });
    }

    /// A decision or its undo reaches Gmail archive without waiting for the
    /// next poll (V53 §7.4).
    fn reconcile_gmail_archive(&self, cx: &mut Context<Self>) {
        if let Some(gmail) = crate::gmail_service::service_for_project(&self.project, cx) {
            gmail.update(cx, |gmail, cx| gmail.reconcile_soon(cx));
        }
    }

    fn show_undo_toast(&self, filed: &[FiledItem], _window: &mut Window, cx: &mut Context<Self>) {
        let count = filed.len();
        let destination = filed.first().map(|item| item.destination);
        let one_destination = filed
            .iter()
            .all(|item| Some(item.destination) == destination);
        let message = match (destination, count) {
            (_, 0) => return,
            (Some(Destination::Archive), 1) if one_destination => "Archived".to_string(),
            (Some(Destination::Archive), n) if one_destination => format!("Archived {n} items"),
            (Some(destination), 1) => format!("Added to {}", destination.label()),
            (Some(destination), n) if one_destination => {
                format!("Added {n} items to {}", destination.label())
            }
            (_, n) => format!("Filed {n} items"),
        };
        let view = cx.weak_entity();
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_toast(
                        Toast::new(NotificationId::unique::<InboxDecisionToast>(), message)
                            .on_click("Undo", move |window, cx| {
                                view.update(cx, |this, cx| this.undo_last(window, cx))
                                    .log_err();
                            })
                            .autohide(),
                        cx,
                    );
                })
                .log_err();
        });
    }

    /// `u`: reverses the last decision, items in reverse order (§6.2).
    fn undo_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        let Some(decision) = self.last_decision.take() else {
            return;
        };
        let Some(vault) = self.vault(cx) else {
            return;
        };
        self.in_flight = true;
        self.status = None;
        cx.notify();
        let project = self.project.clone();
        let fs = self.fs(cx);
        let service = self.service.clone();
        let first_stem = decision.items.first().map(|item| item.stem.clone());
        for item in &decision.items {
            self.departed.remove(&item.stem);
        }
        cx.spawn_in(window, async move |this, cx| {
            let mut failure = None;
            for item in decision.items.iter().rev() {
                if let Err(error) = undo_one(&project, &fs, &vault, item, cx).await {
                    failure = Some(error);
                    break;
                }
                if let Some(service) = &service {
                    service.update(cx, |service, cx| {
                        service.mark_decision_undone(&item.stem, &item.at, cx)
                    });
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.in_flight = false;
                this.filed_today = this.filed_today.saturating_sub(decision.items.len());
                this.pending_selection = first_stem;
                if let Some(error) = failure {
                    this.status = Some(format!("Couldn't undo: {error:#}").into());
                }
                this.reconcile_gmail_archive(cx);
                let workspace = this.workspace.clone();
                cx.defer(move |cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .dismiss_toast(&NotificationId::unique::<InboxDecisionToast>(), cx);
                        })
                        .log_err();
                });
                this.refresh_visible(cx);
                this.run_queued(window, cx);
            })
            .log_err();
        })
        .detach();
    }

    // ----- Layout (§4.2) -----

    fn load_layout(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault(cx) else {
            return;
        };
        let fs = self.fs(cx);
        let path = vault.root.join(VAULT_MARKER_DIR).join(LAYOUT_PATH);
        cx.spawn(async move |this, cx| {
            let Ok(text) = fs.load(&path).await else {
                return;
            };
            let Ok(layout) = serde_json::from_str::<Layout>(&text) else {
                return;
            };
            this.update(cx, |this, cx| {
                this.list_fraction = layout.list_width.clamp(0.1, 0.9);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn resize(&mut self, fraction: f32, cx: &mut Context<Self>) {
        if (self.list_fraction - fraction).abs() < f32::EPSILON {
            return;
        }
        self.list_fraction = fraction;
        cx.notify();
        let Some(vault) = self.vault(cx) else {
            return;
        };
        let fs = self.fs(cx);
        let path = vault.root.join(VAULT_MARKER_DIR).join(LAYOUT_PATH);
        // Latest wins: only the final width of a drag needs writing.
        self.layout_save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let text = serde_json::to_string(&Layout {
                list_width: fraction,
            })
            .unwrap_or_default();
            if let Some(parent) = path.parent()
                && fs.create_dir(parent).await.log_err().is_some()
            {
                fs.atomic_write(path, text).await.log_err();
            }
        }));
    }

    // ----- Rendering -----

    fn carried_count(&self) -> usize {
        self.visible.len().saturating_sub(self.new_count)
    }

    /// The list header's section counts (§4.2): a section with no rows has
    /// none, and the one holding the selection is active.
    fn section_counts(&self) -> Vec<SectionCount> {
        let active = self
            .selected_index()
            .map(|position| self.section_of(position).0);
        [
            (0, "New", self.new_count),
            (1, "Carried over", self.carried_count()),
        ]
        .into_iter()
        .filter(|(_, _, count)| *count > 0)
        .map(|(section, label, count)| SectionCount {
            section,
            label,
            count,
            active: active == Some(section),
        })
        .collect()
    }

    fn scroll_to_section(&mut self, section: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.layout().section_start(section) {
            self.scroll.scroll_to_item(row, gpui::ScrollStrategy::Top);
            cx.notify();
        }
    }

    /// What the destination bar shows for a row (§4.2, §7.4). Without a
    /// suggestion, or with suggestions off, nothing is highlighted and no
    /// probabilities show; an `unsure` answer still shows its spread.
    fn bar_state(&self, entry: &InboxEntry, cx: &App) -> BarState {
        // The cache may hold answers from when the vault had a grant; without
        // one today there is nothing to show.
        if self.availability(cx) != Some(SuggestionAvailability::On) {
            return BarState::default();
        }
        let Some(suggestion) = self.suggestion_for(entry, cx) else {
            return BarState::default();
        };
        let suggested = match (tier(suggestion), suggestion.destination) {
            (tier @ (Tier::Filled | Tier::Outlined), Some(destination)) => {
                Some((destination, tier))
            }
            _ => None,
        };
        let probabilities = Destination::ALL.map(|destination| {
            suggestion
                .probabilities
                .get(destination.key())
                .copied()
                .unwrap_or(0.0)
        });
        BarState {
            suggested,
            leaning: suggested
                .is_none()
                .then(|| most_likely(&probabilities))
                .flatten(),
            probabilities: Some(probabilities),
        }
    }

    /// The list footer's quiet stats (§4.2).
    fn footer_stats(&self, cx: &App) -> FooterStats {
        let availability = self.availability(cx);
        if availability == Some(SuggestionAvailability::NoGrant) && !self.plus_banner_dismissed {
            return FooterStats::PlusOffer;
        }
        let mut parts = Vec::new();
        match availability {
            Some(SuggestionAvailability::On) if self.visible.is_empty() => {}
            Some(SuggestionAvailability::On) => parts.push("Suggestions on".to_string()),
            Some(SuggestionAvailability::OffByConfig) => parts.push("Suggestions off".to_string()),
            Some(SuggestionAvailability::Exhausted) => {
                parts.push("Suggestions paused · allowance spent".to_string())
            }
            Some(SuggestionAvailability::NoGrant) | None => {}
        }
        if self.filed_today > 0 {
            parts.push(format!("{} filed today", self.filed_today));
        }
        if let Some(service) = &self.service {
            let cost = service.read(cx).session_cost();
            if cost > 0.0 {
                parts.push(format_cost(cost));
            }
        }
        FooterStats::Stats(parts.join(" · "))
    }

    fn render_list_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let sections = self.section_counts();
        // With nothing beside it, a section has nothing to be told apart from
        // or scrolled to: it's only a count.
        let lone = sections.len() == 1;
        let counts = sections.into_iter().map(|count| {
            let selector = if count.section == 0 {
                "INBOX_SECTION-new"
            } else {
                "INBOX_SECTION-carried"
            };
            let section = count.section;
            let content = h_flex()
                .gap_1()
                .child(Label::new(count.label).color(if count.active && !lone {
                    Color::Default
                } else {
                    Color::Muted
                }))
                .child(Label::new(count.count.to_string()).color(Color::Muted));
            let count_element = if lone {
                content.into_any_element()
            } else {
                ButtonLike::new(("inbox-section", count.section))
                    .size(ButtonSize::Medium)
                    .style(if count.active {
                        ButtonStyle::Filled
                    } else {
                        ButtonStyle::Subtle
                    })
                    .child(content)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.scroll_to_section(section, cx);
                    }))
                    .into_any_element()
            };
            div()
                .debug_selector(|| selector.to_string())
                .child(count_element)
        });
        h_flex()
            .size_full()
            .pl_5()
            .pr_3()
            .gap_1()
            .child(
                div().mr_2().flex_none().child(
                    Label::new("Inbox")
                        .size(LabelSize::Large)
                        .weight(FontWeight::SEMIBOLD),
                ),
            )
            .children(counts)
            .child(div().flex_1())
            .child(
                IconButton::new("inbox-filter", IconName::MagnifyingGlass)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::for_action_title("Filter", &FilterInbox))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.filter(&FilterInbox, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// The filter box, on its own line at the top of the list so it keeps a
    /// whole edge inside the pane however narrow the pane is.
    fn render_filter_bar(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        let focused = self.filter_editor.focus_handle(cx).is_focused(window);
        if !focused && self.filter.is_empty() {
            return None;
        }
        let colors = cx.theme().colors();
        let leave = self.move_keycap(
            &["menu::Cancel"],
            ["escape", "escape"],
            &Cancel,
            list_key_context(),
            window,
            cx,
        );
        // A lone section's count in the header already says this.
        let matches = (!self.filter.is_empty() && self.section_counts().len() > 1).then(|| {
            div()
                .debug_selector(|| "INBOX_FILTER_COUNT".to_string())
                .flex_none()
                .child(
                    Label::new(match_count_label(self.visible.len()))
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .single_line(),
                )
        });
        Some(
            div()
                .flex_none()
                .w_full()
                .px_1()
                .pt_2()
                .child(
                    h_flex()
                        .debug_selector(|| "INBOX_FILTER".to_string())
                        .w_full()
                        .h(FILTER_BOX_HEIGHT)
                        .px_2()
                        .gap_2()
                        .rounded_md()
                        .border_1()
                        .border_color(if focused {
                            colors.border_focused
                        } else {
                            colors.border
                        })
                        .child(div().flex_1().min_w_0().child(self.filter_editor.clone()))
                        .children(matches)
                        .child(framed_keycap(leave, false, cx)),
                )
                .into_any_element(),
        )
    }

    /// An action on the note rather than a place for it, so it sits in the
    /// header, clear of the destination bar. While its key is off the keycap
    /// and the words dim as one hint; a click still works.
    fn render_quiet_action(
        &self,
        name: &'static str,
        action: &dyn Action,
        keycap: KeyBinding,
        label: &'static str,
        dimmed: bool,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let focus = self.focus_handle.clone();
        let boxed_action = action.boxed_clone();
        div()
            .id(SharedString::from(format!("inbox-action-wrap-{name}")))
            .debug_selector(move || format!("INBOX_ACTION-{name}"))
            .flex_none()
            .child(
                ButtonLike::new(SharedString::from(format!("inbox-action-{name}")))
                    .child(
                        h_flex()
                            .when(dimmed, |this| {
                                this.debug_selector(move || format!("INBOX_DIMMED-{name}"))
                            })
                            .gap_1()
                            .child(framed_keycap(keycap, dimmed, cx))
                            .child(
                                Label::new(label)
                                    .size(LabelSize::Small)
                                    .color(hint_color(dimmed)),
                            ),
                    )
                    .on_click(move |_, window, cx| {
                        focus.dispatch_action(boxed_action.as_ref(), window, cx)
                    }),
            )
    }

    fn render_reader_header(&self, window: &Window, cx: &App) -> AnyElement {
        let entry = self.selected_entry();
        let item = entry.map(|entry| &entry.item);
        let source = item.and_then(|item| item.source.as_deref());
        let fit = self.header_fit;
        let mut meta = h_flex().flex_1().min_w_0().overflow_hidden().gap_2();
        if let Some(item) = item {
            if fit != HeaderFit::Tight {
                meta = meta.child(
                    Icon::new(source_icon(source))
                        .size(IconSize::Small)
                        .color(Color::Muted),
                );
            }
            let name = item
                .from
                .as_deref()
                .map(sender_name)
                .or_else(|| source.map(source_label));
            let named = name.is_some();
            if let Some(name) = name {
                let address = item.from.as_deref().and_then(sender_address);
                let (name, rest) = sender_parts(name, address);
                let rest = rest.filter(|_| fit == HeaderFit::Roomy);
                let joined = rest.as_ref().is_some_and(|rest| rest.starts_with('@'));
                meta = meta.child(
                    h_flex()
                        .id("inbox-header-sender")
                        .debug_selector(|| "INBOX_HEADER_SENDER".to_string())
                        .min_w_0()
                        .when(!joined, |this| this.gap_1p5())
                        .child(
                            div()
                                .min_w_0()
                                .child(Label::new(name).size(LabelSize::Small).truncate()),
                        )
                        .children(rest.map(|rest| {
                            // Far stronger than the name's shrink, so this is
                            // gone before the name loses a character.
                            div().min_w_0().flex_shrink(1000.).child(
                                Label::new(rest)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                        }))
                        .when_some(item.from.clone(), |this, from| {
                            this.tooltip(Tooltip::text(from))
                        }),
                );
            }
            let mut when = Vec::new();
            if let Some(captured) = &item.captured {
                when.push(captured_label(captured));
            }
            if let Some(due) = &item.due {
                when.push(format!("due {due}"));
            }
            if !when.is_empty() {
                let separator = if named { "· " } else { "" };
                meta = meta.child(
                    div()
                        .debug_selector(|| "INBOX_HEADER_WHEN".to_string())
                        .flex_none()
                        .child(
                            Label::new(format!("{separator}{}", when.join(" · ")))
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .single_line(),
                        ),
                );
            }
        }
        let link_text = match (source == Some("gmail"), fit) {
            (true, HeaderFit::Tight) => "Gmail ↗",
            (true, _) => "Open in Gmail ↗",
            (false, HeaderFit::Tight) => "Link ↗",
            (false, _) => "Open link ↗",
        };
        let list_key = |action: &dyn Action| self.keycap(action, list_key_context(), window, cx);
        let list_keys_off = !self.list_keys_live(window, cx);
        h_flex()
            .size_full()
            .px_10()
            .child(
                h_flex()
                    .w_full()
                    .max_w(READER_MAX_WIDTH)
                    .h_full()
                    .gap_2()
                    .child(meta)
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .mr(-ACTION_PADDING)
                            .when(entry.is_some(), |this| {
                                this.child(self.render_quiet_action(
                                    "open",
                                    &OpenInboxItem,
                                    list_key(&OpenInboxItem),
                                    "Open note",
                                    list_keys_off,
                                    cx,
                                ))
                            })
                            // A last decision outlives the list it emptied.
                            .when(entry.is_some() || self.last_decision.is_some(), |this| {
                                this.child(self.render_quiet_action(
                                    "undo",
                                    &UndoInboxMove,
                                    list_key(&UndoInboxMove),
                                    "Undo",
                                    list_keys_off,
                                    cx,
                                ))
                            })
                            .when_some(self.selected_link(), |this, link| {
                                // The link's key also answers from the reader,
                                // where it's wanted most.
                                this.child(
                                    self.render_quiet_action(
                                        "link",
                                        &OpenInboxLink,
                                        list_key(&OpenInboxLink),
                                        link_text,
                                        self.filter_editor.focus_handle(cx).is_focused(window),
                                        cx,
                                    )
                                    .tooltip(Tooltip::text(link)),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_notices(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut notices = Vec::new();
        let line = |text: String, cx: &mut Context<Self>| {
            h_flex()
                .w_full()
                .px_5()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new(text).size(LabelSize::Small).color(Color::Muted))
        };
        if let Some(service) = &self.service {
            let service = service.read(cx);
            if service.suggestions_paused().is_some() || service.any_suggestion_failed() {
                let reason = service
                    .suggestions_paused()
                    .map(|reason| format!("Suggestions paused · {reason}"))
                    .unwrap_or_else(|| "Suggestions paused".to_string());
                notices.push(
                    line(reason, cx)
                        .child(
                            Button::new("inbox-retry-suggestions", "Retry")
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(service) = this.service.clone() {
                                        service.update(cx, |service, cx| {
                                            service.retry_suggestions(cx)
                                        });
                                    }
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        if let Some(status) = &self.status {
            notices.push(
                line(status.to_string(), cx)
                    .when(status.contains("Already archived"), |this| {
                        this.child(
                            Button::new("inbox-open-folder", "Open the folder")
                                .label_size(LabelSize::Small)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(
                                        inbox_service::RevealInboxFolder.boxed_clone(),
                                        cx,
                                    );
                                }),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        IconButton::new("inbox-dismiss-status", IconName::Close)
                            .icon_size(IconSize::XSmall)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.status = None;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        notices
    }

    /// The row's suggestion (§7.4): an arrow and the destination, so it reads
    /// as where the note could go rather than a state it's in. One quiet colour
    /// at either tier, since the bar says which, and nothing below the line. A
    /// failed request shows a warning glyph with the reason (§7.3).
    fn render_chip(&self, entry: &InboxEntry, cx: &App) -> Option<AnyElement> {
        let service = self.service.as_ref()?.read(cx);
        if service.suggestion_availability(cx) != SuggestionAvailability::On {
            return None;
        }
        let suggestion_hint = |destination: Destination| {
            h_flex()
                .gap_0p5()
                .child(
                    Icon::new(IconName::ArrowRight)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    Label::new(destination.label())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .single_line(),
                )
                .into_any_element()
        };
        match service.suggestion_for(&entry.hash) {
            Some(suggestion) => match (tier(suggestion), suggestion.destination) {
                (Tier::Filled | Tier::Outlined, Some(destination)) => {
                    Some(suggestion_hint(destination))
                }
                _ => None,
            },
            None => service.suggestion_failure(&entry.hash).map(|reason| {
                div()
                    .id(SharedString::from(format!("inbox-failed-{}", entry.hash)))
                    .child(
                        Icon::new(IconName::Warning)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .tooltip(Tooltip::text(reason.clone()))
                    .into_any_element()
            }),
        }
    }

    fn render_row(
        &self,
        section: usize,
        index: usize,
        entry: &InboxEntry,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let item = &entry.item;
        let position = self.position_of(section, index);
        let selected = self.selected_index() == Some(position);
        let marked = self.marked.contains(&item.stem);
        let list_focused = self.focus_handle.is_focused(window);
        let stem = item.stem.clone();
        let colors = cx.theme().colors();
        let (ring, hover_background) =
            (colors.text_accent.opacity(0.4), colors.ghost_element_hover);
        let selected_background = if list_focused {
            colors.ghost_element_selected
        } else {
            colors.ghost_element_selected.opacity(0.5)
        };
        // A ticket's number says more than the one address every ticket comes
        // from, and off the title it leaves both lines to the subject. The
        // source says little beside a sender, and every character here comes
        // out of a narrow column.
        let whole_title = whole_title(item);
        let title = whole_title.as_deref().unwrap_or(&item.title);
        let ticket = ticket_prefix(title);
        let title = ticket.map_or(title, |(_, subject)| subject);
        let origin = ticket
            .map(|(number, _)| number.to_string())
            .or_else(|| item.from.as_deref().map(short_sender))
            .or_else(|| item.source.as_deref().map(source_label))
            .unwrap_or_default();
        let age = age_label(item.captured.as_deref(), Utc::now());
        let chip = self.render_chip(entry, cx).map(|chip| {
            div()
                .debug_selector(|| format!("INBOX_CHIP-{position}"))
                .flex_none()
                .child(chip)
                .into_any_element()
        });
        let image = item.has_image.then(|| {
            Icon::new(IconName::Image)
                .size(IconSize::XSmall)
                .color(Color::Muted)
                .into_any_element()
        });
        let extras = image.into_iter().chain(chip);
        let line_height =
            |size: TextSize| size.rems(cx).to_pixels(window.rem_size()) * ROW_LINE_HEIGHT;
        let title_line = line_height(TextSize::Default);
        let meta_line = line_height(TextSize::Small);
        let row = v_flex()
            .id(SharedString::from(format!("inbox-row-{stem}")))
            .size_full()
            .justify_center()
            .gap_0p5()
            .pl_4()
            .pr_5()
            .rounded_lg()
            .border_1()
            .border_color(if selected && list_focused {
                ring
            } else {
                transparent_black()
            })
            .cursor_pointer()
            .when(selected, |this| this.bg(selected_background))
            .when(!selected, |this| {
                this.hover(|style| style.bg(hover_background))
            })
            .on_click(cx.listener({
                let stem = stem.clone();
                move |this, event: &ClickEvent, window, cx| {
                    let position = this
                        .visible
                        .iter()
                        .position(|entry| entry.item.stem == stem);
                    this.select_position(position, cx);
                    if event.click_count() == 2 {
                        this.open_selected(window, cx);
                    } else {
                        this.focus_handle.focus(window, cx);
                    }
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let position = this
                        .visible
                        .iter()
                        .position(|entry| entry.item.stem == stem);
                    this.select_position(position, cx);
                    this.deploy_context_menu(event.position, window, cx);
                }),
            )
            .child(
                div()
                    .debug_selector(|| format!("INBOX_TITLE-{position}"))
                    .relative()
                    .w_full()
                    .line_height(relative(ROW_LINE_HEIGHT))
                    .when(marked, |this| {
                        this.child(
                            h_flex()
                                .debug_selector(|| format!("INBOX_CHECK-{position}"))
                                .absolute()
                                .left(MARK_BADGE_INSET)
                                .top_0()
                                .h(title_line)
                                .child(
                                    h_flex()
                                        .size(MARK_BADGE_SIZE)
                                        .justify_center()
                                        .rounded_full()
                                        .bg(colors.text_accent)
                                        .child(
                                            Icon::new(IconName::Check)
                                                .size(IconSize::Custom(rems(0.5625)))
                                                .color(Color::Custom(colors.editor_background)),
                                        ),
                                ),
                        )
                    })
                    .child(Label::new(title.to_string()).line_clamp(2)),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .line_height(relative(ROW_LINE_HEIGHT))
                    .child(
                        // A ticket's number gives way only to the age: what's
                        // beside it goes whole first. A name gives way to both.
                        div()
                            .debug_selector(|| format!("INBOX_ORIGIN-{position}"))
                            .min_w_0()
                            .when(ticket.is_none(), |this| this.flex_1())
                            .child(
                                Label::new(origin)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                    )
                    .map(|this| {
                        if ticket.is_some() {
                            // One line tall, and the empty first item leaves
                            // what doesn't fit to wrap onto the clipped second.
                            this.child(
                                h_flex()
                                    .debug_selector(|| format!("INBOX_ROW_EXTRAS-{position}"))
                                    .flex_1()
                                    .min_w_0()
                                    .h(meta_line)
                                    .flex_wrap()
                                    .justify_end()
                                    .content_start()
                                    .gap_x_1()
                                    .overflow_hidden()
                                    .child(div().w(px(0.)).h(meta_line))
                                    .children(extras),
                            )
                        } else {
                            this.children(extras)
                        }
                    })
                    .child(
                        h_flex()
                            .debug_selector(|| format!("INBOX_AGE-{position}"))
                            .flex_none()
                            .min_w(AGE_COLUMN_WIDTH)
                            .justify_end()
                            .children(age.map(|age| {
                                Label::new(age)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .single_line()
                            })),
                    ),
            );
        div()
            .debug_selector(|| format!("INBOX_ROW-{position}"))
            .h(ROW_HEIGHT)
            .w_full()
            .py(ROW_GAP)
            .child(row)
            .into_any_element()
    }

    /// **Carried over**'s quiet divider row, laid out at a row's height so
    /// `uniform_list` can give every row the same slot.
    fn render_divider(&self, cx: &App) -> AnyElement {
        v_flex()
            .debug_selector(|| "INBOX_DIVIDER".to_string())
            .h(ROW_HEIGHT)
            .w_full()
            .justify_end()
            .pb_2()
            .pl_4()
            .pr_5()
            .child(
                h_flex()
                    .gap_2p5()
                    .child(
                        Label::new("Carried over")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(1.))
                            .bg(cx.theme().colors().border_variant),
                    ),
            )
            .into_any_element()
    }

    fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.visible.is_empty() {
            let text = if self.filter.is_empty() {
                "Nothing waiting. Captures land here from your phone, Google Tasks and labelled email."
            } else {
                "Nothing matches the filter."
            };
            return v_flex()
                .size_full()
                .px_5()
                .py_4()
                .child(Label::new(text).size(LabelSize::Small).color(Color::Muted))
                .into_any_element();
        }
        let layout = Rc::new(self.layout());
        sectioned_list("inbox-list", layout, cx)
            .size_full()
            .px_1()
            .py_2()
            .track_scroll(&self.scroll)
            .into_any_element()
    }

    /// Whether the list's verbs answer the keyboard right now. Their keycaps
    /// stay on screen either way, dimmed while they don't.
    fn list_keys_live(&self, window: &Window, cx: &App) -> bool {
        !self.reader_focus.is_focused(window)
            && !self.filter_editor.focus_handle(cx).is_focused(window)
    }

    /// An action's keycap as `context` binds it. Asking the focus handle would
    /// answer for whichever pane holds focus, and the list's keys would drop
    /// off the bar once the reader's context reached the frame.
    fn keycap(
        &self,
        action: &dyn Action,
        context: KeyContext,
        window: &Window,
        cx: &App,
    ) -> KeyBinding {
        window
            .highest_precedence_binding_for_action_in_context(action, context)
            .map(|binding| KeyBinding::from_keystrokes(binding.keystrokes().into(), vim_mode(cx)))
            .unwrap_or_else(|| KeyBinding::for_action_in(action, &self.focus_handle, cx))
    }

    /// The keycap the footer shows for moving one line: `k`/`j` when they're
    /// bound, else the arrows. The highest-precedence binding would be the
    /// shared menu's `ctrl-p`/`ctrl-n`, and vim binds the row keys to its own
    /// actions, so both are searched by name.
    fn move_keycap(
        &self,
        action_names: &[&str],
        preferred: [&str; 2],
        fallback: &dyn Action,
        context: KeyContext,
        window: &Window,
        cx: &App,
    ) -> KeyBinding {
        let bindings: Vec<gpui::KeyBinding> = action_names
            .iter()
            .filter_map(|name| cx.build_action(name, None).ok())
            .flat_map(|action| {
                window.bindings_for_action_in_context(action.as_ref(), context.clone())
            })
            .collect();
        preferred
            .into_iter()
            .find_map(|key| {
                bindings.iter().find(|binding| {
                    matches!(binding.keystrokes(), [keystroke]
                        if keystroke.key() == key && !keystroke.modifiers().modified())
                })
            })
            .map(|binding| KeyBinding::from_keystrokes(binding.keystrokes().into(), vim_mode(cx)))
            .unwrap_or_else(|| self.keycap(fallback, context, window, cx))
    }

    /// The footer's hints in the order they give way, and the one that stays
    /// whatever the width: the way to the rest of the keys, or out of the
    /// reader.
    fn footer_hints(&self, window: &Window, cx: &App) -> (Vec<FooterHint>, FooterHint) {
        if self.reader_focus.is_focused(window) {
            let context = reader_key_context();
            let scroll = vec![
                self.move_keycap(
                    &["thock::ScrollInboxReaderUp"],
                    ["k", "up"],
                    &ScrollInboxReaderUp,
                    context.clone(),
                    window,
                    cx,
                ),
                self.move_keycap(
                    &["thock::ScrollInboxReaderDown"],
                    ["j", "down"],
                    &ScrollInboxReaderDown,
                    context.clone(),
                    window,
                    cx,
                ),
            ];
            let mut hints = vec![FooterHint::new("scroll", scroll)];
            if self.reader_note.thread.is_some() {
                let key =
                    |action: &dyn Action| vec![self.keycap(action, context.clone(), window, cx)];
                hints.push(FooterHint::new("next", key(&NextInboxMessage)));
                hints.push(FooterHint::new("folds", key(&ToggleInboxFolds)));
            }
            let list = self.keycap(&FocusInboxReader, context, window, cx);
            return (hints, FooterHint::new("list", vec![list]));
        }
        let context = list_key_context();
        let dimmed = !self.list_keys_live(window, cx);
        let key = |action: &dyn Action| vec![self.keycap(action, context.clone(), window, cx)];
        let keys = FooterHint::new("keys", key(&ShowInboxKeys)).dimmed(dimmed);
        if self.visible.is_empty() {
            return (Vec::new(), keys);
        }
        (
            vec![
                FooterHint::new("move", self.row_keycaps(window, cx)).dimmed(dimmed),
                FooterHint::new("mark", key(&MarkInboxItem)).dimmed(dimmed),
                FooterHint::new("filter", key(&FilterInbox)).dimmed(dimmed),
            ],
            keys,
        )
    }

    /// The two keys that step through the list.
    fn row_keycaps(&self, window: &Window, cx: &App) -> Vec<KeyBinding> {
        let context = list_key_context();
        vec![
            self.move_keycap(
                &["vim::MenuSelectPrevious", "menu::SelectPrevious"],
                ["k", "up"],
                &SelectPrevious,
                context.clone(),
                window,
                cx,
            ),
            self.move_keycap(
                &["vim::MenuSelectNext", "menu::SelectNext"],
                ["j", "down"],
                &SelectNext,
                context,
                window,
                cx,
            ),
        ]
    }

    /// Hints give way whole. They sit in a wrapping row one line tall, so the
    /// first that doesn't fit drops to a clipped second line and takes the
    /// ones after it along; only the last hint stays put.
    fn render_list_footer(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let hint = |hint: FooterHint, cx: &App| {
            let label = hint.label;
            let dimmed = hint.dimmed;
            h_flex()
                .debug_selector(move || format!("INBOX_HINT-{label}"))
                .flex_none()
                .h(FOOTER_HEIGHT)
                .gap_1()
                .children(
                    hint.keycaps
                        .into_iter()
                        .map(|keycap| framed_keycap(keycap, dimmed, cx)),
                )
                .child(
                    Label::new(label)
                        .size(LabelSize::Small)
                        .color(hint_color(dimmed)),
                )
        };
        let (hints, pinned_hint) = self.footer_hints(window, cx);
        let stats = match self.footer_stats(cx) {
            FooterStats::PlusOffer => Some(
                h_flex()
                    .gap_1()
                    .child(
                        Label::new("Suggestions come with Thock Plus")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .single_line(),
                    )
                    .child(
                        IconButton::new("inbox-dismiss-plus", IconName::Close)
                            .icon_size(IconSize::XSmall)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.plus_banner_dismissed = true;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            ),
            FooterStats::Stats(text) if text.is_empty() => None,
            FooterStats::Stats(text) => Some(
                Label::new(text)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .single_line()
                    .into_any_element(),
            ),
        };
        let opens_keys = pinned_hint.label == "keys";
        let hints: Vec<_> = hints.into_iter().map(|h| hint(h, cx)).collect();
        let pinned = hint(pinned_hint, cx);
        h_flex()
            .flex_none()
            .h(FOOTER_HEIGHT)
            .w_full()
            .items_start()
            .px_5()
            .gap_3p5()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex_wrap()
                    .content_start()
                    .overflow_hidden()
                    .gap_x_3p5()
                    .children(hints)
                    .children(stats.map(|stats| {
                        h_flex()
                            .debug_selector(|| "INBOX_STATS".to_string())
                            .flex_none()
                            .h(FOOTER_HEIGHT)
                            .ml_auto()
                            .child(stats)
                    })),
            )
            .child(
                div()
                    .id("inbox-pinned-hint")
                    .flex_none()
                    .child(pinned)
                    .when(opens_keys, |this| {
                        this.cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_keys(&ShowInboxKeys, window, cx);
                            }))
                    }),
            )
            .into_any_element()
    }

    fn markdown_style(&self, window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        style.base_text_style.font_size = px(15.).into();
        style.base_text_style.line_height = relative(1.65);
        style
    }

    /// The body of an email, smaller than a note's: it is scanned, not
    /// studied.
    fn reader_style(&self, window: &Window, cx: &App) -> ReaderStyle {
        let mut body = self.markdown_style(window, cx);
        if self.reader_note.thread.is_some() {
            body.base_text_style.font_size = px(14.).into();
            body.base_text_style.line_height = relative(1.5);
        }
        let mut folded = body.clone();
        folded.base_text_style.color = cx.theme().colors().text_muted;
        let mut system = folded.clone();
        // Markdown text takes its size from the container and its leading from
        // the paragraph; the base style alone reaches neither.
        system.container_style.text.font_size = Some(SYSTEM_LINE_SIZE.into());
        system.paragraph_line_height = SYSTEM_LINE_HEIGHT.into();
        system.paragraph_spacing = px(4.);
        if cx.theme().appearance().is_light() {
            system.base_text_style.color = cx
                .theme()
                .colors()
                .text_muted
                .opacity(SYSTEM_LINE_OPACITY_LIGHT);
        }
        ReaderStyle {
            body,
            folded,
            system,
            images: self.image_resolver(cx),
        }
    }

    /// Brings the reader in step with the selected note.
    fn sync_reader_note(&mut self) {
        let Some(entry) = self.selected_entry() else {
            self.reader_note = ReaderNote::default();
            return;
        };
        if self.reader_note.hash.as_ref() == Some(&entry.hash) {
            return;
        }
        let item = &entry.item;
        let is_email = item.source.as_deref() == Some("gmail");
        let body = without_leading_title(&item.body, &item.title);
        let title = whole_title(item);
        let thread = is_email.then(|| {
            let subject = title.as_deref().unwrap_or(&item.title);
            let mut thread = parse_thread(body, item.from.as_deref());
            drop_restated_subject(&mut thread, subject);
            quiet_system_lines(&mut thread, subject);
            Rc::new(thread)
        });
        let current_message = thread
            .as_ref()
            .map_or(0, |thread| thread.messages.len().saturating_sub(1));
        self.reader_note = ReaderNote {
            hash: Some(entry.hash.clone()),
            thread,
            title,
            current_message,
            reveal_current: true,
            ..ReaderNote::default()
        };
        self.reader_scroll.set_offset(point(px(0.), px(0.)));
    }

    fn render_markdown(
        &mut self,
        key: (usize, usize),
        text: &str,
        style: MarkdownStyle,
        images: &Rc<dyn Fn(&str) -> Option<ImageSource>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let markdown = self
            .reader_note
            .markdown
            .entry(key)
            .or_insert_with(|| {
                let source: SharedString = text.to_string().into();
                cx.new(|cx| Markdown::new(source, None, None, cx))
            })
            .clone();
        let images = images.clone();
        MarkdownElement::new(markdown, style)
            .image_resolver(move |src| images(src))
            .into_any_element()
    }

    /// A card's fold row: the way to every fold of the note, and what this
    /// card holds of them. It closes the card, under whatever stayed on screen,
    /// so it sits in the same place on every card; open, it also heads the
    /// card, so a long one needn't be read through to close it.
    fn render_fold_row(
        &self,
        message: usize,
        heads_card: bool,
        label: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.reader_note.folds_open;
        let hover = cx.theme().colors().element_hover;
        let keycap = framed_keycap(
            self.keycap(&ToggleInboxFolds, list_key_context(), window, cx),
            false,
            cx,
        );
        let place = if heads_card { "TOP" } else { "END" };
        h_flex()
            .id(SharedString::from(format!("inbox-fold-{place}-{message}")))
            .debug_selector(move || {
                if heads_card {
                    format!("INBOX_FOLD_TOP-{message}")
                } else {
                    format!("INBOX_FOLD-{message}")
                }
            })
            .w_full()
            .gap_1p5()
            .px_1()
            .py_0p5()
            .rounded_md()
            .hover(|style| style.bg(hover))
            .cursor_pointer()
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(keycap)
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(label)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                ),
            )
            .tooltip(Tooltip::for_action_title(
                if open {
                    "Hide all folds"
                } else {
                    "Show all folds"
                },
                &ToggleInboxFolds,
            ))
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_folds(&ToggleInboxFolds, window, cx);
            }))
            .into_any_element()
    }

    /// A record as a grid: as many columns as the reader has room for, a
    /// field to a line. What was folded reads dimmed, behind the people's
    /// words. Values stay plain so the message leads: accent is for a real
    /// link. A value holds one line until `z` opens it whole.
    fn render_fields(&self, id: &str, fields: &[&EmailField], dim: bool, cx: &App) -> AnyElement {
        let background = cx.theme().colors().element_background;
        let whole = self.reader_note.folds_open;
        h_flex()
            .w_full()
            .flex_wrap()
            .items_start()
            .gap_x_6()
            .gap_y_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(background)
            .children(fields.iter().enumerate().map(|(index, field)| {
                let text = field_text(&field.value);
                let (shown, more) = field_list(text, whole);
                let value =
                    Label::new(shown)
                        .size(LabelSize::Small)
                        .color(if field.link.is_some() {
                            Color::Accent
                        } else if dim {
                            Color::Muted
                        } else {
                            Color::Default
                        });
                let value = if whole { value } else { value.truncate() };
                let selector = format!("{id}-{index}");
                let previous = field
                    .previous
                    .as_ref()
                    .filter(|previous| whole || previous_helps(previous));
                let mut value_cell = h_flex()
                    .id(SharedString::from(format!("inbox-field-{selector}")))
                    .flex_1()
                    .min_w_0()
                    .when(whole, |this| this.flex_wrap())
                    .child(div().min_w_0().child(value))
                    .when(more > 0, |this| {
                        let selector = selector.clone();
                        this.child(
                            div()
                                .debug_selector(move || format!("INBOX_MORE-{selector}"))
                                .flex_none()
                                .ml_1p5()
                                .child(
                                    Label::new(format!("+{more} more"))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .single_line(),
                                ),
                        )
                    })
                    .when_some(previous, |this, previous| {
                        let selector = selector.clone();
                        this.child(
                            div()
                                .debug_selector(move || format!("INBOX_CHANGED-{selector}"))
                                .when(!whole, |this| this.flex_none())
                                .child(
                                    Label::new(format!(", was {}", was_text(previous, whole)))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                    });
                if let Some(link) = field.link.clone() {
                    value_cell = value_cell
                        .cursor_pointer()
                        .tooltip(Tooltip::text(link.clone()))
                        .on_click(move |_, _, cx| cx.open_url(&link));
                } else if more > 0 {
                    value_cell = value_cell.tooltip(Tooltip::text(text.to_string()));
                }
                h_flex()
                    .flex_grow_1()
                    .flex_basis(FIELD_MIN_WIDTH)
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .child(
                        div().flex_none().w(FIELD_KEY_WIDTH).child(
                            Label::new(field.key.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                    )
                    .child(value_cell)
            }))
            .into_any_element()
    }

    /// A message's blocks as drawn, then its fold row saying what they fold,
    /// however many places that came from. Opened, each folded stretch stands
    /// where it was, dimmed, and the row heads the card too.
    fn render_message_blocks(
        &mut self,
        index: usize,
        message: &EmailMessage,
        style: &ReaderStyle,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let open = self.reader_note.folds_open;
        let mut elements = Vec::new();
        let mut folds = Vec::new();
        for (block_index, block) in message.blocks.iter().enumerate() {
            let key = (index, block_index);
            match block {
                EmailBlock::Text(text) => {
                    elements.push(self.render_markdown(
                        key,
                        text,
                        style.body.clone(),
                        &style.images,
                        cx,
                    ));
                }
                EmailBlock::Meta(text) => {
                    elements.push(self.render_markdown(
                        key,
                        text,
                        style.system.clone(),
                        &style.images,
                        cx,
                    ));
                }
                EmailBlock::Fields { shown, unchanged } => {
                    let id = format!("{index}-{block_index}");
                    let (visible, folded) = split_fields(shown, unchanged);
                    if !visible.is_empty() {
                        elements.push(self.render_fields(&id, &visible, false, cx));
                    }
                    if !folded.is_empty() {
                        folds.push(FoldPiece::Fields(folded.len()));
                        if open {
                            elements.push(self.render_fields(
                                &format!("{id}-same"),
                                &folded,
                                true,
                                cx,
                            ));
                        }
                    }
                }
                EmailBlock::Fold { kind, text, lines } => {
                    folds.push(FoldPiece::Text(*kind, *lines));
                    if open {
                        let text = self.render_markdown(
                            key,
                            text,
                            style.folded.clone(),
                            &style.images,
                            cx,
                        );
                        elements.push(
                            div()
                                .pl_3()
                                .border_l_2()
                                .border_color(cx.theme().colors().border_variant)
                                .child(text)
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        if !folds.is_empty() {
            let label = fold_summary(&folds, open);
            if open {
                elements.insert(
                    0,
                    self.render_fold_row(index, true, label.clone(), window, cx),
                );
            }
            elements.push(self.render_fold_row(index, false, label, window, cx));
        }
        elements
    }

    /// One message: a neutral card headed by who wrote it, with their colour
    /// on the badge alone, so a reply's start can't be missed and the words
    /// keep the most weight. A lone message has only its blocks, since the header says the rest.
    /// While the folds are closed only the current message is open; the rest
    /// are their header, with a snippet where the address would be.
    fn render_message(
        &mut self,
        index: usize,
        message: &EmailMessage,
        total: usize,
        style: &ReaderStyle,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = index == self.reader_note.current_message;
        let folds_open = self.reader_note.folds_open;
        let open = total < 2 || current || folds_open;
        let blocks = if open {
            self.render_message_blocks(index, message, style, window, cx)
        } else {
            Vec::new()
        };
        if total < 2 {
            return v_flex()
                .w_full()
                .max_w(READER_MAX_WIDTH)
                .gap_3()
                .children(blocks)
                .into_any_element();
        }
        let colors = cx.theme().colors();
        let accent = sender_color(message.participant, cx.theme().appearance().is_light());
        let sender = sender_name(&message.from);
        let header_from = self
            .selected_entry()
            .and_then(|entry| entry.item.from.as_deref());
        let (name, detail) = match &message.author {
            Some(author) => (author.clone(), relayed_via(&sender, header_from)),
            None => (sender, sender_address(&message.from)),
        };
        let detail = if open {
            detail
        } else {
            snippet(message).or(detail)
        };
        let when = message
            .sent
            .map(|sent| sent.format("%b %-d, %H:%M").to_string());
        let header = h_flex()
            .w_full()
            .gap_2()
            .pl_4()
            .pr_3()
            .py_2()
            .when(open, |this| {
                this.border_b_1().border_color(colors.border_variant)
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(SENDER_BADGE_SIZE)
                    .rounded_full()
                    .bg(accent.opacity(0.22))
                    .child(
                        Label::new(initials(&name))
                            .size(LabelSize::XSmall)
                            .weight(FontWeight::SEMIBOLD)
                            .color(Color::Custom(accent)),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .child(Label::new(name).weight(FontWeight::SEMIBOLD).truncate()),
            )
            .child(div().flex_1().min_w_0().children(detail.map(|detail| {
                Label::new(detail)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .truncate()
            })))
            .children(when.map(|when| {
                div().flex_none().child(
                    Label::new(when)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .single_line(),
                )
            }))
            .child(
                div()
                    .debug_selector(move || format!("INBOX_POSITION-{index}"))
                    .flex_none()
                    .pl_2()
                    .child(
                        Label::new(format!("{} of {total}", index + 1))
                            .size(LabelSize::XSmall)
                            .color(if current {
                                Color::Default
                            } else {
                                Color::Muted
                            })
                            .single_line(),
                    ),
            );
        // With every card open, the ring is what says which one `n` left on.
        let border = if current && folds_open {
            colors.text_accent.opacity(0.4)
        } else {
            colors.border_variant
        };
        v_flex()
            .id(SharedString::from(format!("inbox-message-{index}")))
            .debug_selector(move || format!("INBOX_MESSAGE-{index}"))
            .flex_none()
            .w_full()
            .max_w(READER_MAX_WIDTH)
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(border)
            .bg(colors.panel_background)
            .child(header)
            .when(open, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .gap_3()
                        .pl_4()
                        .pr_4()
                        .py_3()
                        .children(blocks),
                )
            })
            .when(!open, |this| {
                this.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.open_message(index, cx)))
            })
            .into_any_element()
    }

    /// The note's body as the reader's children, one per message.
    fn render_note_body(
        &mut self,
        entry: &InboxEntry,
        style: &ReaderStyle,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(thread) = self.reader_note.thread.clone() else {
            let body = without_leading_title(&entry.item.body, &entry.item.title);
            let body = self.render_markdown((0, 0), body, style.body.clone(), &style.images, cx);
            return vec![
                div()
                    .w_full()
                    .max_w(READER_MAX_WIDTH)
                    .child(body)
                    .into_any_element(),
            ];
        };
        let total = thread.messages.len();
        let preamble = (!thread.preamble.is_empty()).then(|| {
            // Past the messages' keys, which count from zero.
            let key = (total, 0);
            let preamble =
                self.render_markdown(key, &thread.preamble, style.body.clone(), &style.images, cx);
            div()
                .w_full()
                .max_w(READER_MAX_WIDTH)
                .child(preamble)
                .into_any_element()
        });
        preamble
            .into_iter()
            .chain(thread.messages.iter().enumerate().map(|(index, message)| {
                self.render_message(index, message, total, style, window, cx)
            }))
            .collect()
    }

    fn render_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.keys_sheet {
            return div()
                .relative()
                .size_full()
                .child(self.render_keys_sheet(window, cx))
                .into_any_element();
        }
        // Selection follows the list, so this is an empty list. A filter's
        // says why itself, and what it hides isn't clear.
        let Some(entry) = self.selected_entry().cloned() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .when(self.filter.is_empty(), |this| {
                    this.child(
                        div()
                            .debug_selector(|| "INBOX_ALL_CLEAR".to_string())
                            .child(Label::new("All clear").color(Color::Muted)),
                    )
                })
                .into_any_element();
        };
        let style = self.reader_style(window, cx);
        let body = self.render_note_body(&entry, &style, window, cx);
        let focused = self.reader_focus.is_focused(window);
        let colors = cx.theme().colors();
        let (rule, focus_edge) = (colors.border_variant, colors.border_focused);
        let marked_rows = self
            .visible
            .iter()
            .filter(|entry| self.marked.contains(&entry.item.stem))
            .count();
        let bar_state = self.bar_state(&entry, cx);
        let percentages = bar_state.probabilities.is_some();
        let caption = if marked_rows > 0 {
            Some((
                "INBOX_MARKS_NOTE",
                marks_note(marked_rows, percentages),
                Color::Accent,
            ))
        } else {
            bar_caption(&bar_state).map(|text| ("INBOX_SUGGESTED", text, Color::Muted))
        };
        let earlier = self.reader_note.messages_above;
        let earlier_cue = (earlier > 0).then(|| self.render_earlier_cue(earlier, window, cx));
        v_flex()
            .relative()
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .w_full()
                    .px_10()
                    .pt_5()
                    .pb_3()
                    .border_b_1()
                    .border_color(rule)
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(READER_MAX_WIDTH)
                            .gap_1()
                            // A fixed block and a fixed slot: whatever the
                            // title's length and the caption, the four targets
                            // stay where the hand expects them.
                            .child(
                                div()
                                    .debug_selector(|| "INBOX_READER_TITLE".to_string())
                                    .flex_none()
                                    .h(rems(TITLE_SIZE.rems().0 * TITLE_LINE_HEIGHT * 2.))
                                    .line_height(relative(TITLE_LINE_HEIGHT))
                                    .child(
                                        Label::new(
                                            self.reader_note
                                                .title
                                                .clone()
                                                .unwrap_or_else(|| entry.item.title.clone()),
                                        )
                                        .size(LabelSize::Custom(TITLE_SIZE.rems()))
                                        .weight(FontWeight::SEMIBOLD)
                                        .line_clamp(2),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .h(CAPTION_SLOT_HEIGHT)
                                    .children(caption.map(|(selector, text, color)| {
                                        div().debug_selector(|| selector.to_string()).child(
                                            Label::new(text)
                                                .size(LabelSize::Small)
                                                .color(color)
                                                .truncate(),
                                        )
                                    })),
                            )
                            .child(self.render_destination_bar(&entry, window, cx)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        v_flex()
                            .id("inbox-reader")
                            .size_full()
                            .track_focus(&self.reader_focus)
                            .overflow_y_scroll()
                            .track_scroll(&self.reader_scroll)
                            .on_scroll_wheel(cx.listener(|_, _: &ScrollWheelEvent, _, cx| {
                                cx.notify();
                            }))
                            .px_10()
                            .py_4()
                            .gap_2()
                            .children(body),
                    )
                    .children(earlier_cue),
            )
            // Drawn over the pane rather than beside it, so nothing moves
            // when focus does.
            .when(focused, |this| {
                this.child(
                    div()
                        .debug_selector(|| "INBOX_READER_FOCUS".to_string())
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(FOCUS_RAIL_WIDTH)
                        .bg(focus_edge),
                )
            })
            .into_any_element()
    }

    /// How many of the thread's cards have their header above the cue line,
    /// by the last frame's layout, so a card still running on below the cue
    /// counts once its header has gone.
    fn messages_above(&self) -> usize {
        let Some(thread) = self
            .reader_note
            .thread
            .as_ref()
            .filter(|thread| thread.messages.len() > 1)
        else {
            return 0;
        };
        let scrolled = -self.reader_scroll.offset().y;
        if scrolled <= px(0.) {
            return 0;
        }
        let preamble = usize::from(!thread.preamble.is_empty());
        let cue_line = self.reader_scroll.bounds().top() + scrolled + EARLIER_CUE_HEIGHT;
        (0..thread.messages.len())
            .take_while(|index| {
                self.reader_scroll
                    .bounds_for_item(index + preamble)
                    .is_some_and(|card| card.top() < cue_line)
            })
            .count()
    }

    /// Brings the cue up to the frame just painted. Layout can move the scroll
    /// after render has read it (a taller reader clamps the offset, a parsed
    /// body grows a card), and only the finished frame knows what's above.
    fn sync_earlier_cue(&mut self, cx: &mut Context<Self>) {
        let above = self.messages_above();
        if above != self.reader_note.messages_above {
            self.reader_note.messages_above = above;
            cx.notify();
        }
    }

    /// The header reads the width of the frame just painted, so a resize
    /// reaches it one frame on. Nothing has measured the pane before its first
    /// note.
    fn sync_header_fit(&mut self, window: &Window, cx: &mut Context<Self>) {
        let width = self.reader_scroll.bounds().size.width;
        if width == px(0.) {
            return;
        }
        let fit = HeaderFit::for_width(width / window.rem_size());
        if fit != self.header_fit {
            self.header_fit = fit;
            cx.notify();
        }
    }

    /// Solid, so a card scrolled half out of view ends at a clean edge, and
    /// it says what's up there and the key that goes there.
    fn render_earlier_cue(
        &self,
        earlier: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let keycap = framed_keycap(
            self.keycap(&PreviousInboxMessage, list_key_context(), window, cx),
            false,
            cx,
        );
        h_flex()
            .id("inbox-earlier")
            .debug_selector(|| "INBOX_EARLIER".to_string())
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(EARLIER_CUE_HEIGHT)
            .px_10()
            .gap_1p5()
            .bg(colors.editor_background)
            .border_b_1()
            .border_color(colors.border_variant)
            .cursor_pointer()
            .child(
                Icon::new(IconName::ArrowUp)
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
            )
            .child(
                Label::new(earlier_label(earlier))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(keycap)
            .on_click(cx.listener(|this, _, _, cx| {
                this.reader_scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }))
            .into_any_element()
    }

    /// V39 §4.3's rule for the reader: vault-root paths and the images
    /// folder resolve against the root, bare relative paths against the
    /// inbox folder, web pictures are fetched.
    fn image_resolver(&self, cx: &App) -> Rc<dyn Fn(&str) -> Option<ImageSource>> {
        let vault = self.vault(cx);
        let inbox_dir = self
            .service
            .as_ref()
            .map(|service| service.read(cx).inbox_dir())
            .unwrap_or_else(|| "inbox".to_string());
        Rc::new(move |src: &str| {
            if src.starts_with("http://") || src.starts_with("https://") {
                return Some(ImageSource::Resource(Resource::Uri(src.to_string().into())));
            }
            let vault = vault.as_ref()?;
            let note_dir = RelPath::new(Path::new(&inbox_dir), PathStyle::Unix).ok()?;
            let relative = vault_image_path(src, &note_dir, &vault.config.images.dir)?;
            let path = vault.root.join(relative.as_std_path());
            Some(ImageSource::Resource(Resource::Path(Arc::from(path))))
        })
    }

    /// One of the destination bar's buttons: a rounded box in the look's
    /// colours around a `ButtonLike` that dispatches `action` from the view,
    /// so a click takes the same path as the key.
    fn bar_button(
        &self,
        name: &'static str,
        look: BarLook,
        keys_off: bool,
        action: Box<dyn Action>,
        content: impl IntoElement,
        cx: &App,
    ) -> gpui::Div {
        let (background, border, hover) = bar_colors(look, keys_off, cx);
        let focus = self.focus_handle.clone();
        let button = ButtonLike::new(SharedString::from(format!("inbox-bar-{name}")))
            .style(ButtonStyle::Transparent)
            .size(ButtonSize::None)
            .height(relative(1.))
            .full_width()
            .on_click(move |_, window, cx| focus.dispatch_action(action.as_ref(), window, cx))
            .child(content);
        div()
            .debug_selector(move || format!("INBOX_BAR-{name}"))
            .h(BAR_BUTTON_HEIGHT)
            .flex_1()
            .min_w(DESTINATION_MIN_WIDTH)
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(background)
            .hover(|style| style.bg(hover))
            .overflow_hidden()
            .child(button)
    }

    fn destination_button(
        &self,
        destination: Destination,
        look: BarLook,
        probability: Option<f64>,
        window: &Window,
        cx: &App,
    ) -> gpui::Div {
        let suggested = matches!(look, BarLook::Filled | BarLook::Outlined);
        let colors = cx.theme().colors();
        // One accent meter in the bar, so the likeliest place reads before
        // any of the numbers do.
        let (track, fill) = (
            colors.text_muted.opacity(0.18),
            if look == BarLook::Plain {
                colors.text_muted
            } else {
                colors.text_accent
            },
        );
        let name = destination.key();
        let live = self.list_keys_live(window, cx);
        let keycap = |action: &dyn Action| {
            framed_keycap(
                self.keycap(action, list_key_context(), window, cx),
                !live,
                cx,
            )
        };
        // The name and the odds, the keys, then the meter on a line of its
        // own: the keys grow with the suggestion, so they share a line with
        // neither, and every button's meter keeps its full length.
        let content = v_flex()
            .w_full()
            .h_full()
            .justify_center()
            .px(px(10.))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_2()
                    // Tight, so the name's leading doesn't read as more room
                    // at the top of the button than under the meter.
                    .line_height(relative(1.2))
                    .child(
                        div()
                            .debug_selector(move || format!("INBOX_LABEL-{name}"))
                            .flex_none()
                            .child(
                                Label::new(destination.label())
                                    .color(if suggested {
                                        Color::Accent
                                    } else {
                                        Color::Default
                                    })
                                    .when(look == BarLook::Filled, |this| {
                                        this.weight(FontWeight::SEMIBOLD)
                                    })
                                    .single_line(),
                            ),
                    )
                    .when_some(probability, |this, probability| {
                        this.child(
                            div()
                                .debug_selector(move || format!("INBOX_PERCENT-{name}"))
                                .flex_none()
                                .child(
                                    Label::new(format!(
                                        "{}%",
                                        (probability * 100.0).round() as u32
                                    ))
                                    .size(LabelSize::Small)
                                    .color(
                                        if look == BarLook::Plain {
                                            Color::Muted
                                        } else {
                                            Color::Default
                                        },
                                    ),
                                ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .debug_selector(move || format!("INBOX_KEYS-{name}"))
                    .flex_none()
                    .mt_0p5()
                    .gap_1()
                    .when(suggested, |this| {
                        this.child(keycap(&AcceptInboxSuggestion))
                            .child(keycap_or(!live))
                    })
                    .child(keycap(destination_action(destination).as_ref())),
            )
            .when_some(probability, |this, probability| {
                this.child(
                    div()
                        .debug_selector(move || format!("INBOX_PROBABILITY-{name}"))
                        .flex_none()
                        .mt_1()
                        .w_full()
                        .h(px(3.))
                        .rounded_full()
                        .bg(track)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .w(relative(probability.clamp(0.0, 1.0) as f32))
                                .bg(fill),
                        ),
                )
            });
        self.bar_button(
            destination.key(),
            look,
            !live,
            destination_action(destination),
            content,
            cx,
        )
    }

    /// The destination bar (§4.2): one button per destination in a fixed
    /// order. A reader too narrow for one row wraps them two by two rather
    /// than squeeze a button.
    fn render_destination_bar(&self, entry: &InboxEntry, window: &Window, cx: &App) -> AnyElement {
        let state = self.bar_state(entry, cx);
        let buttons = Destination::ALL
            .into_iter()
            .enumerate()
            .map(|(position, destination)| {
                let look = match state.suggested {
                    Some((suggested, Tier::Filled)) if suggested == destination => BarLook::Filled,
                    Some((suggested, _)) if suggested == destination => BarLook::Outlined,
                    _ if state.leaning == Some(destination) => BarLook::Leaning,
                    _ => BarLook::Plain,
                };
                let probability = state
                    .probabilities
                    .and_then(|probabilities| probabilities.get(position).copied());
                self.destination_button(destination, look, probability, window, cx)
            });
        h_flex()
            .w_full()
            .flex_wrap()
            .gap(BAR_GAP)
            .children(buttons)
            .into_any_element()
    }

    fn render_keys_sheet(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let key = |action: &dyn Action| vec![self.keycap(action, list_key_context(), window, cx)];
        let groups = [
            (
                "Decide",
                vec![
                    (key(&AcceptInboxSuggestion), "Take the suggestion"),
                    (key(&SendInboxItemToToday), "Today"),
                    (key(&SendInboxItemToSoon), "Soon"),
                    (key(&SendInboxItemToSomeday), "Someday"),
                    (key(&ArchiveInboxItem), "Archive"),
                    (key(&UndoInboxMove), "Undo the last decision"),
                    (key(&MarkInboxItem), "Mark the row for a batch decision"),
                    (key(&SuggestInboxDestinations), "Ask for a suggestion again"),
                    (
                        key(&TriageInbox),
                        "Hand what is left to the Triage Inbox ritual",
                    ),
                ],
            ),
            (
                "Move",
                vec![
                    (self.row_keycaps(window, cx), "Up and down the list"),
                    (key(&FocusInboxReader), "Between the list and the note"),
                    (key(&FilterInbox), "Filter by title, sender, source"),
                    (key(&ShowInboxItemMenu), "The row menu"),
                    (key(&ShowInboxKeys), "This sheet"),
                ],
            ),
            (
                "Read",
                vec![
                    (key(&OpenInboxItem), "The note in the editor"),
                    (
                        key(&OpenInboxLink),
                        "The email in Gmail, or the note's link",
                    ),
                    (key(&NextInboxMessage), "Next message of the email"),
                    (key(&PreviousInboxMessage), "Previous message of the email"),
                    (
                        key(&ToggleInboxFolds),
                        "Show or hide what the email folded away",
                    ),
                ],
            ),
        ];
        v_flex()
            .size_full()
            .px_10()
            .pt_5()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .max_w(READER_MAX_WIDTH)
                    .flex_wrap()
                    .items_start()
                    .gap_x_8()
                    .gap_y_3()
                    .children(groups.into_iter().map(|(heading, rows)| {
                        v_flex()
                            .debug_selector(move || format!("INBOX_KEYS_GROUP-{heading}"))
                            .flex_1()
                            .min_w(KEYS_GROUP_MIN_WIDTH)
                            .gap_1()
                            .child(
                                div().pb_0p5().child(
                                    Label::new(heading)
                                        .size(LabelSize::Small)
                                        .weight(FontWeight::SEMIBOLD),
                                ),
                            )
                            .children(rows.into_iter().map(|(keycaps, what)| {
                                h_flex()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .flex_none()
                                            .min_w(KEYS_COLUMN_WIDTH)
                                            .gap_1()
                                            .children(
                                                keycaps
                                                    .into_iter()
                                                    .map(|keycap| framed_keycap(keycap, false, cx)),
                                            ),
                                    )
                                    .child(div().flex_1().min_w_0().child(
                                        Label::new(what).size(LabelSize::Small).color(Color::Muted),
                                    ))
                            }))
                    })),
            )
            .child(
                Label::new("Any key closes this.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element()
    }
}

/// One count in the list header.
#[derive(Debug, Clone, PartialEq)]
struct SectionCount {
    section: usize,
    label: &'static str,
    count: usize,
    active: bool,
}

/// What the destination bar shows for the selected row.
#[derive(Debug, Clone, Default, PartialEq)]
struct BarState {
    /// The highlighted destination and its tier; `space` sits on it.
    suggested: Option<(Destination, Tier)>,
    /// With nothing confident enough to highlight, the likeliest place. It
    /// gets a quiet cue and never a key.
    leaning: Option<Destination>,
    /// Each destination's probability in `Destination::ALL` order, whenever
    /// a suggestion exists.
    probabilities: Option<[f64; 4]>,
}

/// The right side of the list footer.
#[derive(Debug, Clone, PartialEq)]
enum FooterStats {
    /// *Suggestions come with Thock Plus*, until dismissed.
    PlusOffer,
    Stats(String),
}

/// A key hint in the list footer.
struct FooterHint {
    label: &'static str,
    keycaps: Vec<KeyBinding>,
    dimmed: bool,
}

impl FooterHint {
    fn new(label: &'static str, keycaps: Vec<KeyBinding>) -> Self {
        Self {
            label,
            keycaps,
            dimmed: false,
        }
    }

    fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }
}

/// How much the reader header can say at the pane's width.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum HeaderFit {
    /// No glyph, and the link goes by its short name.
    Tight,
    #[default]
    Plain,
    /// The sender's address joins their name.
    Roomy,
}

impl HeaderFit {
    fn for_width(rems: f32) -> Self {
        if rems < HEADER_TIGHT_WIDTH {
            Self::Tight
        } else if rems < HEADER_ROOMY_WIDTH {
            Self::Plain
        } else {
            Self::Roomy
        }
    }
}

/// One folded stretch of a message.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FoldPiece {
    Text(FoldKind, usize),
    Fields(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarLook {
    /// The suggestion at the filled tier.
    Filled,
    /// The suggestion at the outlined tier.
    Outlined,
    /// The likeliest destination, below the outlined tier.
    Leaning,
    Plain,
}

/// A button's background, border and hover. While its keys are off a suggested
/// button's outline dims with them.
fn bar_colors(look: BarLook, keys_off: bool, cx: &App) -> (Hsla, Hsla, Hsla) {
    let colors = cx.theme().colors();
    let accent = colors.text_accent;
    let outline = if keys_off {
        accent.opacity(DIMMED_OUTLINE_OPACITY)
    } else {
        accent
    };
    match look {
        BarLook::Filled => {
            let fill = if cx.theme().appearance().is_light() {
                FILLED_OPACITY_LIGHT
            } else {
                FILLED_OPACITY_DARK
            };
            (accent.opacity(fill), outline, accent.opacity(fill + 0.08))
        }
        BarLook::Outlined => (transparent_black(), outline, colors.ghost_element_hover),
        BarLook::Leaning => (
            colors.element_background,
            colors.border,
            colors.element_hover,
        ),
        BarLook::Plain => (
            colors.element_background,
            colors.border_variant,
            colors.element_hover,
        ),
    }
}

/// The one destination ahead of the others; a tie leans nowhere.
fn most_likely(probabilities: &[f64; 4]) -> Option<Destination> {
    let highest = probabilities.iter().copied().fold(0.0, f64::max);
    let mut ahead = Destination::ALL
        .into_iter()
        .zip(probabilities)
        .filter(|(_, probability)| **probability == highest);
    match (highest > 0.0, ahead.next(), ahead.next()) {
        (true, Some((destination, _)), None) => Some(destination),
        _ => None,
    }
}

fn list_key_context() -> KeyContext {
    let mut context = KeyContext::new_with_defaults();
    context.add(INBOX_VIEW_KEY);
    context.add("menu");
    context
}

fn reader_key_context() -> KeyContext {
    let mut context = list_key_context();
    context.add("reader");
    context
}

fn vim_mode(cx: &App) -> bool {
    cx.global::<SettingsStore>()
        .merged_settings()
        .vim_mode
        .unwrap_or(false)
}

fn destination_action(destination: Destination) -> Box<dyn Action> {
    match destination {
        Destination::Today => SendInboxItemToToday.boxed_clone(),
        Destination::Soon => SendInboxItemToSoon.boxed_clone(),
        Destination::Someday => SendInboxItemToSomeday.boxed_clone(),
        Destination::Archive => ArchiveInboxItem.boxed_clone(),
    }
}

impl SectionedListDelegate for InboxView {
    fn sections(&self) -> usize {
        2
    }

    fn items_in(&self, section: usize) -> usize {
        if section == 0 {
            self.new_count
        } else {
            self.carried_count()
        }
    }

    /// **New** is named by the list header, so only **Carried over** gets a
    /// row of its own.
    fn has_header(&self, section: usize) -> bool {
        section != 0
    }

    fn render_header(
        &mut self,
        _section: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_divider(cx)
    }

    fn render_item(
        &mut self,
        section: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.entry_at(section, index).cloned() {
            Some(entry) => self.render_row(section, index, &entry, window, cx),
            None => div().h(ROW_HEIGHT).into_any_element(),
        }
    }
}

impl gpui::Render for InboxView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let mut key_context = list_key_context();
        if self.filter_editor.focus_handle(cx).is_focused(window) {
            key_context.add("filtering");
        }
        if self.reader_focus.is_focused(window) {
            key_context.add("reader");
        }
        self.sync_reader_note();
        if std::mem::take(&mut self.reader_note.reveal_current) {
            // A scroll reads the frame's bounds, so it waits for this frame to
            // lay the card out.
            cx.defer_in(window, |this, _, cx| this.reveal_current_message(cx));
        }
        cx.defer_in(window, |this, window, cx| {
            this.sync_earlier_cue(cx);
            this.sync_header_fit(window, cx);
        });
        let list_header = self.render_list_header(cx);
        let filter_bar = self.render_filter_bar(window, cx);
        let reader_header = self.render_reader_header(window, cx);
        let notices = self.render_notices(cx);
        let list = self.render_list(cx);
        let footer = self.render_list_footer(window, cx);
        let reader = self.render_reader(window, cx);
        let keys_sheet = self.keys_sheet;
        let border = cx.theme().colors().border;
        // The headers and the panes are two splits sharing one fraction, so
        // the notices can run full width between them.
        let resize = |cx: &mut Context<Self>| {
            cx.listener(|this, fraction: &f32, _, cx| this.resize(*fraction, cx))
                .into_resize_handler()
        };
        v_flex()
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            // While the keys sheet is open no verb runs: a bound key finds no
            // handler here, falls through to the key listener, and closes
            // the sheet instead of filing something.
            .when(!keys_sheet, |this| {
                this.on_action(cx.listener(Self::select_next))
                    .on_action(cx.listener(Self::select_previous))
                    .on_action(cx.listener(Self::select_first))
                    .on_action(cx.listener(Self::select_last))
                    .on_action(cx.listener(Self::confirm))
                    .on_action(cx.listener(Self::cancel))
                    .on_action(cx.listener(Self::accept_suggestion))
                    .on_action(cx.listener(Self::send_to_today))
                    .on_action(cx.listener(Self::send_to_soon))
                    .on_action(cx.listener(Self::send_to_someday))
                    .on_action(cx.listener(Self::archive))
                    .on_action(cx.listener(Self::open_item))
                    .on_action(cx.listener(Self::open_link))
                    .on_action(cx.listener(Self::undo))
                    .on_action(cx.listener(Self::mark))
                    .on_action(cx.listener(Self::suggest))
                    .on_action(cx.listener(Self::filter))
                    .on_action(cx.listener(Self::focus_reader))
                    .on_action(cx.listener(Self::show_menu))
                    .on_action(cx.listener(Self::show_keys))
                    .on_action(cx.listener(Self::scroll_reader_down))
                    .on_action(cx.listener(Self::scroll_reader_up))
                    .on_action(cx.listener(Self::scroll_reader_page_down))
                    .on_action(cx.listener(Self::scroll_reader_page_up))
                    .on_action(cx.listener(Self::next_message))
                    .on_action(cx.listener(Self::previous_message))
                    .on_action(cx.listener(Self::toggle_folds))
                    .on_action(cx.listener(Self::triage))
            })
            .when(keys_sheet, |this| {
                this.on_key_down(cx.listener(|this, _: &KeyDownEvent, _, cx| {
                    this.keys_sheet = false;
                    cx.notify();
                }))
            })
            .child(
                div()
                    .flex_none()
                    .h(PANE_HEADER_HEIGHT)
                    .w_full()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        Split::new(
                            "inbox-split-header",
                            self.list_fraction,
                            list_header,
                            reader_header,
                        )
                        .on_resize(resize(cx)),
                    ),
            )
            .children(notices)
            .child(
                div().flex_1().min_h_0().w_full().child(
                    Split::new(
                        "inbox-split",
                        self.list_fraction,
                        v_flex()
                            .size_full()
                            .children(filter_bar)
                            .child(div().flex_1().min_h_0().w_full().child(list))
                            .child(footer),
                        reader,
                    )
                    .on_resize(resize(cx)),
                ),
            )
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(3)
            }))
    }
}

/// Adapts a `cx.listener` over `&f32` into the split's resize callback.
trait IntoResizeHandler {
    fn into_resize_handler(self) -> Box<dyn Fn(f32, &mut Window, &mut App)>;
}

impl<F> IntoResizeHandler for F
where
    F: Fn(&f32, &mut Window, &mut App) + 'static,
{
    fn into_resize_handler(self) -> Box<dyn Fn(f32, &mut Window, &mut App)> {
        Box::new(move |fraction, window, cx| self(&fraction, window, cx))
    }
}

/// One keycap look for every Inbox hint. Dimmed, the frame fades with the key.
/// Set in the buffer's face, which tells `l` from `I` and from a pipe.
fn framed_keycap(keycap: KeyBinding, dimmed: bool, cx: &App) -> gpui::Div {
    let colors = cx.theme().colors();
    h_flex()
        .font_buffer(cx)
        .flex_none()
        .h(KEYCAP_HEIGHT)
        .min_w(KEYCAP_HEIGHT)
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(if dimmed {
            colors.border_variant
        } else {
            colors.text_placeholder
        })
        .bg(colors.element_background)
        .child(keycap.size(KEYCAP_TEXT_SIZE).disabled(dimmed))
}

/// A hint's words: they dim along with their keycaps.
fn hint_color(dimmed: bool) -> Color {
    if dimmed {
        Color::Disabled
    } else {
        Color::Muted
    }
}

/// Sits between two keycaps that do the same thing, so `space` `s` does not
/// read as one chord.
fn keycap_or(dimmed: bool) -> Label {
    Label::new("or")
        .size(LabelSize::Small)
        .color(hint_color(dimmed))
}

/// A sender's colour in a theme: their hue, at a lightness that holds up as
/// text on the badge.
fn sender_color(participant: u32, light: bool) -> Hsla {
    let hue = SENDER_HUES[participant as usize % SENDER_HUES.len()] / 360.;
    if light {
        hsla(hue, 0.6, 0.36, 1.)
    } else {
        hsla(hue, 0.5, 0.72, 1.)
    }
}

/// An email's whole subject when its sender's system cut the title short.
fn whole_title(item: &InboxItem) -> Option<String> {
    let body = without_leading_title(&item.body, &item.title);
    (item.source.as_deref() == Some("gmail"))
        .then(|| full_title(&item.title, body))
        .flatten()
}

/// The body without a first heading that repeats the title, since the
/// reader already shows the title above the destination bar. A heading that
/// says something else is the note's own and stays.
fn without_leading_title<'a>(body: &'a str, title: &str) -> &'a str {
    let body = body.trim_start();
    let (first, rest) = body.split_once('\n').unwrap_or((body, ""));
    let heading = first.trim_start_matches('#');
    let is_heading = heading.len() < first.len() && heading.starts_with([' ', '\t']);
    if is_heading && heading.trim().eq_ignore_ascii_case(title.trim()) {
        rest.trim_start()
    } else {
        body
    }
}

/// A field's value as drawn: an empty one still reads as a value.
fn field_text(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// A card's fold row as one action: `z` shows or hides every fold of the note,
/// and this says what this card holds of them, each kind once in the order it
/// first appears, with its lines or fields added up.
fn fold_summary(pieces: &[FoldPiece], open: bool) -> String {
    let mut totals: Vec<FoldPiece> = Vec::new();
    for piece in pieces {
        let same_kind = totals.iter_mut().find_map(|total| match (total, piece) {
            (FoldPiece::Text(kind, lines), FoldPiece::Text(added, more)) if kind == added => {
                *lines += more;
                Some(())
            }
            (FoldPiece::Fields(count), FoldPiece::Fields(more)) => {
                *count += more;
                Some(())
            }
            _ => None,
        });
        if same_kind.is_none() {
            totals.push(*piece);
        }
    }
    let mut parts: Vec<String> = totals
        .into_iter()
        .map(|piece| match piece {
            FoldPiece::Text(FoldKind::Repeated, lines) => {
                counted(lines, "earlier line", "earlier lines")
            }
            FoldPiece::Text(FoldKind::Quoted, lines) => {
                counted(lines, "quoted line", "quoted lines")
            }
            FoldPiece::Text(FoldKind::Signature, _) => "signature".to_string(),
            FoldPiece::Text(FoldKind::System, lines) => {
                counted(lines, "system line", "system lines")
            }
            FoldPiece::Fields(count) => counted(count, "field", "fields"),
        })
        .collect();
    let action = if open {
        "Hide all folds"
    } else {
        "Show all folds"
    };
    match parts.pop() {
        Some(last) if parts.is_empty() => format!("{action} · {last} here"),
        Some(last) => format!("{action} · {} and {last} here", parts.join(", ")),
        None => action.to_string(),
    }
}

/// How a field's value reads: a comma list as `a, b, c` and how many more
/// there are, so it holds one line until the folds open; anything else comes
/// back whole. Prose has spaces inside its items and a number has no letters.
fn field_list(value: &str, whole: bool) -> (String, usize) {
    let items: Vec<&str> = value.split(',').map(str::trim).collect();
    let is_list = items.len() > 1
        && items
            .iter()
            .all(|item| item.contains(char::is_alphabetic) && !item.contains(char::is_whitespace));
    if !is_list {
        return (value.to_string(), 0);
    }
    let shown = if whole {
        items.len()
    } else {
        items.len().min(MAX_FIELD_LIST_ITEMS)
    };
    (items[..shown].join(", "), items.len() - shown)
}

/// An old value as `was …` gives it: the same reading as the new one.
fn was_text(previous: &str, whole: bool) -> String {
    let previous = field_text(previous);
    match field_list(previous, whole) {
        (shown, 0) => shown,
        (shown, more) => format!("{shown} +{more} more"),
    }
}

/// Whether `was …` tells more than it costs.
fn previous_helps(previous: &str) -> bool {
    was_text(previous, false).chars().count() <= MAX_PREVIOUS_VALUE_LENGTH
}

fn match_count_label(count: usize) -> String {
    counted(count, "match", "matches")
}

fn earlier_label(count: usize) -> String {
    counted(count, "earlier message", "earlier messages")
}

/// Says what the bar's look means, so it never promises a suggestion there
/// isn't: nothing for a tie or no answer.
fn bar_caption(state: &BarState) -> Option<String> {
    match (state.suggested, state.leaning) {
        (Some((destination, _)), _) => Some(format!("Suggested: {}", destination.label())),
        (None, Some(destination)) => Some(format!("Leaning {}", destination.label())),
        (None, None) => None,
    }
}

/// Said above the bar, where a decision is made: the note on screen isn't the
/// only one it will reach, though the percentages are this note's alone.
fn marks_note(count: usize, percentages: bool) -> String {
    let reach = if count == 1 {
        "1 marked · decisions apply to that row".to_string()
    } else {
        format!("{count} marked · decisions apply to all")
    };
    if percentages {
        format!("{reach} · percentages are this row's")
    } else {
        reach
    }
}

/// `Marta <marta@example.com>` → `Marta`.
fn sender_name(from: &str) -> String {
    let name = from
        .split('<')
        .next()
        .unwrap_or(from)
        .trim()
        .trim_matches('"');
    if name.is_empty() {
        from.trim_matches(['<', '>']).to_string()
    } else {
        name.to_string()
    }
}

/// The row's name for a sender: `helpdesk@example.com` → `helpdesk`.
fn short_sender(from: &str) -> String {
    let name = sender_name(from);
    match name.split_once('@') {
        Some((local, _)) if !local.is_empty() => local.to_string(),
        _ => name,
    }
}

/// The header's sender as what stays and what gives way first:
/// `helpdesk@example.com` → `helpdesk`, `@example.com`. A name keeps itself and
/// gives up its address.
fn sender_parts(name: String, address: Option<String>) -> (String, Option<String>) {
    match name.split_once('@') {
        Some((local, domain)) if !local.is_empty() => {
            (local.to_string(), Some(format!("@{domain}")))
        }
        _ => (name, address),
    }
}

/// `via helpdesk@example.com` on a relayed message's card, unless the reader
/// header already names that sender.
fn relayed_via(sender: &str, header_from: Option<&str>) -> Option<String> {
    let header_sender = header_from.map(sender_name);
    (header_sender.as_deref() != Some(sender)).then(|| format!("via {sender}"))
}

/// `Marta <marta@example.com>` → `marta@example.com`; `None` without one.
fn sender_address(from: &str) -> Option<String> {
    let (_, rest) = from.split_once('<')?;
    let (address, _) = rest.split_once('>')?;
    let address = address.trim();
    (!address.is_empty() && address != sender_name(from)).then(|| address.to_string())
}

/// How a capture source reads in the meta lines.
fn source_label(source: &str) -> String {
    match source {
        "gmail" => "Gmail".to_string(),
        "google-tasks" => "Google Tasks".to_string(),
        "thock-ios" => "Phone".to_string(),
        other => other.to_string(),
    }
}

/// The reader header's glyph for a capture source.
fn source_icon(source: Option<&str>) -> IconName {
    match source {
        Some("gmail") => IconName::Envelope,
        Some("google-tasks") => IconName::ListTodo,
        _ => IconName::Notepad,
    }
}

fn captured_label(captured: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(captured)
        .map(|moment| {
            moment
                .with_timezone(&Local)
                .format("%b %-d, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| captured.to_string())
}

/// `5m`, `2h`, `3d`: how long a row has waited since its capture.
fn age_label(captured: Option<&str>, now: chrono::DateTime<Utc>) -> Option<String> {
    let captured = chrono::DateTime::parse_from_rfc3339(captured?).ok()?;
    let waited = now - captured.with_timezone(&Utc);
    Some(
        match (waited.num_minutes(), waited.num_hours(), waited.num_days()) {
            (minutes, _, _) if minutes < 1 => "now".to_string(),
            (minutes, 0, _) => format!("{minutes}m"),
            (_, hours, 0) => format!("{hours}h"),
            (_, _, days) => format!("{days}d"),
        },
    )
}

/// Where a revealed card lands, as a scroll offset. `landings` holds the offset
/// that brings each card up to the current one, the last, to its place under
/// the cue. Too near the end of the thread for that, the current card lands on
/// the nearest card boundary the scroll reaches, so nothing of the card before
/// shows under the cue, as long as the current card's header stays on screen.
fn landing(landings: &[Pixels], farthest: Pixels, viewport_height: Pixels) -> Pixels {
    let Some((&current, earlier)) = landings.split_last() else {
        return px(0.);
    };
    if current <= farthest {
        return current;
    }
    earlier
        .iter()
        .rev()
        .find(|landing| **landing <= farthest)
        .copied()
        .filter(|landing| {
            current - *landing + REVEAL_CLEARANCE + CARD_HEADER_HEIGHT <= viewport_height
        })
        .unwrap_or(farthest)
}

/// The single replacement that turns `old` into `new`: the bytes between
/// their common prefix and common suffix, as a buffer edit. `None` when the
/// texts are equal.
fn single_edit(old: &str, new: &str) -> Option<(Range<usize>, String)> {
    if old == new {
        return None;
    }
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let longest_suffix = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(longest_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) {
        suffix -= 1;
    }
    Some((
        prefix..old.len() - suffix,
        new[prefix..new.len() - suffix].to_string(),
    ))
}

/// The text without the last line that reads exactly `line`. A decision
/// appends its line at the end of a section, so the last exact match is the
/// one this view wrote; a line of the person's own that merely looks alike
/// (a scheduled `09:00 - 09:30 Standup` beside a filed `Standup`) is never
/// touched. A task appended into an empty section came with its own blank
/// line, so when blank lines surround the match one of them goes too.
fn without_line(text: &str, line: &str) -> Option<String> {
    let mut found = None;
    let mut offset = 0;
    for candidate in text.split_inclusive('\n') {
        if candidate.trim_end_matches(['\n', '\r']) == line {
            found = Some((offset, candidate.len()));
        }
        offset += candidate.len();
    }
    let (start, len) = found?;
    let before = &text[..start];
    let mut after = &text[start + len..];
    if before.ends_with("\n\n") && after.starts_with('\n') {
        after = &after[1..];
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(before);
    out.push_str(after);
    Some(out)
}

/// The destination the triage log names (V40 §5).
fn log_destination(destination: Destination, vault: &Vault) -> String {
    match destination {
        Destination::Today => format!("Today · {}", vault.config.day_planner.heading),
        Destination::Soon => format!("Backlog · {}", vault.config.backlog.headings.soon),
        Destination::Someday => format!("Backlog · {}", vault.config.backlog.headings.someday),
        Destination::Archive => "Archived".to_string(),
    }
}

/// Creates `path` with `contents` when it is missing, so it can be opened as
/// a buffer.
async fn ensure_file(fs: &Arc<dyn Fs>, path: &Path, contents: &str) -> Result<()> {
    if fs.is_file(path).await {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs.create_dir(parent).await?;
    }
    fs.atomic_write(path.to_path_buf(), contents.to_string())
        .await
}

/// Edits an open (or opened) buffer with `edit` applied to its current text
/// and saves it, so a note open in the editor sees the change and a dirty
/// buffer is never overwritten from underneath.
async fn edit_buffer(
    project: &Entity<Project>,
    path: PathBuf,
    edit: impl Fn(&str) -> Option<(Range<usize>, String)> + Send + 'static,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<bool> {
    let buffer = project
        .update(cx, |project, cx| project.open_local_buffer(&path, cx))
        .await?;
    let changed = buffer.update(cx, |buffer, cx| {
        let text = buffer.text();
        match edit(&text) {
            Some((range, new_text)) => {
                buffer.edit([(range, new_text)], None, cx);
                true
            }
            None => false,
        }
    });
    if changed {
        project
            .update(cx, |project, cx| project.save_buffer(buffer, cx))
            .await?;
    }
    Ok(changed)
}

/// V40 §5's writes for one item, in order: the task line, the move, the
/// log line (§6.1). A note already archived under the same name, or already
/// gone, is a no-op: nothing is written and the row stays or leaves on its
/// own (`Ok((None, note))`). A failure before the move leaves the note in
/// the inbox.
async fn file_one(
    project: &Entity<Project>,
    fs: &Arc<dyn Fs>,
    vault: &Vault,
    entry: &InboxEntry,
    destination: Destination,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<(Option<FiledItem>, FilingNote)> {
    let item = &entry.item;
    let inbox_path = vault.root.join(&item.path);
    let archived = vault.root.join(archive_path(&item.path));
    if !fs.is_file(&inbox_path).await {
        return Ok((None, FilingNote::AlreadyGone));
    }
    if fs.is_file(&archived).await {
        return Ok((None, FilingNote::AlreadyArchived));
    }

    let title = sanitize_title(&item.title);
    let task_line = inbox_task_line(&title, &item.stem, item.has_body);
    let now = Local::now();
    let today = now.date_naive();
    let task = match destination {
        Destination::Today => {
            let (path, outcome) = ensure_note(fs, vault, NoteKind::Daily, today, now.time())
                .await
                .context("creating today's note")?;
            if outcome == EnsureNoteOutcome::CreatedWithoutTemplate {
                log::warn!(
                    "Thock: the daily template is missing, so today's note was created empty"
                );
            }
            if outcome != EnsureNoteOutcome::AlreadyExisted {
                cx.update(|_, cx| crate::notes::announce_note(project, &path, cx))?;
            }
            let heading = vault.config.day_planner.heading_names();
            let block = format!("{task_line}\n");
            edit_buffer(
                project,
                path.clone(),
                move |text| {
                    let edit = backlog::append_block_to_note_edit(text, &heading, &block);
                    Some((edit.range, edit.new_text))
                },
                cx,
            )
            .await
            .context("adding the task to today's note")?;
            Some(TaskWrite {
                path,
                line: task_line.clone(),
            })
        }
        Destination::Soon | Destination::Someday => {
            let path = vault.backlog_path();
            ensure_file(fs, &path, DEFAULT_BACKLOG).await?;
            let kind = if destination == Destination::Soon {
                SectionKind::Soon
            } else {
                SectionKind::Someday
            };
            let headings = vault.config.backlog.headings.clone();
            let block = format!("{task_line}\n");
            edit_buffer(
                project,
                path.clone(),
                move |text| {
                    let edit = backlog::append_to_section_edit(text, kind, None, &block, &headings);
                    Some((edit.range, edit.new_text))
                },
                cx,
            )
            .await
            .context("adding the task to the backlog")?;
            Some(TaskWrite {
                path,
                line: task_line.clone(),
            })
        }
        Destination::Archive => None,
    };

    // The phone may have archived the note between the check above and now;
    // the task line is then the spec's accepted worst case (§6.1), and undo
    // must not pull the phone's copy back.
    let (moved, note) = match move_note(fs, &inbox_path, &archived)
        .await
        .context("moving the note into the archive")?
    {
        MoveOutcome::Moved => (true, FilingNote::Filed),
        MoveOutcome::SourceGone => (false, FilingNote::AlreadyGone),
        MoveOutcome::DestinationExists => (false, FilingNote::AlreadyArchived),
    };

    let log_line = triage_log_line(
        &today.format("%Y-%m-%d").to_string(),
        &title,
        &log_destination(destination, vault),
        item.digest.as_deref(),
    );
    let log_path = vault.root.join(TRIAGE_LOG_PATH);
    ensure_file(fs, &log_path, "").await?;
    let write = Write::new(
        "desk",
        TRIAGE_LOG_PATH,
        Operation::Append {
            heading: None,
            lines: vec![log_line.clone()],
            placement: Default::default(),
            blank_line_before: false,
            create_from_template: false,
            after: None,
        },
    );
    // The log is a note the Routine links, so it goes through the buffer
    // like the task line; the applier gives the same bytes the phone gets.
    edit_buffer(
        project,
        log_path,
        move |text| {
            let existing = (!text.is_empty()).then_some(text);
            single_edit(text, &apply(existing, &write, None).text)
        },
        cx,
    )
    .await
    .context("writing the triage log")?;

    Ok((
        Some(FiledItem {
            stem: item.stem.clone(),
            destination,
            inbox_path,
            archive_path: archived,
            moved,
            task,
            log_line,
            at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }),
        note,
    ))
}

/// Reverses one filed item (§6.2): the note back, the task line out (a
/// no-op if it was edited meanwhile), the log line out.
async fn undo_one(
    project: &Entity<Project>,
    fs: &Arc<dyn Fs>,
    vault: &Vault,
    item: &FiledItem,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<()> {
    if item.moved {
        move_note(fs, &item.archive_path, &item.inbox_path)
            .await
            .context("moving the note back")?;
    }
    if let Some(task) = &item.task {
        let line = task.line.clone();
        edit_buffer(
            project,
            task.path.clone(),
            move |text| single_edit(text, &without_line(text, &line)?),
            cx,
        )
        .await
        .context("removing the task line")?;
    }
    let log_line = item.log_line.clone();
    edit_buffer(
        project,
        vault.root.join(TRIAGE_LOG_PATH),
        move |text| single_edit(text, &without_line(text, &log_line)?),
        cx,
    )
    .await
    .context("removing the triage-log line")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::note_hash;
    use crate::inbox_suggest::SuggestionRecord;
    use crate::plus;
    use fs::FakeFs;
    use gpui::{KeyBinding as GpuiKeyBinding, TestAppContext, VisualTestContext};
    use http_client::{AsyncBody, FakeHttpClient, HttpClient, Response};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult};

    const SHIP_IT: &str = "---\nsource:   google-tasks\ncapture:  3f1c3f1c3f1c\ncaptured: 2026-10-09T18:42:00Z\ntitle:    Ship it — a practical guide\nurl:      https://example.com/ship-it\n---\n\n# Ship it — a practical guide\n\nhttps://example.com/ship-it\n";
    const Q4: &str = "---\nsource:   gmail\ncapture:  a1a1a1a1a1a1\ncaptured: 2026-10-09T19:00:00Z\ntitle:    Re: Q4 planning doc\nfrom:     Marta <marta@example.com>\n---\n\n# Re: Q4 planning doc\n\nCan you look at the numbers before Friday?\n";
    const CALL_ANA: &str = "---\nsource:   thock-ios\ncapture:  4d1f9a02c7b3\ncaptured: 2026-10-10T09:31:00Z\ntitle:    Call Ana\n---\n\n# Call Ana\n";
    const TICKET: &str = "---\nsource:   gmail\ncapture:  b2b2b2b2b2b2\ncaptured: 2026-10-09T19:30:00Z\ntitle:    Ticket 42\nfrom:     helpdesk@example.com\n---\n\n# Ticket 42\n\n## helpdesk@example.com — 2026-10-09 08:55\n\nComment #1 (by rsilva on 10/09/26 07:55am):\n\nThe farm stopped taking jobs.\nCould someone look?\nIt's urgent.\n\nStatus:  OPEN\n\nTeam:  render-ops\n\nHost:  N/A\n\n## helpdesk@example.com — 2026-10-09 09:13\n\nComment #2 (by jlopes on 10/09/26 08:13am):\n\nThe print server is down again.\n\nComment #1 (by rsilva on 10/09/26 07:55am):\n\nThe farm stopped taking jobs.\nCould someone look?\nIt's urgent.\n\nStatus:  IN PROGRESS\n\nTeam:  render-ops\n\nHost:  N/A\n";
    const IDEA: &str = "# idea: routines could carry state\n\n![sketch](/images/sketch.png)\n";
    const TODAY_NOTE: &str =
        "# Today\n\n## Journal\n\n## Day planner\n\n- [ ] 09:00 - 09:30 Standup\n\n## Personal\n";

    fn keymap_bindings(content: &str, cx: &App) -> Vec<GpuiKeyBinding> {
        match KeymapFile::load(content, cx) {
            KeymapFileLoadResult::Success { key_bindings } => key_bindings,
            // Actions from crates this one doesn't link (the debugger, the
            // app shell's own `thock::` window actions) are expected to be
            // missing here; the app crate's keymap guard checks every name.
            KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
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
            init(cx);
            let bindings = keymap_bindings(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            );
            cx.bind_keys(bindings);
            if vim {
                // `vim::MenuSelectNext` lives in the vim crate, which this
                // crate doesn't link; it dispatches `menu::SelectNext`.
                let bindings =
                    keymap_bindings(include_str!("../../../assets/keymaps/vim.json"), cx);
                cx.bind_keys(bindings);
                cx.bind_keys([
                    GpuiKeyBinding::new(
                        "j",
                        SelectNext,
                        Some(&format!("{INBOX_VIEW_KEY} && !filtering && !reader")),
                    ),
                    GpuiKeyBinding::new(
                        "k",
                        SelectPrevious,
                        Some(&format!("{INBOX_VIEW_KEY} && !filtering && !reader")),
                    ),
                ]);
            }
        });
    }

    struct Setup {
        fs: Arc<FakeFs>,
        workspace: Entity<Workspace>,
        view: Entity<InboxView>,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    #[derive(Default)]
    struct Options {
        vim: bool,
        grant: bool,
        cached: Vec<(&'static str, &'static str, f64)>,
    }

    fn entitlement_json() -> String {
        json!({
            "user_id": "u1", "status": "active", "plan_id": "dev", "plan_name": "Thock Plus",
            "allowance_units": 500, "used_units": 10, "remaining_units": 490,
            "gateway": {"provider": "openrouter", "api_key": "sk-or-v1-test",
                        "models": {"default": "google/gemini-2.5-flash"}}
        })
        .to_string()
    }

    /// A vault with four waiting items (three new, one carried over), the
    /// view open and focused via `ctrl-alt-i`.
    async fn setup(cx: &mut TestAppContext, options: Options) -> Setup {
        init_test(cx, options.vim);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        let fs = FakeFs::new(cx.executor());
        let mut cache = String::new();
        for (content, choice, confidence) in &options.cached {
            let suggestion = crate::inbox_suggest::parse_response(
                &json!({"answers": {"destination": {"choice": choice,
                    "probabilities": {choice.to_string(): confidence}, "confidence": confidence}},
                    "usage": {"cost": 0.00001}})
                .to_string(),
            )
            .unwrap();
            let record = SuggestionRecord::new(
                &note_hash(content),
                "stem",
                &suggestion,
                "2026-10-10T09:00:00Z",
            );
            cache.push_str(&serde_json::to_string(&record).unwrap());
            cache.push('\n');
        }
        fs.insert_tree(
            &root,
            json!({
                ".thock": {
                    "config.toml": "schema = 1\n",
                    "inbox.toml": "schema = 1\n",
                    "state": { "inbox": {
                        "last-triage": "2026-10-09T18:00:00Z\n",
                        "suggestions.jsonl": cache,
                    } },
                },
                "inbox": {
                    "2026-10-09-1842-ship-it.md": SHIP_IT,
                    "2026-10-09-1900-re-q4-planning-doc.md": Q4,
                    "2026-10-10-0931-call-ana.md": CALL_ANA,
                    "idea.md": IDEA,
                },
                "daily": { (today_note_name()): TODAY_NOTE },
                "backlog.md": DEFAULT_BACKLOG,
                "routines": { "inbox": { "triage-policy.md": "## Proposals\n\n| a | b |\n| --- | --- |\n| A link | Backlog · Someday |\n" } },
            }),
        )
        .await;
        // `idea.md` is hand-written: its mtime decides the section, and the
        // fake fs stamps files with the (frozen) test clock.
        if options.grant {
            cx.update(|cx| {
                plus::set_current_entitlement(
                    Some(serde_json::from_str(&entitlement_json()).unwrap()),
                    cx,
                );
                let client: Arc<dyn HttpClient> = FakeHttpClient::create(|_| async move {
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(
                            json!({"answers": {"destination": {"choice": "unsure",
                                "probabilities": {"unsure": 0.6}, "confidence": 0.6}},
                                "usage": {"cost": 0.00001}})
                            .to_string()
                            .into_bytes(),
                        ))
                        .unwrap())
                });
                cx.set_http_client(client);
            });
        } else {
            cx.update(|cx| plus::set_current_entitlement(None, cx));
        }
        let project = Project::test(fs.clone(), [root.as_path()], cx).await;
        let service = cx.update(|cx| inbox_service::new_for_test(&project, cx));
        // The workspace's key context and registered actions are rendered by
        // the multi-workspace, so the chord needs the real root.
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        cx.run_until_parked();
        // The service reads the vault from disk the way `init` would.
        service.update(&mut cx, |service, cx| service.reload_config(cx));
        cx.run_until_parked();
        // Open a note first so `escape` has somewhere to return to.
        let project_path = project
            .read_with(&cx, |project, cx| {
                project.find_project_path(root.join(today_note_path()), cx)
            })
            .unwrap();
        workspace
            .update_in(&mut cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-alt-i");
        cx.run_until_parked();
        let view = workspace
            .read_with(&cx, |workspace, cx| {
                workspace.items_of_type::<InboxView>(cx).next()
            })
            .expect("ctrl-alt-i opens the Inbox tab");
        Setup {
            fs,
            workspace,
            view,
            cx,
            _vault_dir: vault_dir,
        }
    }

    /// Filing to Today writes the note for the real date, so the fixture's
    /// daily note is named for it too.
    fn today_note_name() -> String {
        format!("{}.md", Local::now().format("%Y-%m-%d"))
    }

    fn today_note_path() -> String {
        format!("daily/{}", today_note_name())
    }

    fn titles(setup: &Setup) -> Vec<String> {
        setup.view.read_with(&setup.cx, |view, _| {
            view.visible
                .iter()
                .map(|entry| entry.item.title.clone())
                .collect()
        })
    }

    fn selected(setup: &Setup) -> Option<String> {
        setup.view.read_with(&setup.cx, |view, _| {
            view.selected_entry().map(|entry| entry.item.title.clone())
        })
    }

    fn view_focused(setup: &mut Setup) -> bool {
        let view = setup.view.clone();
        setup
            .cx
            .update(|window, cx| view.read(cx).focus_handle.contains_focused(window, cx))
    }

    async fn load(setup: &Setup, path: &str) -> Option<String> {
        setup
            .fs
            .load(&setup._vault_dir.path().join(path))
            .await
            .ok()
    }

    #[gpui::test]
    async fn opens_with_the_queue_split_and_the_newest_selected(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        assert!(view_focused(&mut setup));
        // Newest first by capture time; the hand-written idea has only the
        // fake filesystem's old mtime, so it sorts last and is carried over.
        assert_eq!(
            titles(&setup),
            [
                "Call Ana",
                "Re: Q4 planning doc",
                "Ship it — a practical guide",
                "idea: routines could carry state",
            ]
        );
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(view.new_count, 3);
            assert!(!view.visible[3].is_new, "the idea predates the watermark");
            assert_eq!(
                view.layout().len(),
                5,
                "four rows and the carried-over divider; New has no header row"
            );
        });
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            let view = workspace.items_of_type::<InboxView>(cx).next().unwrap();
            assert_eq!(view.read(cx).tab_content_text(0, cx).as_ref(), "Inbox · 4");
        });

        // A second chord focuses the existing tab rather than opening another.
        setup.cx.simulate_keystrokes("ctrl-alt-i");
        setup.cx.run_until_parked();
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<InboxView>(cx).count(), 1);
        });
    }

    #[gpui::test]
    async fn arrows_walk_the_list_and_escape_returns_to_the_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("down down");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("Ship it — a practical guide")
        );
        setup.cx.simulate_keystrokes("up");
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));
        setup.cx.simulate_keystrokes("end");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("idea: routines could carry state")
        );
        setup.cx.simulate_keystrokes("home");
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));

        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(!view_focused(&mut setup));
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            assert!(
                workspace.active_item_as::<Editor>(cx).is_some(),
                "escape goes back to the note that was open"
            );
        });
    }

    #[gpui::test]
    async fn vim_letters_move_only_with_vim_mode(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("j");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("Call Ana"),
            "j is not a motion without vim mode"
        );
        drop(setup);

        let mut setup = setup_vim(cx).await;
        setup.cx.simulate_keystrokes("j j");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("Ship it — a practical guide")
        );
        setup.cx.simulate_keystrokes("k");
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));
        setup.cx.simulate_keystrokes("shift-g");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("idea: routines could carry state")
        );
        setup.cx.simulate_keystrokes("g g");
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
    }

    async fn setup_vim(cx: &mut TestAppContext) -> Setup {
        setup(
            cx,
            Options {
                vim: true,
                ..Options::default()
            },
        )
        .await
    }

    #[gpui::test]
    async fn t_files_to_today_with_the_phones_three_writes(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
        setup.cx.simulate_keystrokes("t");
        setup.cx.run_until_parked();

        let today = load(&setup, &today_note_path()).await.unwrap();
        assert!(
            today.contains("- [ ] 09:00 - 09:30 Standup\n- [ ] Call Ana\n\n## Personal"),
            "{today}"
        );
        assert!(
            load(&setup, "inbox/2026-10-10-0931-call-ana.md")
                .await
                .is_none()
        );
        assert_eq!(
            load(&setup, "archives/inbox/2026-10-10-0931-call-ana.md")
                .await
                .as_deref(),
            Some(CALL_ANA)
        );
        let log = load(&setup, TRIAGE_LOG_PATH).await.unwrap();
        let day = Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(
            log,
            format!("- {day} · Call Ana → Today · Day planner <!--inbox:4d1f9a02c7b3-->\n")
        );
        assert!(crate::inbox::scan_triage_log_markers(&log).contains("4d1f9a02c7b3"));
        // The selection moved to the next row and the footer counts the
        // filing once the Plus offer, which a vault without a grant shows
        // there first, is dismissed.
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));
        setup.view.update(&mut setup.cx, |view, cx| {
            assert_eq!(view.filed_today, 1);
            assert_eq!(view.footer_stats(cx), FooterStats::PlusOffer);
            view.plus_banner_dismissed = true;
            assert_eq!(
                view.footer_stats(cx),
                FooterStats::Stats("1 filed today".to_string())
            );
        });
        let decisions = load(&setup, ".thock/state/inbox/decisions.jsonl")
            .await
            .unwrap();
        let record: DecisionRecord =
            serde_json::from_str(decisions.lines().next().unwrap()).unwrap();
        assert_eq!(record.stem, "2026-10-10-0931-call-ana");
        assert_eq!(record.chosen, "today");
        assert_eq!(record.via, "t");
        assert_eq!(record.suggested, None);
        assert!(view_focused(&mut setup));
    }

    #[gpui::test]
    async fn s_then_u_restores_every_file(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        let backlog_before = load(&setup, "backlog.md").await.unwrap();
        setup.cx.simulate_keystrokes("down");
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));
        setup.cx.simulate_keystrokes("s");
        setup.cx.run_until_parked();

        let backlog = load(&setup, "backlog.md").await.unwrap();
        assert!(
            backlog.contains(
                "## Soon\n\n- [ ] Re: Q4 planning doc [[2026-10-09-1900-re-q4-planning-doc]]\n"
            ),
            "{backlog}"
        );
        assert!(
            load(&setup, "inbox/2026-10-09-1900-re-q4-planning-doc.md")
                .await
                .is_none()
        );
        let log = load(&setup, TRIAGE_LOG_PATH).await.unwrap();
        assert!(
            log.contains("→ Backlog · Soon <!--inbox:a1a1a1a1a1a1-->"),
            "{log}"
        );
        assert_eq!(titles(&setup).len(), 3);

        setup.cx.simulate_keystrokes("u");
        setup.cx.run_until_parked();
        assert_eq!(load(&setup, "backlog.md").await.unwrap(), backlog_before);
        assert_eq!(
            load(&setup, "inbox/2026-10-09-1900-re-q4-planning-doc.md")
                .await
                .as_deref(),
            Some(Q4)
        );
        assert!(
            load(
                &setup,
                "archives/inbox/2026-10-09-1900-re-q4-planning-doc.md"
            )
            .await
            .is_none()
        );
        assert_eq!(load(&setup, TRIAGE_LOG_PATH).await.unwrap(), "");
        assert_eq!(titles(&setup).len(), 4);
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));
        let decisions = load(&setup, ".thock/state/inbox/decisions.jsonl")
            .await
            .unwrap();
        let record: DecisionRecord =
            serde_json::from_str(decisions.lines().next().unwrap()).unwrap();
        assert!(record.undone);
        assert_eq!(record.via, "s");

        // A second undo has nothing to undo.
        setup.cx.simulate_keystrokes("u");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 4);
    }

    /// V53 §10.4: `e` on an email archives its thread in Gmail once the
    /// grace period is over, and `u` puts it back.
    #[gpui::test]
    async fn e_then_u_reaches_gmail_archive(cx: &mut TestAppContext) {
        use crate::gmail::{ArchiveConfig, GmailConfig, ThreadMove};
        use crate::gmail_service::{self, StubTransport};
        use thock_sync_core::DecisionClass;

        let mut setup = setup(cx, Options::default()).await;
        let root = setup._vault_dir.path().to_path_buf();
        setup
            .fs
            .insert_tree(
                root.join(".thock/state/gmail"),
                json!({
                    "imported.jsonl": "{\"digest\":\"a1a1a1a1a1a1\",\"thread\":\"t-q4\",\"path\":\"inbox\"}\n",
                }),
            )
            .await;
        let clock = Arc::new(std::sync::Mutex::new(
            chrono::DateTime::parse_from_rfc3339("2026-11-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ));
        let transport = Arc::new(StubTransport {
            items: std::sync::Mutex::new(vec![vec![], vec![]]),
            ..Default::default()
        });
        let project = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.project().clone());
        let gmail = setup
            .cx
            .update(|_, cx| gmail_service::new_for_test(&project, cx));
        gmail.update(&mut setup.cx, |gmail, cx| {
            gmail.set_now_for_test(Arc::new({
                let clock = clock.clone();
                move || *clock.lock().unwrap()
            }));
            let config = GmailConfig {
                account: Some("diego@example.com".to_string()),
                archive: ArchiveConfig::On([DecisionClass::Archive].into()),
                ..GmailConfig::default()
            };
            let vault = Vault {
                root: root.clone(),
                config: crate::vault::VaultConfig::default(),
            };
            gmail.configure_for_test(vault, config, transport.clone(), cx)
        });
        setup.cx.run_until_parked();
        let advance = |setup: &mut Setup, seconds: u64| {
            *clock.lock().unwrap() += chrono::Duration::seconds(seconds as i64);
            setup
                .cx
                .executor()
                .advance_clock(std::time::Duration::from_secs(seconds));
            setup.cx.run_until_parked();
        };

        setup.cx.simulate_keystrokes("down");
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));
        setup.cx.simulate_keystrokes("e");
        setup.cx.run_until_parked();
        assert!(transport.modified.lock().unwrap().is_empty());
        advance(&mut setup, 61);
        assert_eq!(
            *transport.modified.lock().unwrap(),
            vec![("t-q4".to_string(), ThreadMove::Archive)]
        );

        setup.cx.simulate_keystrokes("u");
        setup.cx.run_until_parked();
        assert_eq!(
            *transport.modified.lock().unwrap(),
            vec![
                ("t-q4".to_string(), ThreadMove::Archive),
                ("t-q4".to_string(), ThreadMove::Unarchive),
            ]
        );
    }

    #[gpui::test]
    async fn marked_rows_archive_together_in_list_order(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        // The first and the last row, so the batch spans both sections and
        // the log follows list order, not marking order.
        let title_before = drawn(&mut setup, "INBOX_TITLE-0");
        assert_eq!(setup.cx.debug_bounds("INBOX_CHECK-0"), None);
        assert_eq!(setup.cx.debug_bounds("INBOX_MARKS_NOTE"), None);
        setup.cx.simulate_keystrokes("end x home x");
        setup
            .view
            .read_with(&setup.cx, |view, _| assert_eq!(view.marked.len(), 2));
        // The check hangs in the row's padding beside the title's first line,
        // so marking moves no text, and the reader says the bar now reaches
        // beyond the note on screen.
        let title = drawn(&mut setup, "INBOX_TITLE-0");
        let check = drawn(&mut setup, "INBOX_CHECK-0");
        let row = drawn(&mut setup, "INBOX_ROW-0");
        assert_eq!(title, title_before);
        assert_eq!(check.top(), title.top(), "{check:?} on {title:?}");
        assert!(check.left() >= row.left(), "{check:?} in {row:?}");
        assert!(
            check.right() + px(3.) <= title.left(),
            "{check:?} clear of {title:?}"
        );
        assert_eq!(setup.cx.debug_bounds("INBOX_CHECK-1"), None);
        assert!(setup.cx.debug_bounds("INBOX_MARKS_NOTE").is_some());
        setup.cx.simulate_keystrokes("e");
        setup.cx.run_until_parked();
        assert_eq!(
            titles(&setup),
            ["Re: Q4 planning doc", "Ship it — a practical guide"]
        );
        let log = load(&setup, TRIAGE_LOG_PATH).await.unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(
            lines[0].contains("Call Ana → Archived <!--inbox:4d1f9a02c7b3-->"),
            "{log}"
        );
        assert!(
            lines[1].contains("idea: routines could carry state → Archived"),
            "{log}"
        );
        assert!(
            !lines[1].contains("<!--"),
            "a hand-written note has no marker"
        );
        assert!(load(&setup, "archives/inbox/idea.md").await.is_some());
        // No task line anywhere.
        assert_eq!(load(&setup, "backlog.md").await.unwrap(), DEFAULT_BACKLOG);
        assert_eq!(load(&setup, &today_note_path()).await.unwrap(), TODAY_NOTE);
        setup
            .view
            .read_with(&setup.cx, |view, _| assert!(view.marked.is_empty()));

        // Undo of a batch restores both.
        setup.cx.simulate_keystrokes("u");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 4);
        assert_eq!(load(&setup, TRIAGE_LOG_PATH).await.unwrap(), "");
    }

    #[gpui::test]
    async fn slash_filters_and_escape_clears(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("/");
        setup.cx.run_until_parked();
        let filter_focused = {
            let view = setup.view.clone();
            setup.cx.update(|window, cx| {
                view.read(cx)
                    .filter_editor
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            })
        };
        assert!(filter_focused, "/ moves focus into the filter box");
        setup.cx.simulate_input("ana");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup), ["Call Ana"]);
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
        // While filtering, letters type instead of filing.
        setup.cx.simulate_keystrokes("t");
        setup.cx.run_until_parked();
        assert!(
            load(&setup, "inbox/2026-10-10-0931-call-ana.md")
                .await
                .is_some()
        );
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 4);
        assert!(view_focused(&mut setup));
        setup
            .view
            .read_with(&setup.cx, |view, _| assert!(view.filter.is_empty()));
    }

    #[gpui::test]
    async fn tab_moves_to_the_reader_and_back(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("tab");
        setup.cx.run_until_parked();
        let reader_focused = |setup: &mut Setup| {
            let view = setup.view.clone();
            setup
                .cx
                .update(|window, cx| view.read(cx).reader_focus.is_focused(window))
        };
        assert!(reader_focused(&mut setup));
        // Reader keys scroll rather than move the selection or file.
        setup.cx.simulate_keystrokes("j t");
        setup.cx.run_until_parked();
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
        assert!(
            load(&setup, "inbox/2026-10-10-0931-call-ana.md")
                .await
                .is_some()
        );
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(!reader_focused(&mut setup));
        setup.cx.simulate_keystrokes("tab tab");
        setup.cx.run_until_parked();
        assert!(!reader_focused(&mut setup));
    }

    #[gpui::test]
    async fn a_file_event_removing_the_selected_item_moves_the_selection(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("down");
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));
        setup
            .fs
            .remove_file(
                &setup
                    ._vault_dir
                    .path()
                    .join("inbox/2026-10-09-1900-re-q4-planning-doc.md"),
                Default::default(),
            )
            .await
            .unwrap();
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 3);
        assert_eq!(
            selected(&setup).as_deref(),
            Some("Ship it — a practical guide")
        );
        // The last row leaving selects the new last row.
        setup.cx.simulate_keystrokes("end");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("idea: routines could carry state")
        );
        setup
            .fs
            .remove_file(
                &setup._vault_dir.path().join("inbox/idea.md"),
                Default::default(),
            )
            .await
            .unwrap();
        setup.cx.run_until_parked();
        assert_eq!(
            selected(&setup).as_deref(),
            Some("Ship it — a practical guide")
        );
    }

    #[gpui::test]
    async fn without_a_grant_space_does_nothing(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                cached: vec![(SHIP_IT, "someday", 0.91)],
                ..Options::default()
            },
        )
        .await;
        setup.view.read_with(&setup.cx, |view, cx| {
            assert_eq!(view.availability(cx), Some(SuggestionAvailability::NoGrant));
            for entry in &view.visible {
                assert_eq!(view.suggested_destination(entry, cx), None);
            }
        });
        setup.cx.simulate_keystrokes("space");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 4, "nothing was filed");
        assert_eq!(load(&setup, TRIAGE_LOG_PATH).await, None);
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
        // Opening is `o`'s job; `space` never opens the note.
        assert!(view_focused(&mut setup));
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            assert!(workspace.active_item_as::<InboxView>(cx).is_some());
        });
    }

    #[gpui::test]
    async fn with_a_grant_space_takes_the_cached_suggestion(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91), (Q4, "soon", 0.71)],
                ..Options::default()
            },
        )
        .await;
        setup.cx.run_until_parked();
        setup.view.read_with(&setup.cx, |view, cx| {
            assert_eq!(view.availability(cx), Some(SuggestionAvailability::On));
            let by_title = |title: &str| {
                view.visible
                    .iter()
                    .find(|entry| entry.item.title == title)
                    .cloned()
                    .unwrap()
            };
            assert_eq!(
                view.suggested_destination(&by_title("Ship it — a practical guide"), cx),
                Some(Destination::Someday)
            );
            assert_eq!(
                view.suggested_destination(&by_title("Re: Q4 planning doc"), cx),
                Some(Destination::Soon)
            );
            // The uncached rows were asked and came back `unsure`: no chip.
            assert_eq!(view.suggested_destination(&by_title("Call Ana"), cx), None);
        });

        // `j` `j` `space` from the top files the third row to its suggestion
        // and lands on the next row.
        setup.cx.simulate_keystrokes("down down");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("Ship it — a practical guide")
        );
        setup.cx.simulate_keystrokes("space");
        setup.cx.run_until_parked();
        let backlog = load(&setup, "backlog.md").await.unwrap();
        assert!(
            backlog.contains(
                "## Someday\n\n- [ ] Ship it — a practical guide [[2026-10-09-1842-ship-it]]\n"
            ),
            "{backlog}"
        );
        assert_eq!(
            selected(&setup).as_deref(),
            Some("idea: routines could carry state")
        );
        let log = load(&setup, TRIAGE_LOG_PATH).await.unwrap();
        assert!(
            log.contains("→ Backlog · Someday <!--inbox:3f1c3f1c3f1c-->"),
            "{log}"
        );
        let decisions = load(&setup, ".thock/state/inbox/decisions.jsonl")
            .await
            .unwrap();
        let last: DecisionRecord = serde_json::from_str(decisions.lines().last().unwrap()).unwrap();
        assert_eq!(last.suggested.as_deref(), Some("someday"));
        assert_eq!(last.chosen, "someday");
        assert_eq!(last.via, "space");
        assert_eq!(last.confidence, Some(0.91));

        // An outlined chip files too; the row with no suggestion is skipped.
        setup.cx.simulate_keystrokes("up space");
        setup.cx.run_until_parked();
        let backlog = load(&setup, "backlog.md").await.unwrap();
        assert!(
            backlog.contains(
                "## Soon\n\n- [ ] Re: Q4 planning doc [[2026-10-09-1900-re-q4-planning-doc]]\n"
            ),
            "{backlog}"
        );
        assert_eq!(
            titles(&setup),
            ["Call Ana", "idea: routines could carry state"]
        );
    }

    #[gpui::test]
    async fn an_already_archived_name_leaves_the_row_and_says_so(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("archives/inbox"),
                json!({ "idea.md": "older copy\n" }),
            )
            .await;
        setup.cx.simulate_keystrokes("end e");
        setup.cx.run_until_parked();
        assert_eq!(load(&setup, "inbox/idea.md").await.as_deref(), Some(IDEA));
        assert_eq!(
            load(&setup, "archives/inbox/idea.md").await.as_deref(),
            Some("older copy\n")
        );
        assert_eq!(titles(&setup).len(), 4);
        // A no-op all the way down (§6.1): no log line, no decision, no
        // undo, and the row is still the selected one.
        assert_eq!(load(&setup, TRIAGE_LOG_PATH).await, None);
        assert_eq!(
            load(&setup, ".thock/state/inbox/decisions.jsonl").await,
            None
        );
        assert_eq!(
            selected(&setup).as_deref(),
            Some("idea: routines could carry state")
        );
        setup.view.read_with(&setup.cx, |view, _| {
            assert!(
                view.status
                    .as_ref()
                    .is_some_and(|status| status.contains("Already archived")),
                "{:?}",
                view.status
            );
            assert_eq!(view.filed_today, 0);
            assert!(view.last_decision.is_none());
        });
        // `t` on it writes no task either.
        setup.cx.simulate_keystrokes("t");
        setup.cx.run_until_parked();
        assert_eq!(
            load(&setup, &today_note_path()).await.as_deref(),
            Some(TODAY_NOTE)
        );
    }

    #[gpui::test]
    async fn the_keys_sheet_swallows_the_key_that_closes_it(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("?");
        setup.cx.run_until_parked();
        setup
            .view
            .read_with(&setup.cx, |view, _| assert!(view.keys_sheet));
        // A bound verb closes the sheet instead of filing.
        setup.cx.simulate_keystrokes("t");
        setup.cx.run_until_parked();
        setup
            .view
            .read_with(&setup.cx, |view, _| assert!(!view.keys_sheet));
        assert_eq!(titles(&setup).len(), 4);
        assert_eq!(
            load(&setup, &today_note_path()).await.as_deref(),
            Some(TODAY_NOTE)
        );
        // Now the verb works again.
        setup.cx.simulate_keystrokes("t");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 3);
    }

    #[gpui::test]
    async fn space_with_marks_files_each_row_to_its_own_chip(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91), (Q4, "soon", 0.71)],
                ..Options::default()
            },
        )
        .await;
        setup.cx.run_until_parked();
        // Call Ana (no chip), Q4 (Soon) and Ship it (Someday) marked.
        setup.cx.simulate_keystrokes("x down x down x");
        setup
            .view
            .read_with(&setup.cx, |view, _| assert_eq!(view.marked.len(), 3));
        setup.cx.simulate_keystrokes("space");
        setup.cx.run_until_parked();
        let backlog = load(&setup, "backlog.md").await.unwrap();
        assert!(
            backlog.contains("## Soon\n\n- [ ] Re: Q4 planning doc"),
            "{backlog}"
        );
        assert!(
            backlog.contains("## Someday\n\n- [ ] Ship it — a practical guide"),
            "{backlog}"
        );
        // The row without a suggestion is skipped and stays marked (§4.3).
        assert_eq!(
            titles(&setup),
            ["Call Ana", "idea: routines could carry state"]
        );
        assert!(
            load(&setup, "inbox/2026-10-10-0931-call-ana.md")
                .await
                .is_some()
        );
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(view.marked.len(), 1);
            assert!(view.marked.contains("2026-10-10-0931-call-ana"));
        });
        // One undo takes both back.
        setup.cx.simulate_keystrokes("u");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 4);
        assert_eq!(load(&setup, "backlog.md").await.unwrap(), DEFAULT_BACKLOG);
    }

    #[gpui::test]
    async fn quick_repeated_keys_file_one_row_each(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        // Three archives without letting the first finish writing: the
        // selection moves at the keystroke, so each targets the next row.
        setup.cx.simulate_keystrokes("e e e");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup), ["idea: routines could carry state"]);
        let log = load(&setup, TRIAGE_LOG_PATH).await.unwrap();
        assert_eq!(log.lines().count(), 3, "{log}");
        assert!(log.contains("Call Ana → Archived"), "{log}");
        assert!(log.contains("Re: Q4 planning doc → Archived"), "{log}");
        assert!(
            log.contains("Ship it — a practical guide → Archived"),
            "{log}"
        );
    }

    #[gpui::test]
    async fn a_decision_leaves_the_list_at_the_keystroke(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        assert_eq!(titles(&setup).len(), 4);
        // Hold the folder events back, so only the view can take the row away.
        setup.fs.pause_events();
        setup.view.update_in(&mut setup.cx, |view, window, cx| {
            view.archive(&ArchiveInboxItem, window, cx)
        });
        assert!(!titles(&setup).contains(&"Call Ana".to_string()));
        assert_eq!(selected(&setup).as_deref(), Some("Re: Q4 planning doc"));

        // The write lands before the folder event: the row stays gone.
        setup.cx.run_until_parked();
        assert!(
            load(&setup, "archives/inbox/2026-10-10-0931-call-ana.md")
                .await
                .is_some()
        );
        assert!(!titles(&setup).contains(&"Call Ana".to_string()));
        setup.fs.unpause_events_and_flush();
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 3);
        setup.view.read_with(&setup.cx, |view, _| {
            assert!(view.departed.is_empty());
        });

        // Undo brings it back.
        setup.cx.simulate_keystrokes("u");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 4);
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
    }

    // ----- Layout (rev 2) -----

    const BAR_BUTTONS: [&str; 4] = [
        "INBOX_BAR-today",
        "INBOX_BAR-soon",
        "INBOX_BAR-someday",
        "INBOX_BAR-archive",
    ];

    /// Re-renders the view and settles, so its elements are in the frame:
    /// the pane reuses an unchanged item's last frame, which drops its
    /// debug selectors.
    fn draw(setup: &mut Setup) {
        setup.view.update(&mut setup.cx, |_, cx| cx.notify());
        setup.cx.run_until_parked();
    }

    fn drawn(setup: &mut Setup, selector: &'static str) -> gpui::Bounds<Pixels> {
        draw(setup);
        setup
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
    }

    /// The bar's buttons left to right, as laid out.
    fn bar_order(setup: &mut Setup) -> Vec<&'static str> {
        let mut buttons: Vec<(&'static str, gpui::Bounds<Pixels>)> = BAR_BUTTONS
            .into_iter()
            .map(|selector| (selector, drawn(setup, selector)))
            .collect();
        buttons.sort_by(|a, b| f32::from(a.1.left()).total_cmp(&f32::from(b.1.left())));
        buttons.into_iter().map(|(selector, _)| selector).collect()
    }

    /// The bar button the `space` keycap is drawn on.
    fn space_button(setup: &mut Setup) -> Option<&'static str> {
        draw(setup);
        let space = setup.cx.debug_bounds("KEY_BINDING-space")?;
        BAR_BUTTONS.into_iter().find(|selector| {
            setup
                .cx
                .debug_bounds(selector)
                .is_some_and(|button| button.contains(&space.center()))
        })
    }

    fn select(setup: &mut Setup, title: &str) {
        setup.view.update(&mut setup.cx, |view, cx| {
            let position = view
                .visible
                .iter()
                .position(|entry| entry.item.title == title);
            view.select_position(position, cx);
        });
        setup.cx.run_until_parked();
        assert_eq!(selected(setup).as_deref(), Some(title));
    }

    #[gpui::test]
    async fn the_destination_bar_keeps_its_order_whatever_the_suggestion(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91), (Q4, "soon", 0.71)],
                ..Options::default()
            },
        )
        .await;
        setup.cx.run_until_parked();
        let mut first_layout: Option<Vec<gpui::Bounds<Pixels>>> = None;
        // Unsure (Call Ana), outlined Soon (Q4), filled Someday (Ship it).
        for (title, suggested, space_on) in [
            ("Call Ana", None, None),
            (
                "Re: Q4 planning doc",
                Some((Destination::Soon, Tier::Outlined)),
                Some("INBOX_BAR-soon"),
            ),
            (
                "Ship it — a practical guide",
                Some((Destination::Someday, Tier::Filled)),
                Some("INBOX_BAR-someday"),
            ),
        ] {
            select(&mut setup, title);
            assert_eq!(bar_order(&mut setup), BAR_BUTTONS, "{title}");
            // Moving between notes never resizes a button, wherever `space`
            // sits.
            let layout: Vec<gpui::Bounds<Pixels>> = BAR_BUTTONS
                .into_iter()
                .map(|selector| drawn(&mut setup, selector))
                .collect();
            let destinations = &layout[..4];
            assert!(
                destinations
                    .iter()
                    .all(|button| (button.size.width - destinations[0].size.width).abs() <= px(1.)),
                "{title}: the four destinations share the width equally: {destinations:?}"
            );
            match &first_layout {
                Some(first) => assert_eq!(&layout, first, "{title}"),
                None => first_layout = Some(layout),
            }
            setup.view.read_with(&setup.cx, |view, cx| {
                let state = view.bar_state(view.selected_entry().unwrap(), cx);
                assert_eq!(state.suggested, suggested, "{title}");
                assert!(state.probabilities.is_some(), "{title}");
            });
            assert_eq!(space_button(&mut setup), space_on, "{title}");
            // The percentage is never clipped by its button.
            for (button, percent) in [
                ("INBOX_BAR-today", "INBOX_PERCENT-today"),
                ("INBOX_BAR-soon", "INBOX_PERCENT-soon"),
                ("INBOX_BAR-someday", "INBOX_PERCENT-someday"),
                ("INBOX_BAR-archive", "INBOX_PERCENT-archive"),
            ] {
                let button = drawn(&mut setup, button);
                let percent = drawn(&mut setup, percent);
                assert!(
                    percent.right() + px(8.) <= button.right(),
                    "{title}: {percent:?} inside {button:?}"
                );
            }
            // Every option shows its probability whenever there is an
            // answer, an `unsure` one included.
            for selector in [
                "INBOX_PROBABILITY-today",
                "INBOX_PROBABILITY-soon",
                "INBOX_PROBABILITY-someday",
                "INBOX_PROBABILITY-archive",
            ] {
                assert!(
                    setup.cx.debug_bounds(selector).is_some(),
                    "{title}: {selector}"
                );
            }
        }
        setup.view.read_with(&setup.cx, |view, cx| {
            let ship_it = view.selected_entry().unwrap();
            let probabilities = view.bar_state(ship_it, cx).probabilities.unwrap();
            assert_eq!(probabilities, [0.0, 0.0, 0.91, 0.0]);
        });
    }

    #[gpui::test]
    async fn without_a_suggestion_space_is_on_no_button(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                cached: vec![(SHIP_IT, "someday", 0.91)],
                ..Options::default()
            },
        )
        .await;
        // A cached answer without a grant today shows nothing.
        select(&mut setup, "Ship it — a practical guide");
        assert_eq!(bar_order(&mut setup), BAR_BUTTONS);
        assert_eq!(space_button(&mut setup), None);
        assert_eq!(setup.cx.debug_bounds("KEY_BINDING-space"), None);
        assert_eq!(setup.cx.debug_bounds("INBOX_PROBABILITY-someday"), None);
        assert_eq!(setup.cx.debug_bounds("INBOX_PROBABILITY-today"), None);
        setup.view.read_with(&setup.cx, |view, cx| {
            let entry = view.selected_entry().unwrap();
            assert_eq!(view.bar_state(entry, cx), BarState::default());
            assert!(view.render_chip(entry, cx).is_none());
        });
    }

    #[gpui::test]
    async fn the_reader_does_not_repeat_the_title(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        // Call Ana's body is just `# Call Ana`, which the title above the
        // bar already says.
        select(&mut setup, "Call Ana");
        draw(&mut setup);
        setup.view.read_with(&setup.cx, |view, cx| {
            let markdown = view.reader_note.markdown.get(&(0, 0)).unwrap();
            assert_eq!(markdown.read(cx).source().as_ref(), "");
        });
    }

    #[gpui::test]
    async fn the_footer_shows_the_row_keys_for_moving(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        // The hint reads the bindings from the last frame, so draw twice.
        draw(&mut setup);
        draw(&mut setup);
        assert!(setup.cx.debug_bounds("KEY_BINDING-up").is_some());
        assert!(setup.cx.debug_bounds("KEY_BINDING-down").is_some());
        assert_eq!(setup.cx.debug_bounds("KEY_BINDING-p"), None);
        assert_eq!(setup.cx.debug_bounds("KEY_BINDING-n"), None);
        drop(setup);

        let mut setup = setup_vim(cx).await;
        draw(&mut setup);
        draw(&mut setup);
        assert!(setup.cx.debug_bounds("KEY_BINDING-k").is_some());
        assert!(setup.cx.debug_bounds("KEY_BINDING-j").is_some());
    }

    #[gpui::test]
    async fn clicking_a_destination_button_files_like_its_key(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        let backlog_before = load(&setup, "backlog.md").await.unwrap();
        select(&mut setup, "Re: Q4 planning doc");
        let soon = drawn(&mut setup, "INBOX_BAR-soon");
        setup
            .cx
            .simulate_click(soon.center(), gpui::Modifiers::none());
        setup.cx.run_until_parked();

        let backlog = load(&setup, "backlog.md").await.unwrap();
        assert!(
            backlog.contains(
                "## Soon\n\n- [ ] Re: Q4 planning doc [[2026-10-09-1900-re-q4-planning-doc]]\n"
            ),
            "{backlog}"
        );
        assert!(
            load(&setup, "inbox/2026-10-09-1900-re-q4-planning-doc.md")
                .await
                .is_none()
        );
        let log = load(&setup, TRIAGE_LOG_PATH).await.unwrap();
        assert!(
            log.contains("→ Backlog · Soon <!--inbox:a1a1a1a1a1a1-->"),
            "{log}"
        );
        let decisions = load(&setup, ".thock/state/inbox/decisions.jsonl")
            .await
            .unwrap();
        let record: DecisionRecord =
            serde_json::from_str(decisions.lines().next().unwrap()).unwrap();
        assert_eq!(record.chosen, "soon");
        assert_eq!(record.via, "s", "a click is recorded like its key");
        assert_eq!(
            selected(&setup).as_deref(),
            Some("Ship it — a practical guide")
        );
        assert!(view_focused(&mut setup), "the list keeps the keyboard");

        // Undo is a button too.
        let undo = drawn(&mut setup, "INBOX_ACTION-undo");
        setup
            .cx
            .simulate_click(undo.center(), gpui::Modifiers::none());
        setup.cx.run_until_parked();
        assert_eq!(load(&setup, "backlog.md").await.unwrap(), backlog_before);
        assert_eq!(titles(&setup).len(), 4);
    }

    #[gpui::test]
    async fn every_list_row_lays_out_at_the_measured_height(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91), (Q4, "soon", 0.71)],
                ..Options::default()
            },
        )
        .await;
        // Rows with and without a chip, an image glyph and a mark.
        setup.cx.simulate_keystrokes("end x");
        let rows: Vec<gpui::Bounds<Pixels>> =
            ["INBOX_ROW-0", "INBOX_ROW-1", "INBOX_ROW-2", "INBOX_ROW-3"]
                .into_iter()
                .map(|selector| drawn(&mut setup, selector))
                .collect();
        for (position, row) in rows.iter().enumerate() {
            assert_eq!(row.size.height, ROW_HEIGHT, "row {position}");
        }
        for pair in rows.windows(2) {
            assert!(pair[1].top() >= pair[0].bottom(), "rows never overlap");
        }
        let divider = drawn(&mut setup, "INBOX_DIVIDER");
        assert!(divider.size.height <= ROW_HEIGHT, "{divider:?}");
        assert!(divider.top() >= rows[2].bottom() && rows[3].top() >= divider.bottom());
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(
                view.layout().row(0),
                Some(crate::ui::sectioned_list::SectionedRow::Item {
                    section: 0,
                    index: 0
                })
            );
        });
    }

    #[gpui::test]
    async fn section_counts_follow_the_selection_and_the_filter(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        let counts = |setup: &Setup| {
            setup.view.read_with(&setup.cx, |view, _| {
                view.section_counts()
                    .into_iter()
                    .map(|count| (count.label, count.count, count.active))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            counts(&setup),
            [("New", 3, true), ("Carried over", 1, false)]
        );
        drawn(&mut setup, "INBOX_SECTION-new");
        drawn(&mut setup, "INBOX_SECTION-carried");
        setup.cx.simulate_keystrokes("end");
        assert_eq!(
            counts(&setup),
            [("New", 3, false), ("Carried over", 1, true)]
        );

        // A filter narrows the counts, and a section with no rows hides its
        // count.
        setup.cx.simulate_keystrokes("/");
        setup.cx.simulate_input("ana");
        setup.cx.run_until_parked();
        assert_eq!(counts(&setup), [("New", 1, true)]);
        drawn(&mut setup, "INBOX_SECTION-new");
        assert_eq!(setup.cx.debug_bounds("INBOX_SECTION-carried"), None);
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert_eq!(counts(&setup).len(), 2);
    }

    #[gpui::test]
    async fn the_destination_bar_stays_on_one_row_as_the_reader_narrows(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91)],
                ..Options::default()
            },
        )
        .await;
        // The suggested Someday is the widest button: `space or shift-s`.
        select(&mut setup, "Ship it — a practical guide");
        for width in [2400., 1600., 1300., 1100.] {
            setup.cx.simulate_resize(gpui::size(px(width), px(900.)));
            setup.cx.run_until_parked();
            let buttons: Vec<gpui::Bounds<Pixels>> = BAR_BUTTONS
                .into_iter()
                .map(|selector| drawn(&mut setup, selector))
                .collect();
            let first = buttons[0];
            for button in &buttons {
                assert_eq!(button.top(), first.top(), "{width}: one row");
                assert_eq!(button.size.height, BAR_BUTTON_HEIGHT, "{width}");
            }
            let archive = buttons[3];
            assert!(
                archive.right() <= px(width),
                "{width}: Archive fits in the window, {archive:?}"
            );
            let undo = drawn(&mut setup, "INBOX_ACTION-undo");
            assert!(
                undo.bottom() <= first.top(),
                "{width}: Undo stays out of the bar, {undo:?}"
            );
            for (button, percent) in [
                ("INBOX_BAR-today", "INBOX_PERCENT-today"),
                ("INBOX_BAR-soon", "INBOX_PERCENT-soon"),
                ("INBOX_BAR-someday", "INBOX_PERCENT-someday"),
                ("INBOX_BAR-archive", "INBOX_PERCENT-archive"),
            ] {
                let button = drawn(&mut setup, button);
                let percent = drawn(&mut setup, percent);
                assert!(
                    percent.right() + px(8.) <= button.right(),
                    "{width}: {percent:?} inside {button:?}"
                );
            }
            // The `space` keycap is drawn whole inside the suggested button.
            let someday = drawn(&mut setup, "INBOX_BAR-someday");
            let space = drawn(&mut setup, "KEY_BINDING-space");
            assert!(
                space.left() >= someday.left() && space.right() <= someday.right(),
                "{width}: {space:?} inside {someday:?}"
            );
        }
    }

    #[gpui::test]
    async fn a_button_keeps_its_name_clear_of_its_keys(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91)],
                ..Options::default()
            },
        )
        .await;
        select(&mut setup, "Ship it — a practical guide");
        for width in [1600., 1000.] {
            setup.cx.simulate_resize(gpui::size(px(width), px(900.)));
            setup.cx.run_until_parked();
            let label = drawn(&mut setup, "INBOX_LABEL-someday");
            let space = drawn(&mut setup, "KEY_BINDING-space");
            let keys = drawn(&mut setup, "INBOX_KEYS-someday");
            let meter = drawn(&mut setup, "INBOX_PROBABILITY-someday");
            let button = drawn(&mut setup, "INBOX_BAR-someday");
            assert!(
                space.top() >= label.bottom(),
                "{width}: {space:?} below {label:?}"
            );
            assert!(
                keys.bottom() + px(3.) <= meter.top(),
                "{width}: {meter:?} clear of {keys:?}"
            );
            let (above, below) = (label.top() - button.top(), button.bottom() - meter.bottom());
            assert!(
                (above - below).abs() <= px(2.),
                "{width}: {above:?} above and {below:?} below"
            );
            for (selector, tile) in [
                ("INBOX_PROBABILITY-today", "INBOX_BAR-today"),
                ("INBOX_PROBABILITY-soon", "INBOX_BAR-soon"),
                ("INBOX_PROBABILITY-someday", "INBOX_BAR-someday"),
                ("INBOX_PROBABILITY-archive", "INBOX_BAR-archive"),
            ] {
                let bounds = drawn(&mut setup, selector);
                let tile = drawn(&mut setup, tile);
                assert_eq!(
                    bounds.left() - tile.left(),
                    tile.right() - bounds.right(),
                    "{width}: {selector} fills {tile:?}"
                );
                assert!(
                    (bounds.size.width - meter.size.width).abs() <= px(1.),
                    "{width}: {selector} {bounds:?}"
                );
            }
            assert!(
                space.bottom() <= button.bottom() && meter.bottom() <= button.bottom(),
                "{width}: {space:?} and {meter:?} inside {button:?}"
            );
        }
    }

    #[gpui::test]
    async fn the_destination_bar_wraps_rather_than_squeeze_a_button(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91)],
                ..Options::default()
            },
        )
        .await;
        select(&mut setup, "Ship it — a practical guide");
        for width in [1000., 700.] {
            setup.cx.simulate_resize(gpui::size(px(width), px(900.)));
            setup.cx.run_until_parked();
            for selector in [
                "INBOX_BAR-today",
                "INBOX_BAR-soon",
                "INBOX_BAR-someday",
                "INBOX_BAR-archive",
            ] {
                let button = drawn(&mut setup, selector);
                assert!(
                    button.size.width >= DESTINATION_MIN_WIDTH,
                    "{width}: {selector} {button:?}"
                );
                assert!(
                    button.right() <= px(width),
                    "{width}: {selector} {button:?}"
                );
            }
            let undo = drawn(&mut setup, "INBOX_ACTION-undo");
            assert!(undo.right() <= px(width), "{width}: {undo:?}");
        }
    }

    #[gpui::test]
    async fn the_inbox_takes_the_room_between_the_docks(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_resize(gpui::size(px(1600.), px(900.)));
        let column = px(700.);
        setup.workspace.update(&mut setup.cx, |workspace, cx| {
            workspace.centered_layout = true;
            workspace.set_center_anchor(
                Some(workspace::CenterAnchor {
                    column,
                    min_column: px(500.),
                    thread: None,
                    left: workspace::TideSide::default(),
                    right: workspace::TideSide::default(),
                }),
                cx,
            );
        });
        setup.cx.run_until_parked();
        setup.cx.update(|window, _| window.refresh());
        setup.cx.run_until_parked();
        let layout = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.anchor_layout())
            .expect("the page is anchored");
        assert!(layout.column > px(1500.), "{layout:?}");
    }

    async fn setup_with_ticket(cx: &mut TestAppContext) -> Setup {
        let mut setup = setup(cx, Options::default()).await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("inbox"),
                json!({ "2026-10-09-1930-ticket-42.md": TICKET }),
            )
            .await;
        setup.cx.run_until_parked();
        select(&mut setup, "Ticket 42");
        setup
    }

    async fn setup_with_note(cx: &mut TestAppContext, note: String) -> Setup {
        let mut setup = setup(cx, Options::default()).await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("inbox"),
                json!({ "2026-10-09-1930-ticket-42.md": note }),
            )
            .await;
        setup.cx.run_until_parked();
        select(&mut setup, "Ticket 42");
        setup
    }

    #[gpui::test]
    async fn a_card_has_one_fold_row_under_its_changed_fields(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        setup.view.read_with(&setup.cx, |view, _| {
            let thread = view.reader_note.thread.as_ref().unwrap();
            assert!(matches!(
                thread.messages[1].blocks[1..],
                [EmailBlock::Fold { .. }, EmailBlock::Fields { .. }]
            ));
        });
        let row = drawn(&mut setup, "INBOX_FOLD-1");
        let changed = drawn(&mut setup, "INBOX_CHANGED-1-2-0");
        assert!(row.top() >= changed.bottom(), "{row:?} under {changed:?}");
        let key = drawn(&mut setup, "KEY_BINDING-z");
        assert!(row.contains(&key.center()), "{key:?} in {row:?}");

        // The keycap leads the words, as on every other hint.
        assert_eq!(setup.cx.debug_bounds("INBOX_FOLD_TOP-1"), None);
        setup.cx.simulate_keystrokes("z");
        let row = drawn(&mut setup, "INBOX_FOLD-1");
        assert!(row.top() >= changed.bottom(), "{row:?} under {changed:?}");

        // Open, the same row heads the card too.
        let top = drawn(&mut setup, "INBOX_FOLD_TOP-1");
        let card = drawn(&mut setup, "INBOX_MESSAGE-1");
        assert!(
            top.top() > card.top() && top.bottom() < row.top(),
            "{top:?}"
        );
        setup.cx.simulate_keystrokes("z");
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_FOLD_TOP-1"), None);
    }

    #[gpui::test]
    async fn a_changed_field_says_what_it_was_on_its_own_line(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        let was = drawn(&mut setup, "INBOX_CHANGED-1-2-0");
        let row = drawn(&mut setup, "INBOX_FOLD-1");
        assert!(was.size.height < px(30.), "{was:?} is one line");
        assert!(row.top() - was.bottom() < px(30.), "{row:?} right under");
    }

    #[gpui::test]
    async fn a_long_list_holds_one_line_until_the_folds_open(cx: &mut TestAppContext) {
        let note = TICKET.replace(
            "Status:  IN PROGRESS\n\nTeam:  render-ops",
            "Status:  IN PROGRESS\n\nTeam:  a,b,c,d,e,f",
        );
        let mut setup = setup_with_note(cx, note).await;
        let status_changed = "INBOX_CHANGED-1-2-0";
        drawn(&mut setup, status_changed);
        drawn(&mut setup, "INBOX_MORE-1-2-1");

        setup.cx.simulate_keystrokes("z");
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_MORE-1-2-1"), None);
    }

    #[gpui::test]
    async fn the_cue_says_what_sits_above_a_revealed_card(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_EARLIER"), None);

        setup.cx.simulate_resize(gpui::size(px(1200.), px(420.)));
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("shift-n n");
        draw(&mut setup);
        draw(&mut setup);
        let cue = drawn(&mut setup, "INBOX_EARLIER");
        let card = drawn(&mut setup, "INBOX_MESSAGE-1");
        assert!(card.top() >= cue.bottom(), "{card:?} under {cue:?}");
        let key = drawn(&mut setup, "KEY_BINDING-n");
        assert!(cue.contains(&key.center()), "{key:?} on {cue:?}");
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(view.reader_note.messages_above, 1);
        });

        // Scrolled further into the card, its header has gone above the line.
        setup.cx.simulate_keystrokes("tab j");
        draw(&mut setup);
        draw(&mut setup);
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(view.reader_note.messages_above, 2);
        });

        // A taller reader takes the scroll away during layout, after render
        // has read it, and the cue still goes.
        setup.cx.simulate_resize(gpui::size(px(1200.), px(1400.)));
        setup.cx.run_until_parked();
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_EARLIER"), None);
        let reader = setup
            .view
            .read_with(&setup.cx, |view, _| view.reader_scroll.bounds());
        let first = drawn(&mut setup, "INBOX_MESSAGE-0");
        assert!(first.top() >= reader.top(), "{first:?} whole in {reader:?}");
    }

    /// A four-message thread: each card repeats the ticket's record, so
    /// opening the folds makes every card tall.
    fn long_thread() -> String {
        let mut note = "---\nsource:   gmail\ncapture:  e5e5e5e5e5e5\ncaptured: 2026-10-09T19:30:00Z\ntitle:    Ticket 42\nfrom:     helpdesk@example.com\n---\n\n# Ticket 42\n".to_string();
        for number in 1..=4 {
            note.push_str(&format!(
                "\n## helpdesk@example.com — 2026-10-09 0{number}:00\n\n\
                 Comment #{number} (by ana on 10/09/26 0{number}:00am):\n\n\
                 Word {number}.\n\n\
                 The farm stopped.\nCould someone look?\nIt's urgent.\n\n\
                 Status:  OPEN\n\nTeam:  render-ops\n\nHost:  a1\n"
            ));
        }
        note
    }

    fn cards_over_the_cue(setup: &mut Setup) -> usize {
        let cue = drawn(setup, "INBOX_EARLIER");
        [
            "INBOX_MESSAGE-0",
            "INBOX_MESSAGE-1",
            "INBOX_MESSAGE-2",
            "INBOX_MESSAGE-3",
        ]
        .into_iter()
        .filter(|selector| drawn(setup, selector).top() < cue.bottom())
        .count()
    }

    #[gpui::test]
    async fn the_cue_counts_the_headers_above_it_with_the_folds_open(cx: &mut TestAppContext) {
        let mut setup = setup_with_note(cx, long_thread()).await;
        setup.cx.simulate_resize(gpui::size(px(1200.), px(620.)));
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("z");
        draw(&mut setup);
        draw(&mut setup);
        assert_eq!(current_message(&setup), 3);
        let above = setup
            .view
            .read_with(&setup.cx, |view, _| view.reader_note.messages_above);
        assert!(above >= 2, "{above}");
        assert_eq!(above, cards_over_the_cue(&mut setup));

        // Up the thread, a card running on below the cue is above it by its
        // header, and the count falls as the headers come back.
        setup.cx.simulate_keystrokes("tab");
        let mut seen = HashSet::new();
        for _ in 0..40 {
            setup.cx.simulate_keystrokes("k");
            draw(&mut setup);
            draw(&mut setup);
            let above = setup
                .view
                .read_with(&setup.cx, |view, _| view.reader_note.messages_above);
            if above == 0 {
                break;
            }
            assert_eq!(above, cards_over_the_cue(&mut setup));
            seen.insert(above);
        }
        assert!(seen.contains(&2), "{seen:?}");
    }

    #[gpui::test]
    async fn a_thread_lands_on_a_card_boundary(cx: &mut TestAppContext) {
        let mut setup = setup_with_note(cx, long_thread()).await;
        // Too short for the newest card to reach the top only by a little.
        setup.cx.simulate_resize(gpui::size(px(1200.), px(620.)));
        setup.cx.run_until_parked();
        draw(&mut setup);
        draw(&mut setup);
        let cue = setup.cx.debug_bounds("INBOX_EARLIER");
        let reader = setup
            .view
            .read_with(&setup.cx, |view, _| view.reader_scroll.bounds());
        let line = cue.map_or(reader.top(), |cue| cue.bottom());
        for selector in [
            "INBOX_MESSAGE-0",
            "INBOX_MESSAGE-1",
            "INBOX_MESSAGE-2",
            "INBOX_MESSAGE-3",
        ] {
            let card = drawn(&mut setup, selector);
            assert!(
                card.bottom() <= line || card.top() >= line,
                "{selector}: {card:?} is whole on one side of {line:?}"
            );
        }
    }

    #[test]
    fn a_landing_snaps_to_a_card_it_can_reach() {
        // The newest card reaches its place.
        assert_eq!(landing(&[px(0.), px(50.)], px(80.), px(400.)), px(50.));
        // It can't, so the card before takes the place.
        assert_eq!(
            landing(&[px(0.), px(40.), px(100.), px(160.)], px(120.), px(400.)),
            px(100.)
        );
        // That card is so tall the current one would drop off the screen.
        assert_eq!(
            landing(&[px(0.), px(40.), px(900.)], px(860.), px(400.)),
            px(860.)
        );
    }

    #[gpui::test]
    async fn the_keys_of_the_header_dim_with_their_words(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        select(&mut setup, "Ship it — a practical guide");
        draw(&mut setup);
        for dimmed in [
            "INBOX_DIMMED-open",
            "INBOX_DIMMED-undo",
            "INBOX_DIMMED-link",
        ] {
            assert_eq!(setup.cx.debug_bounds(dimmed), None, "{dimmed}");
        }

        // `l` still works in the reader, and only there of the three.
        setup.cx.simulate_keystrokes("tab");
        drawn(&mut setup, "INBOX_DIMMED-open");
        drawn(&mut setup, "INBOX_DIMMED-undo");
        assert_eq!(setup.cx.debug_bounds("INBOX_DIMMED-link"), None);

        setup.cx.simulate_keystrokes("tab /");
        drawn(&mut setup, "INBOX_DIMMED-link");
    }

    #[gpui::test]
    async fn a_reader_column_ends_on_one_right_edge(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        for width in [1000., 2400.] {
            setup.cx.simulate_resize(gpui::size(px(width), px(900.)));
            setup.cx.run_until_parked();
            draw(&mut setup);
            let archive = drawn(&mut setup, "INBOX_BAR-archive");
            let card = drawn(&mut setup, "INBOX_MESSAGE-1");
            let title = drawn(&mut setup, "INBOX_READER_TITLE");
            assert_eq!(archive.right(), card.right(), "{width}");
            assert_eq!(archive.right(), title.right(), "{width}");
            let link = setup.cx.debug_bounds("INBOX_ACTION-link");
            let last = link.unwrap_or_else(|| drawn(&mut setup, "INBOX_ACTION-undo"));
            assert_eq!(last.right() - ACTION_PADDING, archive.right(), "{width}");
        }
    }

    #[gpui::test]
    async fn the_header_keeps_the_sender_whole_at_macbook_width(cx: &mut TestAppContext) {
        let note = TICKET.replace("helpdesk@example.com", "helpdesk_submit@example.com");
        let mut setup = setup_with_note(cx, note).await;
        let wide = gpui::size(px(2400.), px(900.));
        setup.cx.simulate_resize(wide);
        setup.cx.run_until_parked();
        draw(&mut setup);
        let roomy = drawn(&mut setup, "INBOX_HEADER_SENDER");
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(view.header_fit, HeaderFit::Roomy);
        });

        // A MacBook's reader, between the docks, in rems.
        setup.cx.simulate_resize(gpui::size(px(700.), px(900.)));
        setup.cx.run_until_parked();
        draw(&mut setup);
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(view.header_fit, HeaderFit::Tight);
        });
        let tight = drawn(&mut setup, "INBOX_HEADER_SENDER");
        let when = drawn(&mut setup, "INBOX_HEADER_WHEN");
        let open = drawn(&mut setup, "INBOX_ACTION-open");
        assert!(tight.size.width < roomy.size.width, "the domain has gone");
        assert!(tight.right() <= when.left(), "{tight:?} then {when:?}");
        assert!(when.right() <= open.left(), "{when:?} then {open:?}");
    }

    #[test]
    fn the_header_trades_its_glyph_before_the_senders_room() {
        // A MacBook's reader at the owner's interface size; a Dell's.
        assert_eq!(HeaderFit::for_width(34.2), HeaderFit::Tight);
        assert_eq!(HeaderFit::for_width(40.), HeaderFit::Plain);
        assert_eq!(HeaderFit::for_width(75.), HeaderFit::Roomy);
    }

    #[gpui::test]
    async fn the_header_gives_way_from_the_sender_and_never_the_date(cx: &mut TestAppContext) {
        let note = TICKET.replace(
            "from:     helpdesk@example.com",
            "from:     a_very_long_helpdesk_submission_queue@example.com",
        );
        let mut setup = setup_with_note(cx, note).await;
        setup.cx.simulate_resize(gpui::size(px(1000.), px(900.)));
        setup.cx.run_until_parked();
        draw(&mut setup);
        let sender = drawn(&mut setup, "INBOX_HEADER_SENDER");
        assert!(sender.size.width > px(50.), "{sender:?}");
        let open = drawn(&mut setup, "INBOX_ACTION-open");
        let when = drawn(&mut setup, "INBOX_HEADER_WHEN");
        assert!(sender.right() <= when.left(), "{sender:?} then {when:?}");
        assert!(
            when.right() <= open.left(),
            "{when:?} whole beside {open:?}"
        );

        setup
            .view
            .update(&mut setup.cx, |view, _| view.list_fraction = 0.5);
        setup.cx.run_until_parked();
        let narrow = drawn(&mut setup, "INBOX_HEADER_SENDER");
        let when_narrow = drawn(&mut setup, "INBOX_HEADER_WHEN");
        assert!(narrow.size.width < sender.size.width, "{narrow:?}");
        assert_eq!(when_narrow.size.width, when.size.width);
    }

    #[gpui::test]
    async fn the_link_has_a_key_and_a_note_without_one_does_not(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        select(&mut setup, "Ship it — a practical guide");
        let link = drawn(&mut setup, "INBOX_ACTION-link");
        let key = drawn(&mut setup, "KEY_BINDING-l");
        assert!(link.contains(&key.center()), "{key:?} on {link:?}");
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(
                view.selected_link().as_deref(),
                Some("https://example.com/ship-it")
            );
        });

        // The link's key still answers from the reader.
        setup.cx.simulate_keystrokes("tab");
        draw(&mut setup);
        setup.cx.update(|window, _| {
            let bound = window
                .highest_precedence_binding_for_action_in_context(
                    &OpenInboxLink,
                    reader_key_context(),
                )
                .expect("bound in the reader");
            assert_eq!(bound.keystrokes().len(), 1);
        });
        setup.cx.simulate_keystrokes("tab");

        select(&mut setup, "Call Ana");
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_ACTION-link"), None);
        // With no link the key does nothing, and nothing breaks.
        setup.cx.simulate_keystrokes("l");
        assert_eq!(selected(&setup).as_deref(), Some("Call Ana"));
    }

    #[gpui::test]
    async fn a_lone_section_is_a_plain_count(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        let button = drawn(&mut setup, "INBOX_SECTION-new");
        setup.cx.simulate_keystrokes("/");
        setup.cx.simulate_input("ana");
        setup.cx.run_until_parked();
        let plain = drawn(&mut setup, "INBOX_SECTION-new");
        assert!(plain.size.height < button.size.height, "{plain:?}");
    }

    #[gpui::test]
    async fn the_slot_above_the_bar_says_what_the_percentages_are(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(SHIP_IT, "someday", 0.91)],
                ..Options::default()
            },
        )
        .await;
        select(&mut setup, "Ship it — a practical guide");
        drawn(&mut setup, "INBOX_SUGGESTED");
        setup.cx.simulate_keystrokes("x");
        drawn(&mut setup, "INBOX_MARKS_NOTE");
        assert_eq!(setup.cx.debug_bounds("INBOX_SUGGESTED"), None);
    }

    #[gpui::test]
    async fn the_slot_says_a_lean_as_a_lean(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(Q4, "soon", 0.45)],
                ..Options::default()
            },
        )
        .await;
        select(&mut setup, "Re: Q4 planning doc");
        drawn(&mut setup, "INBOX_SUGGESTED");
        setup.view.read_with(&setup.cx, |view, cx| {
            let state = view.bar_state(view.selected_entry().unwrap(), cx);
            assert_eq!(bar_caption(&state).as_deref(), Some("Leaning Soon"));
        });
    }

    #[test]
    fn the_caption_promises_only_what_the_bar_shows() {
        let suggested = BarState {
            suggested: Some((Destination::Soon, Tier::Outlined)),
            ..BarState::default()
        };
        assert_eq!(bar_caption(&suggested).as_deref(), Some("Suggested: Soon"));
        let leaning = BarState {
            leaning: Some(Destination::Someday),
            ..BarState::default()
        };
        assert_eq!(bar_caption(&leaning).as_deref(), Some("Leaning Someday"));
        let tie = BarState {
            probabilities: Some([0.4, 0.4, 0.1, 0.1]),
            ..BarState::default()
        };
        assert_eq!(bar_caption(&tie), None);
    }

    #[gpui::test]
    async fn no_percentages_leave_the_slot_above_the_bar_empty(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_SUGGESTED"), None);
    }

    #[gpui::test]
    async fn an_empty_list_keeps_suggestions_out_of_its_footer(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                ..Options::default()
            },
        )
        .await;
        let says_on = |setup: &Setup| {
            setup.view.read_with(&setup.cx, |view, cx| {
                matches!(view.footer_stats(cx), FooterStats::Stats(text)
                    if text.contains("Suggestions on"))
            })
        };
        assert!(says_on(&setup));
        setup.cx.simulate_keystrokes("/");
        setup.cx.simulate_input("nothing is called this");
        setup.cx.run_until_parked();
        assert!(!says_on(&setup));
    }

    #[gpui::test]
    async fn system_lines_read_a_step_under_the_note(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        draw(&mut setup);
        let view = setup.view.clone();
        setup.cx.update(|window, cx| {
            let style = view.read(cx).reader_style(window, cx);
            assert_eq!(
                style.system.container_style.text.font_size,
                Some(SYSTEM_LINE_SIZE.into())
            );
            assert_eq!(
                style.system.paragraph_line_height,
                SYSTEM_LINE_HEIGHT.into()
            );
        });
    }

    #[gpui::test]
    async fn a_suggested_outline_dims_with_its_keys(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.update(|_, cx| {
            for look in [BarLook::Filled, BarLook::Outlined] {
                let (_, live, _) = bar_colors(look, false, cx);
                let (_, off, _) = bar_colors(look, true, cx);
                assert!(off.a < live.a, "{look:?}");
            }
        });
    }

    #[gpui::test]
    async fn the_bar_stays_put_whatever_the_title_and_the_marks(cx: &mut TestAppContext) {
        let long = "A subject long enough to need the title's second line ".repeat(3);
        let note = TICKET.replace("# Ticket 42\n", &format!("# {long}\n"));
        let note = note.replace("title:    Ticket 42", &format!("title:    {long}"));
        let mut setup = setup(cx, Options::default()).await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("inbox"),
                json!({ "2026-10-09-1930-long.md": note }),
            )
            .await;
        setup.cx.run_until_parked();
        setup.cx.simulate_resize(gpui::size(px(1000.), px(900.)));
        setup.cx.run_until_parked();

        select(&mut setup, "Call Ana");
        let bar = drawn(&mut setup, "INBOX_BAR-today");
        select(&mut setup, long.trim());
        assert_eq!(drawn(&mut setup, "INBOX_BAR-today"), bar);
        setup.cx.simulate_keystrokes("x");
        drawn(&mut setup, "INBOX_MARKS_NOTE");
        assert_eq!(drawn(&mut setup, "INBOX_BAR-today"), bar);
    }

    #[gpui::test]
    async fn a_ticket_row_shows_its_number_where_the_sender_goes(cx: &mut TestAppContext) {
        const NUMBERED: &str = "---\nsource:   gmail\ncapture:  d4d4d4d4d4d4\ncaptured: 2026-10-09T19:40:00Z\ntitle:    HD 7: Backups not restarted\nfrom:     helpdesk@example.com\n---\n\n# HD 7: Backups not restarted\n\nThe disk is full.\n";
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(NUMBERED, "soon", 0.91)],
                ..Options::default()
            },
        )
        .await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("inbox"),
                json!({ "2026-10-09-1940-hd-7.md": NUMBERED }),
            )
            .await;
        setup.cx.run_until_parked();
        select(&mut setup, "HD 7: Backups not restarted");
        let position = setup
            .view
            .read_with(&setup.cx, |view, _| view.selected_index().unwrap());
        setup.cx.simulate_resize(gpui::size(px(1000.), px(900.)));
        setup
            .view
            .update(&mut setup.cx, |view, _| view.list_fraction = 0.2);
        setup.cx.run_until_parked();
        draw(&mut setup);
        let selector = |name: &str| -> &'static str {
            Box::leak(format!("{name}-{position}").into_boxed_str())
        };
        let chip = drawn(&mut setup, selector("INBOX_CHIP"));
        let age = drawn(&mut setup, selector("INBOX_AGE"));
        let row = drawn(&mut setup, selector("INBOX_ROW"));
        assert!(chip.right() <= age.left(), "{chip:?} before {age:?}");
        assert!(age.right() <= row.right(), "{age:?} in {row:?}");
    }

    #[gpui::test]
    async fn a_ticket_number_outlasts_the_suggestion_beside_it(cx: &mut TestAppContext) {
        const LONG_NUMBER: &str = "---\nsource:   gmail\ncapture:  d4d4d4d4d4d4\ncaptured: 2026-10-09T19:40:00Z\ntitle:    HD 5197591234: Backups not restarted\nfrom:     helpdesk@example.com\n---\n\n# HD 5197591234: Backups not restarted\n\nThe disk is full.\n";
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(LONG_NUMBER, "someday", 0.91)],
                ..Options::default()
            },
        )
        .await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("inbox"),
                json!({ "2026-10-09-1940-hd-519759.md": LONG_NUMBER }),
            )
            .await;
        setup.cx.run_until_parked();
        select(&mut setup, "HD 5197591234: Backups not restarted");
        let position = setup
            .view
            .read_with(&setup.cx, |view, _| view.selected_index().unwrap());
        let selector = |name: &str| -> &'static str {
            Box::leak(format!("{name}-{position}").into_boxed_str())
        };
        setup.cx.simulate_resize(gpui::size(px(1600.), px(900.)));
        setup.cx.run_until_parked();
        draw(&mut setup);
        let number = drawn(&mut setup, selector("INBOX_ORIGIN"));
        let extras = drawn(&mut setup, selector("INBOX_ROW_EXTRAS"));
        let chip = drawn(&mut setup, selector("INBOX_CHIP"));
        assert!(chip.top() < extras.bottom(), "{chip:?} shows in {extras:?}");

        // Narrowed until the two can't share the line: the number stays whole
        // and the suggestion goes, whole, below the clipped line.
        let age_width = drawn(&mut setup, selector("INBOX_AGE")).size.width;
        let row_width = drawn(&mut setup, selector("INBOX_ROW")).size.width;
        let room = number.size.width + age_width + chip.size.width / 2. + px(50.);
        let fraction = setup
            .view
            .read_with(&setup.cx, |view, _| view.list_fraction * (room / row_width));
        setup
            .view
            .update(&mut setup.cx, |view, _| view.list_fraction = fraction);
        setup.cx.run_until_parked();
        draw(&mut setup);
        let narrow = drawn(&mut setup, selector("INBOX_ORIGIN"));
        let extras = drawn(&mut setup, selector("INBOX_ROW_EXTRAS"));
        let chip = drawn(&mut setup, selector("INBOX_CHIP"));
        assert_eq!(narrow.size.width, number.size.width, "the number is whole");
        assert!(chip.top() >= extras.bottom(), "{chip:?} past {extras:?}");
    }

    #[gpui::test]
    async fn the_filter_box_sits_inside_the_list_with_its_count(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_resize(gpui::size(px(1000.), px(900.)));
        setup
            .view
            .update(&mut setup.cx, |view, _| view.list_fraction = 0.2);
        setup.cx.run_until_parked();
        let list_right = drawn(&mut setup, "INBOX_ROW-0").right();
        setup.cx.simulate_keystrokes("/");
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_FILTER_COUNT"), None);
        // One section's count in the header already says it.
        setup.cx.simulate_input("ship");
        setup.cx.run_until_parked();
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_FILTER_COUNT"), None);
        setup.cx.simulate_keystrokes("escape /");

        // Across both sections, none does.
        setup.cx.simulate_input("i");
        setup.cx.run_until_parked();
        let filter = drawn(&mut setup, "INBOX_FILTER");
        assert!(filter.right() <= list_right + px(8.), "{filter:?}");
        drawn(&mut setup, "INBOX_FILTER_COUNT");
        drawn(&mut setup, "KEY_BINDING-escape");
    }

    #[gpui::test]
    async fn a_list_with_nothing_to_move_through_offers_only_the_keys(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("/");
        setup.cx.simulate_input("nothing is called this");
        setup.cx.run_until_parked();
        drawn(&mut setup, "INBOX_HINT-keys");
        assert_eq!(setup.cx.debug_bounds("INBOX_HINT-move"), None);
        assert_eq!(setup.cx.debug_bounds("INBOX_ACTION-open"), None);
    }

    #[gpui::test]
    async fn the_keys_sheet_groups_keys_by_what_they_do(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("?");
        setup.cx.simulate_resize(gpui::size(px(2400.), px(900.)));
        setup.cx.run_until_parked();
        let decide = drawn(&mut setup, "INBOX_KEYS_GROUP-Decide");
        let moving = drawn(&mut setup, "INBOX_KEYS_GROUP-Move");
        let read = drawn(&mut setup, "INBOX_KEYS_GROUP-Read");
        assert_eq!(decide.top(), moving.top(), "columns where the width allows");
        assert_eq!(decide.top(), read.top());
        assert!(decide.right() <= moving.left() && moving.right() <= read.left());

        // Without the width, a group wraps under the ones before it.
        setup.cx.simulate_resize(gpui::size(px(1000.), px(900.)));
        setup.cx.run_until_parked();
        let decide = drawn(&mut setup, "INBOX_KEYS_GROUP-Decide");
        let read = drawn(&mut setup, "INBOX_KEYS_GROUP-Read");
        assert!(decide.bottom() <= read.top(), "{decide:?} over {read:?}");
    }

    #[gpui::test]
    async fn an_empty_inbox_is_all_clear_and_can_still_undo(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        assert_eq!(setup.cx.debug_bounds("INBOX_ALL_CLEAR"), None);
        for _ in 0..titles(&setup).len() {
            setup.cx.simulate_keystrokes("e");
            setup.cx.run_until_parked();
        }
        assert!(titles(&setup).is_empty());
        let clear = drawn(&mut setup, "INBOX_ALL_CLEAR");
        let header = drawn(&mut setup, "INBOX_ACTION-undo");
        assert!(header.bottom() <= clear.top(), "{clear:?} in the reader");
        assert_eq!(setup.cx.debug_bounds("INBOX_ACTION-open"), None);

        setup.cx.simulate_keystrokes("u");
        setup.cx.run_until_parked();
        assert_eq!(titles(&setup).len(), 1);
        assert_eq!(setup.cx.debug_bounds("INBOX_ALL_CLEAR"), None);
    }

    #[gpui::test]
    async fn a_filter_with_no_matches_is_not_all_clear(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_keystrokes("/");
        setup.cx.simulate_input("nothing is called this");
        setup.cx.run_until_parked();
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_ALL_CLEAR"), None);
    }

    #[gpui::test]
    async fn a_thread_reads_as_a_card_per_message(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        let first = drawn(&mut setup, "INBOX_MESSAGE-0");
        let second = drawn(&mut setup, "INBOX_MESSAGE-1");
        assert!(second.top() >= first.bottom(), "{first:?} then {second:?}");
        setup.view.read_with(&setup.cx, |view, _| {
            let thread = view.reader_note.thread.as_ref().unwrap();
            assert_eq!(thread.messages[1].author.as_deref(), Some("jlopes"));
        });
    }

    #[gpui::test]
    async fn a_cut_subject_reads_whole_in_the_reader_and_stays_cut_in_the_note(
        cx: &mut TestAppContext,
    ) {
        const CUT: &str = "---\nsource:   gmail\ncapture:  c3c3c3c3c3c3\ncaptured: 2026-10-09T19:40:00Z\ntitle:    HD 7: Backups not restarted when t...\nfrom:     helpdesk@example.com\n---\n\n# HD 7: Backups not restarted when t...\n\nTicket 7:Backups not restarted when the disk is full | FS-1\n";
        let mut setup = setup(cx, Options::default()).await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("inbox"),
                json!({ "2026-10-09-1940-hd-7.md": CUT }),
            )
            .await;
        setup.cx.run_until_parked();
        select(&mut setup, "HD 7: Backups not restarted when t...");
        draw(&mut setup);
        setup.view.read_with(&setup.cx, |view, _| {
            assert_eq!(
                view.reader_note.title.as_deref(),
                Some("HD 7: Backups not restarted when the disk is full | FS-1")
            );
            // The row reads it whole too: only its width may cut it.
            assert_eq!(
                whole_title(&view.selected_entry().unwrap().item).as_deref(),
                view.reader_note.title.as_deref()
            );
            let thread = view.reader_note.thread.as_ref().unwrap();
            assert!(
                thread.messages[0].blocks.is_empty(),
                "the body's restated subject stays out of the reader"
            );
        });
        assert_eq!(
            load(&setup, "inbox/2026-10-09-1940-hd-7.md")
                .await
                .as_deref(),
            Some(CUT)
        );
    }

    #[gpui::test]
    async fn only_an_email_reads_as_a_thread(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        select(&mut setup, "Call Ana");
        draw(&mut setup);
        setup.view.read_with(&setup.cx, |view, _| {
            assert!(view.reader_note.thread.is_none());
        });
    }

    fn current_message(setup: &Setup) -> usize {
        setup
            .view
            .read_with(&setup.cx, |view, _| view.reader_note.current_message)
    }

    #[gpui::test]
    async fn a_thread_opens_on_its_newest_message(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        assert_eq!(current_message(&setup), 1);
        let older = drawn(&mut setup, "INBOX_MESSAGE-0");
        let newest = drawn(&mut setup, "INBOX_MESSAGE-1");
        assert!(
            older.size.height < newest.size.height,
            "{older:?} folds to its header above {newest:?}"
        );
        assert_eq!(setup.cx.debug_bounds("INBOX_FOLD-0"), None);

        // `z` opens every card, and closing it leaves the newest open.
        setup.cx.simulate_keystrokes("z");
        drawn(&mut setup, "INBOX_FOLD-0");
        setup.cx.simulate_keystrokes("z");
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_FOLD-0"), None);
        assert_eq!(current_message(&setup), 1);
    }

    #[gpui::test]
    async fn n_walks_the_messages_from_the_list(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        setup.cx.simulate_keystrokes("n");
        assert_eq!(current_message(&setup), 1, "the newest is the last");
        setup.cx.simulate_keystrokes("shift-n");
        draw(&mut setup);
        assert_eq!(current_message(&setup), 0);
        drawn(&mut setup, "INBOX_FOLD-0");
        assert_eq!(selected(&setup).as_deref(), Some("Ticket 42"));
        setup.cx.simulate_keystrokes("shift-n");
        assert_eq!(current_message(&setup), 0, "the first stays the first");
        setup.cx.simulate_keystrokes("n");
        draw(&mut setup);
        assert_eq!(current_message(&setup), 1);
    }

    #[gpui::test]
    async fn clicking_a_collapsed_message_opens_it(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        let older = drawn(&mut setup, "INBOX_MESSAGE-0");
        setup
            .cx
            .simulate_click(older.center(), gpui::Modifiers::none());
        setup.cx.run_until_parked();
        assert_eq!(current_message(&setup), 0);
    }

    #[gpui::test]
    async fn n_brings_the_message_into_view_whatever_the_interface_size(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        setup.cx.simulate_resize(gpui::size(px(1200.), px(420.)));
        setup
            .cx
            .update(|_, cx| theme_settings::adjust_ui_font_size(cx, |_| px(20.)));
        setup.cx.run_until_parked();
        for (keys, selector) in [("shift-n", "INBOX_MESSAGE-0"), ("n", "INBOX_MESSAGE-1")] {
            setup.cx.simulate_keystrokes(keys);
            draw(&mut setup);
            draw(&mut setup);
            let card = drawn(&mut setup, selector);
            let reader = setup
                .view
                .read_with(&setup.cx, |view, _| view.reader_scroll.bounds());
            assert!(
                card.top() >= reader.top() && card.top() < reader.bottom(),
                "{selector}: {card:?} starts inside {reader:?}"
            );
        }
    }

    #[gpui::test]
    async fn a_long_sender_leaves_the_card_its_position(cx: &mut TestAppContext) {
        let long_sender = format!("{}<help@example.com>", "Automated Notifications ".repeat(6));
        let note = TICKET.replace("## helpdesk@example.com", &format!("## {long_sender}"));
        let mut setup = setup(cx, Options::default()).await;
        setup
            .fs
            .insert_tree(
                setup._vault_dir.path().join("inbox"),
                json!({ "2026-10-09-1930-ticket-42.md": note }),
            )
            .await;
        setup.cx.run_until_parked();
        select(&mut setup, "Ticket 42");
        setup.cx.simulate_resize(gpui::size(px(1000.), px(900.)));
        setup.cx.run_until_parked();
        let card = drawn(&mut setup, "INBOX_MESSAGE-0");
        let position = drawn(&mut setup, "INBOX_POSITION-0");
        assert!(position.right() <= card.right(), "{position:?} in {card:?}");
    }

    #[gpui::test]
    async fn z_shows_what_the_email_folded_and_the_next_note_starts_folded(
        cx: &mut TestAppContext,
    ) {
        let mut setup = setup_with_ticket(cx).await;
        // The second message's words, then the repeated comment and its
        // fields; its author line is the card's header.
        let folded_comment = (1, 1);
        let has_comment = |setup: &Setup| {
            setup.view.read_with(&setup.cx, |view, _| {
                view.reader_note.markdown.contains_key(&folded_comment)
            })
        };
        drawn(&mut setup, "INBOX_FOLD-1");
        assert!(!has_comment(&setup));

        setup.cx.simulate_keystrokes("z");
        draw(&mut setup);
        assert!(has_comment(&setup));

        setup.cx.simulate_keystrokes("z");
        draw(&mut setup);
        setup
            .view
            .read_with(&setup.cx, |view, _| assert!(!view.reader_note.folds_open));

        setup.cx.simulate_keystrokes("z");
        select(&mut setup, "Re: Q4 planning doc");
        draw(&mut setup);
        setup
            .view
            .read_with(&setup.cx, |view, _| assert!(!view.reader_note.folds_open));
    }

    #[gpui::test]
    async fn keys_stay_on_screen_while_the_reader_has_focus(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        setup.cx.simulate_keystrokes("tab z");
        // A key lookup that followed focus would only fail from the frame
        // after focus moved, so draw twice.
        draw(&mut setup);
        draw(&mut setup);
        let view = setup.view.clone();
        assert!(
            setup
                .cx
                .update(|window, cx| view.read(cx).reader_focus.is_focused(window)),
            "`z` leaves focus in the reader"
        );
        for key in ["KEY_BINDING-t", "KEY_BINDING-s", "KEY_BINDING-e"] {
            assert!(setup.cx.debug_bounds(key).is_some(), "{key}");
        }
        for hint in ["INBOX_HINT-scroll", "INBOX_HINT-list"] {
            assert!(setup.cx.debug_bounds(hint).is_some(), "{hint}");
        }
        assert_eq!(setup.cx.debug_bounds("INBOX_HINT-move"), None);
        assert_eq!(setup.cx.debug_bounds("KEY_BINDING-p"), None);

        setup.cx.simulate_keystrokes("tab");
        draw(&mut setup);
        draw(&mut setup);
        assert!(setup.cx.debug_bounds("KEY_BINDING-t").is_some());
        assert!(setup.cx.debug_bounds("INBOX_HINT-move").is_some());
    }

    #[gpui::test]
    async fn focus_moving_to_the_reader_moves_nothing(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        let before = drawn(&mut setup, "INBOX_MESSAGE-1");
        assert_eq!(setup.cx.debug_bounds("INBOX_READER_FOCUS"), None);
        setup.cx.simulate_keystrokes("tab");
        let after = drawn(&mut setup, "INBOX_MESSAGE-1");
        assert_eq!(before, after);
        assert!(setup.cx.debug_bounds("INBOX_READER_FOCUS").is_some());
    }

    #[gpui::test]
    async fn the_footer_gives_hints_up_whole_and_keeps_the_keys_sheet(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                ..Options::default()
            },
        )
        .await;
        for (width, fraction) in [(1000., 0.2), (1000., 0.26), (1400., 0.26), (2400., 0.32)] {
            setup.cx.simulate_resize(gpui::size(px(width), px(900.)));
            setup
                .view
                .update(&mut setup.cx, |view, _| view.list_fraction = fraction);
            setup.cx.run_until_parked();
            let keys = drawn(&mut setup, "INBOX_HINT-keys");
            let footer_bottom = keys.bottom();
            assert!(
                keys.right() <= px(width) * fraction,
                "{width}: {keys:?} inside the list"
            );
            for hint in [
                "INBOX_HINT-move",
                "INBOX_HINT-mark",
                "INBOX_HINT-filter",
                "INBOX_STATS",
            ] {
                let Some(bounds) = setup.cx.debug_bounds(hint) else {
                    continue;
                };
                let whole = bounds.right() <= keys.left() && bounds.top() == keys.top();
                let gone = bounds.top() >= footer_bottom;
                assert!(whole || gone, "{width}: {hint} {bounds:?} beside {keys:?}");
            }
        }
    }

    #[gpui::test]
    async fn age_keeps_its_place_in_a_narrow_list(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        setup.cx.simulate_resize(gpui::size(px(1000.), px(900.)));
        setup
            .view
            .update(&mut setup.cx, |view, _| view.list_fraction = 0.2);
        setup.cx.run_until_parked();
        let row = drawn(&mut setup, "INBOX_ROW-0");
        let age = drawn(&mut setup, "INBOX_AGE-0");
        assert!(age.right() <= row.right(), "{age:?} inside {row:?}");
        assert!(age.left() > row.center().x, "{age:?} pinned right");
    }

    #[gpui::test]
    async fn below_the_line_the_likeliest_place_leans_without_a_key(cx: &mut TestAppContext) {
        let mut setup = setup(
            cx,
            Options {
                grant: true,
                cached: vec![(Q4, "soon", 0.45)],
                ..Options::default()
            },
        )
        .await;
        select(&mut setup, "Re: Q4 planning doc");
        setup.view.read_with(&setup.cx, |view, cx| {
            let state = view.bar_state(view.selected_entry().unwrap(), cx);
            assert_eq!(state.suggested, None);
            assert_eq!(state.leaning, Some(Destination::Soon));
        });
        assert_eq!(space_button(&mut setup), None);
    }

    #[test]
    fn a_sender_keeps_their_hue_across_themes_and_none_reads_as_an_error() {
        for participant in 0..SENDER_HUES.len() as u32 {
            let (light, dark) = (
                sender_color(participant, true),
                sender_color(participant, false),
            );
            assert_eq!(light.h, dark.h, "{participant}");
            assert!(light.l < dark.l, "{participant}");
            let degrees = light.h * 360.;
            assert!((30.0..=340.0).contains(&degrees), "{degrees} reads as red");
        }
        assert_eq!(
            sender_color(0, true),
            sender_color(SENDER_HUES.len() as u32, true)
        );
    }

    #[test]
    fn a_fold_row_reads_as_one_action_adding_up_by_kind() {
        let pieces = [
            FoldPiece::Text(FoldKind::Repeated, 9),
            FoldPiece::Fields(2),
            FoldPiece::Text(FoldKind::Repeated, 3),
            FoldPiece::Fields(12),
        ];
        assert_eq!(
            fold_summary(&pieces, false),
            "Show all folds · 12 earlier lines and 14 fields here"
        );
        assert_eq!(
            fold_summary(&pieces, true),
            "Hide all folds · 12 earlier lines and 14 fields here"
        );
        assert_eq!(
            fold_summary(&[FoldPiece::Text(FoldKind::Signature, 4)], false),
            "Show all folds · signature here"
        );
        assert_eq!(
            fold_summary(
                &[
                    FoldPiece::Text(FoldKind::Quoted, 1),
                    FoldPiece::Text(FoldKind::Signature, 4),
                    FoldPiece::Fields(1),
                ],
                false
            ),
            "Show all folds · 1 quoted line, signature and 1 field here"
        );
    }

    #[test]
    fn the_header_names_a_sender_once() {
        assert_eq!(
            sender_parts("helpdesk@example.com".to_string(), None),
            ("helpdesk".to_string(), Some("@example.com".to_string()))
        );
        assert_eq!(
            sender_parts("Marta".to_string(), Some("m@example.com".to_string())),
            ("Marta".to_string(), Some("m@example.com".to_string()))
        );
        assert_eq!(
            relayed_via("help@example.com", Some("help@example.com")),
            None
        );
        assert_eq!(
            relayed_via("help@example.com", Some("Marta <m@example.com>")).as_deref(),
            Some("via help@example.com")
        );
    }

    #[test]
    fn the_marks_say_whose_percentages_these_are() {
        assert_eq!(marks_note(2, false), "2 marked · decisions apply to all");
        assert_eq!(
            marks_note(2, true),
            "2 marked · decisions apply to all · percentages are this row's"
        );
    }

    #[test]
    fn every_list_and_its_old_value_read_one_way() {
        assert_eq!(field_list("a,b, c,d,e", false), ("a, b, c".to_string(), 2));
        assert_eq!(
            field_list("a,b, c,d,e", true),
            ("a, b, c, d, e".to_string(), 0)
        );
        assert_eq!(field_list("sosh,lmiller", false).0, "sosh, lmiller");
        assert_eq!(field_list("1,000", false).0, "1,000");
        let prose = "first, second thing, third, fourth, fifth";
        assert_eq!(field_list(prose, false), (prose.to_string(), 0));
        assert_eq!(was_text("sosh,lmiller", false), "sosh, lmiller");
        assert_eq!(was_text("a,b,c,d,e", false), "a, b, c +2 more");
        assert_eq!(was_text("a,b,c,d,e", true), "a, b, c, d, e");
        assert_eq!(was_text("", false), "—");
        assert!(previous_helps("EDIT"));
        assert!(previous_helps("a,b,c,d,e"));
        assert!(!previous_helps(&"long".repeat(20)));
    }

    #[test]
    fn a_tie_leans_nowhere() {
        assert_eq!(most_likely(&[0.1, 0.4, 0.2, 0.1]), Some(Destination::Soon));
        assert_eq!(most_likely(&[0.3, 0.3, 0.2, 0.1]), None);
        assert_eq!(most_likely(&[0.0; 4]), None);
    }

    #[test]
    fn a_row_shortens_a_bare_address() {
        assert_eq!(short_sender("helpdesk@example.com"), "helpdesk");
        assert_eq!(short_sender("Marta <marta@example.com>"), "Marta");
    }

    #[gpui::test]
    async fn the_filter_box_keeps_the_message_keys(cx: &mut TestAppContext) {
        let mut setup = setup_with_ticket(cx).await;
        setup.cx.simulate_keystrokes("/ z n");
        setup.cx.run_until_parked();
        setup.view.read_with(&setup.cx, |view, cx| {
            assert_eq!(view.filter_editor.read(cx).text(cx), "zn");
            assert!(!view.reader_note.folds_open);
        });
    }

    #[test]
    fn a_leading_heading_that_repeats_the_title_goes() {
        assert_eq!(
            without_leading_title("# Call Ana\n\nAbout the keys.\n", "Call Ana"),
            "About the keys.\n"
        );
        assert_eq!(
            without_leading_title("\n## call ana  \nbody", "Call Ana"),
            "body"
        );
        assert_eq!(without_leading_title("# Call Ana", "Call Ana"), "");
        // A different heading, a plain first line and a hashtag stay.
        assert_eq!(
            without_leading_title("# Agenda\nCall Ana", "Call Ana"),
            "# Agenda\nCall Ana"
        );
        assert_eq!(
            without_leading_title("Call Ana\nbody", "Call Ana"),
            "Call Ana\nbody"
        );
        assert_eq!(
            without_leading_title("#Call Ana\nbody", "Call Ana"),
            "#Call Ana\nbody"
        );
    }

    #[test]
    fn line_removal_helpers() {
        let text = "## Soon\n\n- [ ] Call Ana [[a]]\n- [x] Call Ana [[a]] ✅\n\n## Someday\n";
        let without = without_line(text, "- [ ] Call Ana [[a]]").unwrap();
        assert_eq!(
            without,
            "## Soon\n\n- [x] Call Ana [[a]] ✅\n\n## Someday\n"
        );
        // The two lines share a prefix, so the edit may start inside the
        // first; what matters is that applying it yields the new text.
        let (range, replacement) = single_edit(text, &without).unwrap();
        let mut applied = text.to_string();
        applied.replace_range(range, &replacement);
        assert_eq!(applied, without);
        assert_eq!(without_line(text, "- [ ] Nope"), None);
        assert_eq!(single_edit(text, text), None);
        // An append is one edit too, and multi-byte text keeps its boundaries.
        let (range, replacement) = single_edit("- é\n", "- é\n- ü\n").unwrap();
        assert_eq!((range, replacement.as_str()), (5..5, "- ü\n"));
        let (range, replacement) = single_edit("aéb", "aüb").unwrap();
        assert_eq!((range, replacement.as_str()), (1..3, "ü"));

        // Only the exact line goes, and the last one: a scheduled block that
        // normalises to the same words is the person's and stays.
        let note = "- [ ] 09:00 - 09:30 Standup\n- [ ] Standup\n\n## Personal\n";
        assert_eq!(
            without_line(note, "- [ ] Standup").unwrap(),
            "- [ ] 09:00 - 09:30 Standup\n\n## Personal\n"
        );
        let twice = "- [ ] A\n- [x] B\n- [ ] A\n";
        assert_eq!(
            without_line(twice, "- [ ] A").unwrap(),
            "- [ ] A\n- [x] B\n"
        );
        // A task that was appended into an empty section takes its blank
        // line with it, so the section reads as it did before.
        assert_eq!(
            without_line(
                "## Soon\n\n- [ ] Call Ana\n\n## Someday\n",
                "- [ ] Call Ana"
            )
            .unwrap(),
            "## Soon\n\n## Someday\n"
        );
        assert_eq!(
            without_line(
                "- [ ] Standup\n- [ ] Call Ana\n\n## Personal\n",
                "- [ ] Call Ana"
            )
            .unwrap(),
            "- [ ] Standup\n\n## Personal\n"
        );

        let log = "- d · A → Archived\n- d · B → Archived\n- d · A → Archived\n";
        assert_eq!(
            without_line(log, "- d · A → Archived").unwrap(),
            "- d · A → Archived\n- d · B → Archived\n"
        );
        assert_eq!(without_line(log, "- d · C → Archived"), None);
        assert_eq!(sender_name("Marta <marta@example.com>"), "Marta");
        assert_eq!(sender_name("<x@y.z>"), "x@y.z");
        assert_eq!(sender_name("\"Ana\" <a@b.c>"), "Ana");
        assert_eq!(
            sender_address("Marta <marta@example.com>").as_deref(),
            Some("marta@example.com")
        );
        assert_eq!(sender_address("<x@y.z>"), None);
        assert_eq!(sender_address("Marta"), None);
    }

    #[test]
    fn age_reads_in_minutes_hours_then_days() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-11T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let age = |captured: &str| age_label(Some(captured), now);
        assert_eq!(age("2026-10-11T12:00:30Z").as_deref(), Some("now"));
        assert_eq!(age("2026-10-11T13:00:00Z").as_deref(), Some("now"));
        assert_eq!(age("2026-10-11T11:35:00Z").as_deref(), Some("25m"));
        assert_eq!(age("2026-10-11T10:00:00Z").as_deref(), Some("2h"));
        assert_eq!(age("2026-10-10T08:00:00Z").as_deref(), Some("1d"));
        assert_eq!(age("2026-10-08T11:00:00Z").as_deref(), Some("3d"));
        assert_eq!(age_label(None, now), None);
        assert_eq!(age_label(Some("not a date"), now), None);
        assert_eq!(source_label("google-tasks"), "Google Tasks");
        assert_eq!(source_label("readwise"), "readwise");
    }
}
