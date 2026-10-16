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
    EventEmitter, FocusHandle, Focusable, FontWeight, ImageSource, KeyContext, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Point, Resource, ScrollHandle, SharedString, Subscription,
    Task, TaskExt as _, UniformListScrollHandle, WeakEntity, Window, actions, anchored, deferred,
    div, point, prelude::*, px, relative, transparent_black,
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use menu::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::Project;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use thock_sync_core::{Operation, Write, apply, archive_path, inbox_task_line, triage_log_line};
use ui::{
    ActiveTheme as _, Button, ButtonCommon as _, ButtonLike, ButtonSize, ButtonStyle, Chip,
    Clickable as _, Color, ContextMenu, FixedWidth as _, HeadlineSize, Icon, IconButton, IconName,
    IconSize, KeyBinding, Label, LabelCommon as _, LabelSize, Toggleable as _, Tooltip, h_flex,
    v_flex,
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
use crate::inbox::{TRIAGE_LOG_PATH, sanitize_title};
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
const ROW_HEIGHT: Pixels = px(60.);
const FOOTER_HEIGHT: Pixels = px(36.);
const BAR_BUTTON_HEIGHT: Pixels = px(56.);
const READER_MAX_WIDTH: Pixels = px(680.);

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
    /// The rendered note, rebuilt when the selected bytes change.
    markdown: Option<(String, Entity<Markdown>)>,
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
            markdown: None,
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
            .action("Open", OpenInboxItem.boxed_clone())
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
        BarState {
            suggested,
            probabilities: Some(Destination::ALL.map(|destination| {
                suggestion
                    .probabilities
                    .get(destination.key())
                    .copied()
                    .unwrap_or(0.0)
            })),
        }
    }

    /// The list footer's quiet stats (§4.2).
    fn footer_stats(&self, cx: &App) -> FooterStats {
        if !self.marked.is_empty() {
            return FooterStats::Marked(self.marked.len());
        }
        let availability = self.availability(cx);
        if availability == Some(SuggestionAvailability::NoGrant) && !self.plus_banner_dismissed {
            return FooterStats::PlusOffer;
        }
        let mut parts = Vec::new();
        match availability {
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

    fn render_list_header(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let filtering =
            self.filter_editor.focus_handle(cx).is_focused(window) || !self.filter.is_empty();
        let border = cx.theme().colors().border;
        let counts = self.section_counts().into_iter().map(|count| {
            let selector = if count.section == 0 {
                "INBOX_SECTION-new"
            } else {
                "INBOX_SECTION-carried"
            };
            let section = count.section;
            div().debug_selector(|| selector.to_string()).child(
                ButtonLike::new(("inbox-section", count.section))
                    .size(ButtonSize::Medium)
                    .style(if count.active {
                        ButtonStyle::Filled
                    } else {
                        ButtonStyle::Subtle
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Label::new(count.label).color(if count.active {
                                Color::Default
                            } else {
                                Color::Muted
                            }))
                            .child(Label::new(count.count.to_string()).color(Color::Muted)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.scroll_to_section(section, cx);
                    })),
            )
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
            .child(if filtering {
                div()
                    .w(px(180.))
                    .min_w(px(80.))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .child(self.filter_editor.clone())
                    .into_any_element()
            } else {
                IconButton::new("inbox-filter", IconName::MagnifyingGlass)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::for_action_title("Filter", &FilterInbox))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.filter(&FilterInbox, window, cx);
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_reader_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let entry = self.selected_entry();
        let item = entry.map(|entry| &entry.item);
        let source = item.and_then(|item| item.source.as_deref());
        let mut meta = h_flex().min_w_0().overflow_hidden().gap_2p5();
        if let Some(item) = item {
            meta = meta.child(
                Icon::new(source_icon(source))
                    .size(IconSize::Small)
                    .color(Color::Muted),
            );
            let name = item
                .from
                .as_deref()
                .map(sender_name)
                .or_else(|| source.map(source_label));
            let named = name.is_some();
            if let Some(name) = name {
                meta = meta.child(Label::new(name).size(LabelSize::Small).single_line());
            }
            if let Some(address) = item.from.as_deref().and_then(sender_address) {
                meta = meta.child(
                    Label::new(address)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
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
                    Label::new(format!("{separator}{}", when.join(" · ")))
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .single_line(),
                );
            }
        }
        // Only web links open from here: the note is untrusted input, and
        // any other scheme could reach an app on this machine.
        let link = item
            .and_then(|item| item.link.clone().or_else(|| item.url.clone()))
            .filter(|link| link.starts_with("https://") || link.starts_with("http://"));
        let link_text = if source == Some("gmail") {
            "Open in Gmail ↗"
        } else {
            "Open link ↗"
        };
        h_flex()
            .size_full()
            .pl_10()
            .pr_4()
            .gap_2()
            .child(meta)
            .child(div().flex_1())
            .when_some(link, |this, link| {
                this.child(
                    Button::new("inbox-open-link", link_text)
                        .label_size(LabelSize::Small)
                        .color(Color::Accent)
                        .tooltip(Tooltip::text(link.clone()))
                        .on_click(move |_, _, cx| cx.open_url(&link)),
                )
            })
            .child(
                IconButton::new("inbox-keys", IconName::Keyboard)
                    .icon_size(IconSize::Small)
                    .toggle_state(self.keys_sheet)
                    .tooltip(Tooltip::for_action_title("Keys", &ShowInboxKeys))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_keys(&ShowInboxKeys, window, cx);
                    })),
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

    /// The row's suggestion chip (§7.4): the destination only, filled or
    /// outlined by tier, and nothing below the line. A failed request shows
    /// a warning glyph with the reason (§7.3).
    fn render_chip(&self, entry: &InboxEntry, cx: &App) -> Option<AnyElement> {
        let service = self.service.as_ref()?.read(cx);
        if service.suggestion_availability(cx) != SuggestionAvailability::On {
            return None;
        }
        let accent = cx.theme().colors().text_accent;
        match service.suggestion_for(&entry.hash) {
            Some(suggestion) => match (tier(suggestion), suggestion.destination) {
                (Tier::Filled, Some(destination)) => Some(
                    Chip::new(destination.label())
                        .bg_color(accent.opacity(0.15))
                        .border_color(accent)
                        .label_color(Color::Accent)
                        .into_any_element(),
                ),
                (Tier::Outlined, Some(destination)) => Some(
                    Chip::new(destination.label())
                        .border_color(accent)
                        .into_any_element(),
                ),
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
        let marking = !self.marked.is_empty();
        let list_focused = self.focus_handle.is_focused(window);
        let stem = item.stem.clone();
        let colors = cx.theme().colors();
        let (ring, selected_background, hover_background) = (
            colors.text_accent.opacity(0.4),
            colors.ghost_element_selected,
            colors.ghost_element_hover,
        );
        let mut meta: Vec<String> = Vec::new();
        if let Some(source) = &item.source {
            meta.push(source_label(source));
        }
        if let Some(from) = &item.from {
            meta.push(sender_name(from));
        }
        if let Some(age) = age_label(item.captured.as_deref(), Utc::now()) {
            meta.push(age);
        }
        let chip = self.render_chip(entry, cx);
        h_flex()
            .id(SharedString::from(format!("inbox-row-{stem}")))
            .debug_selector(|| format!("INBOX_ROW-{position}"))
            .relative()
            .h(ROW_HEIGHT)
            .w_full()
            .pl_3()
            .pr_4()
            .gap_3()
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
            // While any row is marked every row gets a check column, so the
            // check has room to breathe and the titles stay aligned.
            .when(marking, |this| {
                this.child(
                    div()
                        .debug_selector(move || format!("INBOX_CHECK-{position}"))
                        .flex_none()
                        .w(px(14.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(marked, |this| {
                            this.child(
                                Icon::new(IconName::Check)
                                    .size(IconSize::Small)
                                    .color(Color::Accent),
                            )
                        }),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(Label::new(item.title.clone()).truncate())
                    .child(
                        h_flex()
                            .min_w_0()
                            .overflow_hidden()
                            .gap_1p5()
                            .child(
                                Label::new(meta.join(" · "))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                            .when(item.has_image, |this| {
                                this.child(
                                    Icon::new(IconName::Image)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                            }),
                    ),
            )
            .children(chip)
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
            .px_3()
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
            .p_2()
            .track_scroll(&self.scroll)
            .into_any_element()
    }

    /// The keycap the footer shows for moving one row: `k`/`j` under vim,
    /// else the arrows. The highest-precedence binding would be the shared
    /// menu's `ctrl-p`/`ctrl-n`, and vim binds the row keys to its own
    /// actions, so both are searched by name.
    fn move_keycap(
        &self,
        action_names: [&str; 2],
        preferred: [&str; 2],
        fallback: &dyn Action,
        window: &Window,
        cx: &App,
    ) -> KeyBinding {
        let bindings: Vec<(bool, gpui::KeyBinding)> = action_names
            .into_iter()
            .filter_map(|name| cx.build_action(name, None).ok())
            .flat_map(|action| {
                let vim = action.name().starts_with("vim::");
                window
                    .bindings_for_action_in(action.as_ref(), &self.focus_handle)
                    .into_iter()
                    .map(move |binding| (vim, binding))
            })
            .collect();
        preferred
            .into_iter()
            .find_map(|key| {
                bindings.iter().find(|(_, binding)| {
                    matches!(binding.keystrokes(), [keystroke]
                        if keystroke.key() == key && !keystroke.modifiers().modified())
                })
            })
            .map(|(vim, binding)| KeyBinding::from_keystrokes(binding.keystrokes().into(), *vim))
            .unwrap_or_else(|| KeyBinding::for_action_in(fallback, &self.focus_handle, cx))
    }

    fn render_list_footer(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let hint = |keycaps: Vec<KeyBinding>, label: &'static str| {
            h_flex()
                .flex_none()
                .gap_1()
                .children(keycaps)
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
        };
        let keycap =
            |action: &dyn Action| vec![KeyBinding::for_action_in(action, &self.focus_handle, cx)];
        let move_keys = vec![
            self.move_keycap(
                ["vim::MenuSelectPrevious", "menu::SelectPrevious"],
                ["k", "up"],
                &SelectPrevious,
                window,
                cx,
            ),
            self.move_keycap(
                ["vim::MenuSelectNext", "menu::SelectNext"],
                ["j", "down"],
                &SelectNext,
                window,
                cx,
            ),
        ];
        let stats = match self.footer_stats(cx) {
            FooterStats::Marked(count) => Some(
                Label::new(format!("{count} marked"))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element(),
            ),
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
        h_flex()
            .flex_none()
            .h(FOOTER_HEIGHT)
            .w_full()
            .px_5()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .child(
                h_flex()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_3p5()
                    .child(hint(move_keys, "move"))
                    .child(hint(keycap(&MarkInboxItem), "mark"))
                    .child(hint(keycap(&FilterInbox), "filter"))
                    .child(hint(keycap(&ShowInboxKeys), "keys")),
            )
            .child(div().flex_1())
            .children(stats.map(|stats| div().flex_none().child(stats)))
            .into_any_element()
    }

    fn markdown_style(&self, window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        style.base_text_style.font_size = px(15.).into();
        style.base_text_style.line_height = relative(1.65);
        style
    }

    fn markdown_for(&mut self, entry: &InboxEntry, cx: &mut Context<Self>) -> Entity<Markdown> {
        if let Some((hash, markdown)) = &self.markdown
            && *hash == entry.hash
        {
            return markdown.clone();
        }
        let body = without_leading_title(&entry.item.body, &entry.item.title);
        let source: SharedString = body.to_string().into();
        let markdown = cx.new(|cx| Markdown::new(source, None, None, cx));
        self.markdown = Some((entry.hash.clone(), markdown.clone()));
        self.reader_scroll.set_offset(point(px(0.), px(0.)));
        markdown
    }

    fn render_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.keys_sheet {
            return div()
                .relative()
                .size_full()
                .child(self.render_keys_sheet(cx))
                .into_any_element();
        }
        let Some(entry) = self.selected_entry().cloned() else {
            return v_flex()
                .relative()
                .size_full()
                .px_10()
                .pt_7()
                .child(
                    Label::new("Select an item to read it here.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        };
        let markdown = self.markdown_for(&entry, cx);
        let style = self.markdown_style(window, cx);
        let resolver = self.image_resolver(cx);
        let focused = self.reader_focus.is_focused(window);
        let colors = cx.theme().colors();
        let (rule, focus_edge) = (colors.border_variant, colors.border_focused);
        v_flex()
            .relative()
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .w_full()
                    .px_10()
                    .pt_7()
                    .pb_5()
                    .gap(px(18.))
                    .border_b_1()
                    .border_color(rule)
                    .child(
                        Label::new(entry.item.title.clone())
                            .size(LabelSize::Custom(HeadlineSize::XLarge.rems()))
                            .weight(FontWeight::SEMIBOLD),
                    )
                    .child(self.render_destination_bar(&entry, cx)),
            )
            .child(
                v_flex()
                    .id("inbox-reader")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .track_focus(&self.reader_focus)
                    .overflow_y_scroll()
                    .track_scroll(&self.reader_scroll)
                    .px_10()
                    .py_6()
                    .when(focused, |this| this.border_l_2().border_color(focus_edge))
                    .child(
                        div().w_full().max_w(READER_MAX_WIDTH).child(
                            MarkdownElement::new(markdown, style)
                                .image_resolver(move |src| resolver(src)),
                        ),
                    ),
            )
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
        action: Box<dyn Action>,
        content: impl IntoElement,
        cx: &App,
    ) -> gpui::Div {
        let colors = cx.theme().colors();
        let accent = colors.text_accent;
        let (background, border, hover) = match look {
            BarLook::Filled => (accent.opacity(0.18), accent, accent.opacity(0.26)),
            BarLook::Outlined => (colors.element_background, accent, colors.element_hover),
            BarLook::Plain => (
                colors.element_background,
                colors.border_variant,
                colors.element_hover,
            ),
            BarLook::Quiet => (
                transparent_black(),
                colors.border_variant,
                colors.ghost_element_hover,
            ),
        };
        let fills = look != BarLook::Quiet;
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
            // The four destinations share what is left after Open and Undo
            // equally and shrink with the reader, so the bar is always one
            // row.
            .when(fills, |this| this.flex_1().min_w_0())
            .when(!fills, |this| this.flex_none())
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
        cx: &App,
    ) -> gpui::Div {
        let suggested = matches!(look, BarLook::Filled | BarLook::Outlined);
        let colors = cx.theme().colors();
        let (track, fill) = (colors.text_muted.opacity(0.18), colors.text_accent);
        let name = destination.key();
        // Two lines, so a narrow reader keeps every button on one row: the
        // keys and the label, then the probability bar and its percentage.
        // The keys and the percentage keep their size; only the label gives
        // way.
        let content = v_flex()
            .w_full()
            .h_full()
            .justify_center()
            .gap_2()
            .px(px(10.))
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .when(suggested, |this| {
                        this.child(div().flex_none().child(KeyBinding::for_action_in(
                            &AcceptInboxSuggestion,
                            &self.focus_handle,
                            cx,
                        )))
                        .child(div().flex_none().child(keycap_or()))
                    })
                    .child(div().flex_none().child(KeyBinding::for_action_in(
                        destination_action(destination).as_ref(),
                        &self.focus_handle,
                        cx,
                    )))
                    .child(
                        div().flex_1().min_w_0().child(
                            Label::new(destination.label())
                                .color(if suggested {
                                    Color::Accent
                                } else {
                                    Color::Default
                                })
                                .truncate(),
                        ),
                    ),
            )
            .when_some(probability, |this, probability| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .debug_selector(move || format!("INBOX_PROBABILITY-{name}"))
                                .flex_1()
                                .min_w_0()
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
                        .child(
                            div()
                                .debug_selector(move || format!("INBOX_PERCENT-{name}"))
                                .flex_none()
                                .child(
                                    Label::new(format!(
                                        "{}%",
                                        (probability * 100.0).round() as u32
                                    ))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                                ),
                        ),
                )
            });
        self.bar_button(
            destination.key(),
            look,
            destination_action(destination),
            content,
            cx,
        )
    }

    /// Open or Undo, after the divider.
    fn quiet_button(
        &self,
        name: &'static str,
        action: &dyn Action,
        label: &'static str,
        cx: &App,
    ) -> gpui::Div {
        let content = h_flex()
            .w_full()
            .h_full()
            .justify_center()
            .gap_1p5()
            .px(px(12.))
            .child(KeyBinding::for_action_in(action, &self.focus_handle, cx))
            .child(Label::new(label).color(Color::Muted));
        self.bar_button(name, BarLook::Quiet, action.boxed_clone(), content, cx)
    }

    /// The destination bar (§4.2): one button per destination in a fixed
    /// order, then Open and Undo, always on one row.
    fn render_destination_bar(&self, entry: &InboxEntry, cx: &App) -> AnyElement {
        let state = self.bar_state(entry, cx);
        let buttons = Destination::ALL
            .into_iter()
            .enumerate()
            .map(|(position, destination)| {
                let look = match state.suggested {
                    Some((suggested, Tier::Filled)) if suggested == destination => BarLook::Filled,
                    Some((suggested, _)) if suggested == destination => BarLook::Outlined,
                    _ => BarLook::Plain,
                };
                let probability = state
                    .probabilities
                    .and_then(|probabilities| probabilities.get(position).copied());
                self.destination_button(destination, look, probability, cx)
            });
        let open = self.quiet_button("open", &OpenInboxItem, "Open", cx);
        let undo = self.quiet_button("undo", &UndoInboxMove, "Undo", cx);
        let rule = cx.theme().colors().border_variant;
        let divider = |height: Pixels| div().flex_none().w(px(1.)).h(height).mx_1().bg(rule);
        h_flex()
            .w_full()
            .gap_2()
            .children(buttons)
            .child(divider(BAR_BUTTON_HEIGHT - px(12.)))
            .child(open)
            .child(undo)
            .into_any_element()
    }

    fn render_keys_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<(&dyn Action, &str)> = vec![
            (&AcceptInboxSuggestion, "Take the suggestion"),
            (
                &SendInboxItemToToday,
                "Task under today's Day planner heading",
            ),
            (&SendInboxItemToSoon, "Task under Soon"),
            (&SendInboxItemToSomeday, "Task under Someday"),
            (&ArchiveInboxItem, "Out of the inbox, nothing else"),
            (&OpenInboxItem, "The note in the editor"),
            (&UndoInboxMove, "Reverse the last decision"),
            (&MarkInboxItem, "Mark the row for a batch decision"),
            (&SuggestInboxDestinations, "Ask for a suggestion again"),
            (&FilterInbox, "Filter by title, sender, source"),
            (&FocusInboxReader, "Move between the list and the note"),
            (&ShowInboxItemMenu, "The row menu"),
            (&TriageInbox, "Hand what is left to the Triage Inbox ritual"),
            (&ShowInboxKeys, "This sheet"),
        ];
        v_flex()
            .size_full()
            .px_10()
            .pt_7()
            .gap_1()
            .child(
                div()
                    .pb_2()
                    .child(Label::new("Keys").weight(FontWeight::SEMIBOLD)),
            )
            .children(rows.into_iter().map(|(action, what)| {
                h_flex()
                    .gap_2()
                    .child(div().w(px(120.)).child(KeyBinding::for_action_in(
                        action,
                        &self.focus_handle,
                        cx,
                    )))
                    .child(Label::new(what).size(LabelSize::Small).color(Color::Muted))
            }))
            .child(
                div().pt_2().child(
                    Label::new("Any key closes this.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
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
    /// Each destination's probability in `Destination::ALL` order, whenever
    /// a suggestion exists.
    probabilities: Option<[f64; 4]>,
}

/// The right side of the list footer.
#[derive(Debug, Clone, PartialEq)]
enum FooterStats {
    Marked(usize),
    /// *Suggestions come with Thock Plus*, until dismissed.
    PlusOffer,
    Stats(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarLook {
    /// The suggestion at the filled tier.
    Filled,
    /// The suggestion at the outlined tier.
    Outlined,
    Plain,
    /// Open and Undo.
    Quiet,
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
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add(INBOX_VIEW_KEY);
        key_context.add("menu");
        if self.filter_editor.focus_handle(cx).is_focused(window) {
            key_context.add("filtering");
        }
        if self.reader_focus.is_focused(window) {
            key_context.add("reader");
        }
        let list_header = self.render_list_header(window, cx);
        let reader_header = self.render_reader_header(cx);
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

/// Sits between two keycaps that do the same thing, so `space` `s` does not
/// read as one chord.
fn keycap_or() -> Label {
    Label::new("or").size(LabelSize::Small).color(Color::Muted)
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

/// `2h`, `yesterday`, `3 days`: how long a row has waited since its capture.
fn age_label(captured: Option<&str>, now: chrono::DateTime<Utc>) -> Option<String> {
    let captured = chrono::DateTime::parse_from_rfc3339(captured?).ok()?;
    let waited = now - captured.with_timezone(&Utc);
    Some(
        match (waited.num_minutes(), waited.num_hours(), waited.num_days()) {
            (minutes, _, _) if minutes < 1 => "now".to_string(),
            (minutes, 0, _) => format!("{minutes}m"),
            (_, hours, 0) => format!("{hours}h"),
            (_, _, 1) => "yesterday".to_string(),
            (_, _, days) => format!("{days} days"),
        },
    )
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
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};

    const SHIP_IT: &str = "---\nsource:   google-tasks\ncapture:  3f1c3f1c3f1c\ncaptured: 2026-10-09T18:42:00Z\ntitle:    Ship it — a practical guide\nurl:      https://example.com/ship-it\n---\n\n# Ship it — a practical guide\n\nhttps://example.com/ship-it\n";
    const Q4: &str = "---\nsource:   gmail\ncapture:  a1a1a1a1a1a1\ncaptured: 2026-10-09T19:00:00Z\ntitle:    Re: Q4 planning doc\nfrom:     Marta <marta@example.com>\n---\n\n# Re: Q4 planning doc\n\nCan you look at the numbers before Friday?\n";
    const CALL_ANA: &str = "---\nsource:   thock-ios\ncapture:  4d1f9a02c7b3\ncaptured: 2026-10-10T09:31:00Z\ntitle:    Call Ana\n---\n\n# Call Ana\n";
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

    #[gpui::test]
    async fn marked_rows_archive_together_in_list_order(cx: &mut TestAppContext) {
        let mut setup = setup(cx, Options::default()).await;
        // The first and the last row, so the batch spans both sections and
        // the log follows list order, not marking order.
        draw(&mut setup);
        assert_eq!(setup.cx.debug_bounds("INBOX_CHECK-0"), None);
        setup.cx.simulate_keystrokes("end x home x");
        setup.view.read_with(&setup.cx, |view, cx| {
            assert_eq!(view.marked.len(), 2);
            assert_eq!(view.footer_stats(cx), FooterStats::Marked(2));
        });
        // While marks exist every row has a check column inside its
        // padding, the same for all rows so the titles stay aligned.
        let first_row = drawn(&mut setup, "INBOX_ROW-0");
        let first_check = drawn(&mut setup, "INBOX_CHECK-0");
        assert!(first_check.left() >= first_row.left() + px(8.));
        let unmarked_check = drawn(&mut setup, "INBOX_CHECK-1");
        assert_eq!(unmarked_check.left(), first_check.left());
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

    const BAR_BUTTONS: [&str; 6] = [
        "INBOX_BAR-today",
        "INBOX_BAR-soon",
        "INBOX_BAR-someday",
        "INBOX_BAR-archive",
        "INBOX_BAR-open",
        "INBOX_BAR-undo",
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
            let (_, markdown) = view.markdown.as_ref().unwrap();
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
        let undo = drawn(&mut setup, "INBOX_BAR-undo");
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
        for width in [2400., 1600., 1300.] {
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
            let undo = buttons[5];
            assert!(
                undo.right() <= px(width),
                "{width}: Undo fits in the window, {undo:?}"
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
        assert_eq!(age("2026-10-10T08:00:00Z").as_deref(), Some("yesterday"));
        assert_eq!(age("2026-10-08T11:00:00Z").as_deref(), Some("3 days"));
        assert_eq!(age_label(None, now), None);
        assert_eq!(age_label(Some("not a date"), now), None);
        assert_eq!(source_label("google-tasks"), "Google Tasks");
        assert_eq!(source_label("readwise"), "readwise");
    }
}
