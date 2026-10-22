//! The Backlog view (spec `v6-backlog.md`, laid out by V43 §8): a checklist
//! over the vault's `backlog.md`, shown by the Desk panel as the Soon and
//! Someday lanes stacked on its Backlog tab and the Completed history on its
//! Done tab. Task text is editable inline; checking a task off records it as
//! done in today's daily note and files it under Completed with the date. The
//! file stays the single source of truth: edits go through the open buffer
//! (so an editor tab and the view never fight) and the view re-parses on
//! every buffer change.

use anyhow::Context as _;
use chrono::{Local, NaiveDate};
use db::kvp::KeyValueStore;
use editor::{Editor, EditorEvent, SelectionEffects, scroll::Autoscroll};
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, ClipboardItem, Context, Entity, FocusHandle,
    Focusable, FontWeight, Hsla, KeyContext, ScrollHandle, Subscription, Task, WeakEntity, Window,
    actions, div, px, rems,
};
use language::{Buffer, BufferEvent};
use menu::{Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::Project;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use text::{Bias, Point};
use ui::prelude::*;
use ui::{Checkbox, Icon, IconButton, IconSize, Label, LabelLike, ToggleState, Tooltip};
use util::ResultExt as _;
use util::rel_path::RelPath;
use workspace::{OpenOptions, OpenVisible, Workspace};

use crate::backlog::{
    self, Backlog, BacklogHeadings, BacklogTask, SectionKind, parse_backlog, split_completion,
};
use crate::day_plan::strip_trailing_metadata;
use crate::gmail_service::{self, GmailService};
use crate::inbox_service::{self, InboxService};
use crate::markdown_conceal::resolve_wikilink_target;
use crate::markdown_text::{LinkTarget, parse_inline_spans, render_markdown_row};
use crate::notes::{EnsureNoteOutcome, NoteKind, ensure_note};
use crate::readwise_service::{self, ReadwiseService};
use crate::reminder_index::{IndexSection, IndexedReminder, IndexedState, StampKind};
use crate::reminder_service::{self, ReminderEvent, ReminderService};
use crate::reminders::{ChipState, ReminderClock, ReminderDay, reminder_chip};
use crate::sync_status;
use crate::vault::{Vault, VaultStatus};
use crate::vault_sync::{self, VaultSyncService};

const BACKLOG_PANEL_KEY: &str = "ThockBacklogPanel";
const REPARSE_DEBOUNCE: Duration = Duration::from_millis(150);
/// How long a copied row stays lit — vim's `highlight_on_yank_duration`
/// default, so a yank in the panel feels like a yank in the editor.
const COPY_FLASH_DURATION: Duration = Duration::from_millis(200);

actions!(
    thock,
    [
        /// Opens the Desk panel on its Backlog tab, or returns to the note.
        ToggleBacklogFocus,
        /// Jumps to the next backlog lane's title (Soon → Someday).
        SelectNextBacklogColumn,
        /// Jumps to the previous backlog lane's title (Someday → Soon).
        SelectPreviousBacklogColumn,
        /// Edits the selected backlog task's text in place.
        EditBacklogTask,
        /// Adds a new task to the selected backlog column.
        AddBacklogTask,
        /// Marks the selected backlog task done, recording it in today's note.
        CompleteBacklogTask,
        /// Moves the selected task from Soon to Someday.
        MoveBacklogTaskRight,
        /// Moves the selected task from Someday to Soon.
        MoveBacklogTaskLeft,
        /// Moves the selected backlog task onto today's note's task list.
        MoveBacklogTaskToToday,
        /// Copies the selected task to the clipboard as Markdown.
        CopyBacklogTask,
        /// Opens backlog.md at the selected task's line.
        RevealBacklogTask,
        /// Closes the backlog category or lane the cursor is in.
        CollapseBacklogCategory,
        /// Opens the backlog category or lane the cursor is in.
        ExpandBacklogCategory
    ]
);

/// The view state that outlives a session: which categories and lanes the
/// user closed (V17 §5.4, V43 §8.3). Category membership itself lives in
/// `backlog.md`.
#[derive(Default, Serialize, Deserialize)]
struct SerializedBacklogPanel {
    /// `(section id, category name)` pairs. Keyed on `SectionKind::id()` so
    /// the state survives a language change; pre-V19 builds wrote the English
    /// headings, which `from_id` still accepts (V19 §3 #3).
    collapsed: Vec<(String, String)>,
    /// Section ids of the folded lanes; absent in state written before V43,
    /// which means the default of Someday folded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folded_lanes: Option<Vec<String>>,
}

/// The remembered fold state, as loaded from the store.
#[derive(Default)]
pub struct BacklogFolds {
    pub collapsed: HashSet<(SectionKind, String)>,
    pub folded_lanes: Option<HashSet<SectionKind>>,
}

/// Which half of the backlog the view shows: the open lanes on the Desk's
/// Backlog tab, or the completed history on its Done tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacklogView {
    Open,
    Done,
}

impl BacklogView {
    fn sections(self) -> &'static [SectionKind] {
        match self {
            Self::Open => &[SectionKind::Soon, SectionKind::Someday],
            Self::Done => &[SectionKind::Completed],
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Open => 0,
            Self::Done => 1,
        }
    }
}

/// What the inline editor is editing (spec §6.2, §9.5).
enum EditTarget {
    /// A task addressed by its section, line, and text when editing started —
    /// the line disambiguates duplicate texts and external edits mid-edit are
    /// detected (§6.5).
    Existing {
        section: SectionKind,
        line: u32,
        original_text: String,
    },
    /// The `+` affordance: a brand-new task appended to the section, inside
    /// `category` when the add started from a group (V17 §5.3).
    New {
        section: SectionKind,
        category: Option<String>,
    },
}

struct EditState {
    target: EditTarget,
    editor: Entity<Editor>,
    _subscription: Subscription,
}

/// How a task leaves the backlog for today's note: checked off into the
/// Completed section, or moved wholesale onto today's plan.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TodayWrite {
    Done,
    Move,
}

/// A row of a column: the lane's title, a category header, or a task. Both
/// the renderer and the keyboard cursor index into this list, so there is
/// one ordering to keep in step (V17 §5.2). An open lane's title is its row
/// 0, so a lane always has somewhere for the cursor to land.
enum BacklogRow<'a> {
    Lane {
        /// Open tasks in the lane, whether or not it is folded.
        count: usize,
        folded: bool,
    },
    Category {
        name: &'a str,
        /// Open tasks in the group, whether or not it is collapsed.
        count: usize,
        collapsed: bool,
    },
    Task {
        task: &'a BacklogTask,
    },
}

/// The keyboard cursor: a column plus a row index into the tasks that column
/// renders. Addressing by index (rather than by task identity) is what lets
/// the selection survive a re-parse — the index is clamped against the current
/// parse every time it is read, so a task disappearing under the cursor moves
/// the cursor rather than losing it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TaskSelection {
    section: SectionKind,
    index: usize,
}

pub struct BacklogPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    view: BacklogView,
    /// Set by the Desk while a row sits above the first lane (the Next
    /// card): moving up from the top then releases the selection to it.
    row_above: bool,
    vault_status: VaultStatus,
    /// The open `backlog.md` buffer — panel writes go through it so an open
    /// editor tab and the panel stay coherent (spec §6.4). `None` while the
    /// workspace isn't a vault or the file doesn't exist yet.
    buffer: Option<Entity<Buffer>>,
    /// The path `buffer` was resolved for (kept even when the file is
    /// missing, to notice `[backlog]` config changes).
    buffer_path: Option<PathBuf>,
    _buffer_subscription: Option<Subscription>,
    backlog: Backlog,
    /// The categories the user has closed, restored from the key-value store
    /// on load and written back on every change (V17 §5.4).
    collapsed: HashSet<(SectionKind, String)>,
    /// The lanes the user has folded, remembered the same way (V43 §8.3).
    folded_lanes: HashSet<SectionKind>,
    pending_serialization: Option<Task<()>>,
    selected: Option<TaskSelection>,
    /// The folder each linked note lives in, by link target, resolved on
    /// every reparse for the rows' meta lines.
    link_folders: HashMap<String, Option<String>>,
    /// One scroll position per view, so a keyboard motion can bring the
    /// selected row into view.
    scroll_handles: [ScrollHandle; 2],
    /// The row lit by the copy flash, if one is running.
    copy_flash: Option<TaskSelection>,
    copy_flash_task: Option<Task<()>>,
    edit_state: Option<EditState>,
    /// A mark-done is running its two ordered writes; checkboxes are disabled
    /// until it settles (spec §6.3).
    mark_in_flight: bool,
    load_buffer_task: Option<Task<()>>,
    reparse_task: Option<Task<()>>,
    /// The email-capture service, for the status row (spec v9 §10.3). The
    /// service lives independently of the panel; this is display only.
    gmail_service: Option<Entity<GmailService>>,
    /// The inbox-capture service, for its status row (V13 §10.4) — display
    /// only, like the Gmail one.
    inbox_service: Option<Entity<InboxService>>,
    /// The Readwise sync service, for its status row (V31 §8.5) — display
    /// only, like the other two.
    readwise_service: Option<Entity<ReadwiseService>>,
    vault_sync_service: Option<Entity<VaultSyncService>>,
    /// The vault's reminders, whose closed and dropped ones Done lists
    /// beside the finished tasks (V45 §9.1).
    reminder_service: Option<Entity<ReminderService>>,
    _subscriptions: Vec<Subscription>,
}

impl BacklogPanel {
    /// Reads the remembered fold state, for the Desk to build the view with.
    pub async fn load_folds(
        workspace: WeakEntity<Workspace>,
        cx: &mut AsyncWindowContext,
    ) -> BacklogFolds {
        let key = workspace
            .read_with(cx, |workspace, _| Self::serialization_key(workspace))
            .ok()
            .flatten();
        let Some(store) = cx.update(|_, cx| KeyValueStore::global(cx)).ok() else {
            return BacklogFolds::default();
        };
        match key {
            Some(key) => cx
                .background_spawn(async move { store.read_kvp(&key) })
                .await
                .context("loading the backlog's collapsed groups")
                .log_err()
                .flatten()
                .and_then(|value| serde_json::from_str::<SerializedBacklogPanel>(&value).log_err())
                .map(|state| BacklogFolds {
                    collapsed: state
                        .collapsed
                        .into_iter()
                        .filter_map(|(section, name)| Some((SectionKind::from_id(&section)?, name)))
                        .collect(),
                    folded_lanes: state.folded_lanes.map(|lanes| {
                        lanes
                            .iter()
                            .filter_map(|id| SectionKind::from_id(id))
                            .collect()
                    }),
                })
                .unwrap_or_default(),
            None => BacklogFolds::default(),
        }
    }

    /// The selected row as `(section id, index)`, for keystroke tests.
    #[cfg(test)]
    pub(crate) fn selected_for_tests(&self) -> Option<(&'static str, usize)> {
        self.selection()
            .map(|selection| (selection.section.id(), selection.index))
    }

    /// The row text and meta line of the first Soon task containing
    /// `needle`, for label tests.
    #[cfg(test)]
    pub(crate) fn row_for_tests(
        &self,
        needle: &str,
        cx: &App,
    ) -> Option<(String, Option<(String, bool)>)> {
        let task = self
            .backlog
            .section(SectionKind::Soon)
            .iter()
            .find(|task| task.text.contains(needle))?;
        Some((
            thock_sync_core::strip_reminder(&task.text),
            self.task_meta(task, cx),
        ))
    }

    /// Whether the new-task editor's row lies inside the list's viewport
    /// as last drawn, and how far the list is scrolled, for keystroke tests.
    #[cfg(test)]
    pub(crate) fn new_task_row_in_view_for_tests(&self) -> Option<(bool, gpui::Pixels)> {
        let EditTarget::New { section, category } = &self.edit_state.as_ref()?.target else {
            return None;
        };
        let handle = &self.scroll_handles[self.view.index()];
        let row = handle.bounds_for_item(self.new_task_row_index(*section, category.as_deref()))?;
        let viewport = handle.bounds();
        let offset = handle.offset();
        let in_view =
            row.top() + offset.y >= viewport.top() && row.bottom() + offset.y <= viewport.bottom();
        Some((in_view, offset.y))
    }

    pub fn new(
        workspace: &mut Workspace,
        folds: BacklogFolds,
        _window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = workspace.weak_handle();
        cx.new(|cx| {
            let project_subscription = cx.subscribe(&project, |this: &mut Self, _, event, cx| {
                if matches!(
                    event,
                    project::Event::WorktreeAdded(_)
                        | project::Event::WorktreeRemoved(_)
                        | project::Event::WorktreeUpdatedEntries(..)
                ) {
                    this.refresh_vault_status(cx);
                }
            });
            let gmail_service = gmail_service::service_for_project(&project, cx);
            let inbox_service = inbox_service::service_for_project(&project, cx);
            let readwise_service = readwise_service::service_for_project(&project, cx);
            let vault_sync_service = vault_sync::service_for_project(&project, cx);
            let reminder_service = reminder_service::service_for_project(&project, cx);
            let mut subscriptions = vec![project_subscription];
            if let Some(service) = &gmail_service {
                subscriptions.push(cx.observe(service, |_, _, cx| cx.notify()));
            }
            if let Some(service) = &inbox_service {
                subscriptions.push(cx.observe(service, |_, _, cx| cx.notify()));
            }
            if let Some(service) = &readwise_service {
                subscriptions.push(cx.observe(service, |_, _, cx| cx.notify()));
            }
            if let Some(service) = &vault_sync_service {
                subscriptions.push(cx.observe(service, |_, _, cx| cx.notify()));
            }
            if let Some(service) = &reminder_service {
                subscriptions
                    .push(cx.subscribe(service, |_, _, _: &ReminderEvent, cx| cx.notify()));
            }
            // Reminder chips relabel when the day turns.
            let reminder_day = ReminderDay::global(cx);
            subscriptions.push(cx.observe(&reminder_day, |_, _, cx| cx.notify()));
            let mut this = Self {
                workspace: weak_workspace,
                project,
                focus_handle: cx.focus_handle(),
                view: BacklogView::Open,
                row_above: false,
                vault_status: VaultStatus::NotAVault,
                buffer: None,
                buffer_path: None,
                _buffer_subscription: None,
                backlog: Backlog::default(),
                collapsed: folds.collapsed,
                // Soon opens expanded and Someday starts folded (§8.3).
                folded_lanes: folds
                    .folded_lanes
                    .unwrap_or_else(|| HashSet::from_iter([SectionKind::Someday])),
                pending_serialization: None,
                selected: None,
                link_folders: HashMap::default(),
                scroll_handles: std::array::from_fn(|_| ScrollHandle::new()),
                copy_flash: None,
                copy_flash_task: None,
                edit_state: None,
                mark_in_flight: false,
                load_buffer_task: None,
                reparse_task: None,
                gmail_service,
                inbox_service,
                readwise_service,
                vault_sync_service,
                reminder_service,
                _subscriptions: subscriptions,
            };
            this.vault_status = this.detect_vault_status(cx);
            this.ensure_buffer(cx);
            this
        })
    }

    fn serialization_key(workspace: &Workspace) -> Option<String> {
        workspace
            .database_id()
            .map(|id| i64::from(id).to_string())
            .or(workspace.session_id())
            .map(|id| format!("{BACKLOG_PANEL_KEY}-{id}"))
    }

    fn serialize(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self
            .workspace
            .read_with(cx, |workspace, _| Self::serialization_key(workspace))
            .ok()
            .flatten()
        else {
            return;
        };
        let state = SerializedBacklogPanel {
            collapsed: self
                .collapsed
                .iter()
                .map(|(section, name)| (section.id().to_string(), name.clone()))
                .collect(),
            folded_lanes: Some(
                self.folded_lanes
                    .iter()
                    .map(|section| section.id().to_string())
                    .collect(),
            ),
        };
        let Some(payload) = serde_json::to_string(&state).log_err() else {
            return;
        };
        let store = KeyValueStore::global(cx);
        // Replacing the task cancels a superseded write, which is what we
        // want: the newest state owns the record.
        self.pending_serialization = Some(cx.background_spawn(async move {
            if let Err(error) = store.write_kvp(key, payload).await {
                log::error!("couldn't save the backlog panel's collapsed groups: {error}");
            }
        }));
    }

    fn vault(&self) -> Option<&Vault> {
        match &self.vault_status {
            VaultStatus::Valid(vault) => Some(vault),
            _ => None,
        }
    }

    /// The vault's resolved section headings (V19 §5.2) — what the parser
    /// matches, the write helpers create, and the column headers show.
    fn headings(&self) -> BacklogHeadings {
        self.vault()
            .map(|vault| vault.config.backlog.headings.clone())
            .unwrap_or_default()
    }

    fn detect_vault_status(&self, cx: &App) -> VaultStatus {
        match self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        {
            Some(root) => Vault::detect(&root),
            None => VaultStatus::NotAVault,
        }
    }

    fn refresh_vault_status(&mut self, cx: &mut Context<Self>) {
        // A worktree event may have moved a linked note.
        self.refresh_link_folders(cx);
        let status = self.detect_vault_status(cx);
        if status != self.vault_status {
            self.vault_status = status;
            // A config change can rename the parsed headings without moving
            // the file, so the buffer must be re-read under the new ones.
            self.reparse(cx);
            cx.notify();
        }
        // Re-resolve the buffer either way: a worktree event may mean
        // `backlog.md` just appeared (created by a wrap skill or by hand).
        self.ensure_buffer(cx);
    }

    /// Points the panel's buffer at the vault's current backlog path, opening
    /// the file's buffer when it exists. Idempotent; called on every vault /
    /// worktree change.
    fn ensure_buffer(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.vault().map(Vault::backlog_path) else {
            if self.buffer.is_some() || self.buffer_path.is_some() {
                self.buffer = None;
                self.buffer_path = None;
                self._buffer_subscription = None;
                self.backlog = Backlog::default();
                self.selected = None;
                self.copy_flash = None;
                self.edit_state = None;
                self.load_buffer_task = None;
                cx.notify();
            }
            return;
        };
        if self.buffer.is_some() && self.buffer_path.as_ref() == Some(&path) {
            // Cancel any in-flight load for a superseded path, or its late
            // completion would repoint the panel away from the current config.
            self.load_buffer_task = None;
            return;
        }
        let project = self.project.clone();
        // Loads are idempotent and read-only until the final assignment, so
        // replacing an in-flight load with a newer one is safe.
        self.load_buffer_task = Some(cx.spawn(async move |this, cx| {
            let exists = cx
                .background_spawn({
                    let path = path.clone();
                    async move { path.is_file() }
                })
                .await;
            let buffer = if exists {
                project
                    .update(cx, |project, cx| project.open_local_buffer(&path, cx))
                    .await
                    .log_err()
            } else {
                None
            };
            this.update(cx, |this, cx| {
                this.buffer_path = Some(path);
                this._buffer_subscription = buffer.as_ref().map(|buffer| {
                    cx.subscribe(buffer, |this, _, event: &BufferEvent, cx| {
                        if matches!(event, BufferEvent::Edited { .. } | BufferEvent::Reloaded) {
                            this.schedule_reparse(cx);
                        }
                    })
                });
                this.buffer = buffer;
                this.reparse(cx);
                cx.notify();
            })
            .log_err();
        }));
    }

    fn schedule_reparse(&mut self, cx: &mut Context<Self>) {
        self.reparse_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REPARSE_DEBOUNCE).await;
            this.update(cx, |this, cx| this.reparse(cx)).log_err();
        }));
    }

    fn reparse(&mut self, cx: &mut Context<Self>) {
        let backlog = match &self.buffer {
            Some(buffer) => parse_backlog(&buffer.read(cx).text(), &self.headings()),
            None => Backlog::default(),
        };
        if backlog != self.backlog {
            self.backlog = backlog;
            self.refresh_link_folders(cx);
            cx.notify();
        }
    }

    fn show_error(&self, message: String, cx: &mut Context<Self>) {
        // Deferred: this can be reached synchronously from action handlers
        // whose callers hold the workspace lease (the V5 double-lease trap).
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.show_error(message, cx))
                .log_err();
        });
    }

    /// Applies `edits` to the backlog buffer and saves it. All edit ranges
    /// address the buffer's current text (`Buffer::edit` resolves the shifts).
    fn write_edits(
        &mut self,
        buffer: Entity<Buffer>,
        mut edits: Vec<backlog::Edit>,
        error_context: &'static str,
        cx: &mut Context<Self>,
    ) {
        edits.sort_by_key(|edit| edit.range.start);
        buffer.update(cx, |buffer, cx| {
            buffer.edit(
                edits.into_iter().map(|edit| (edit.range, edit.new_text)),
                None,
                cx,
            );
        });
        // Re-parse now rather than waiting for the debounced Edited event, so
        // the panel never renders (or accepts gestures against) the pre-edit
        // state.
        self.reparse(cx);
        let save = self
            .project
            .update(cx, |project, cx| project.save_buffer(buffer, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = save.await {
                this.update(cx, |this, cx| {
                    this.show_error(format!("{error_context}: {error}"), cx);
                })
                .log_err();
            }
        })
        .detach();
    }

    // --- Keyboard navigation (spec §6.6) ---

    fn completed_tasks(&self) -> Vec<&BacklogTask> {
        completed_tasks(&self.backlog)
    }

    fn visible_rows(&self, section: SectionKind) -> Vec<BacklogRow<'_>> {
        column_rows(&self.backlog, section, &self.collapsed, &self.folded_lanes)
    }

    /// The lanes the current view lays out, in order.
    fn sections(&self) -> &'static [SectionKind] {
        self.view.sections()
    }

    /// Shows the open lanes or the completed history (V43 §8.1). The cursor
    /// does not carry across: a Done row means nothing on the Backlog tab.
    pub fn set_view(&mut self, view: BacklogView, cx: &mut Context<Self>) {
        if self.view != view {
            self.view = view;
            self.selected = None;
            self.edit_state = None;
            cx.notify();
        }
    }

    pub fn view(&self) -> BacklogView {
        self.view
    }

    /// Tells the view whether a selectable row sits above its first lane.
    pub fn set_row_above(&mut self, row_above: bool) {
        self.row_above = row_above;
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.take().is_some() {
            cx.notify();
        }
    }

    /// Open tasks across Soon and Someday — the Backlog tab's count.
    pub fn open_count(&self) -> usize {
        [SectionKind::Soon, SectionKind::Someday]
            .into_iter()
            .map(|section| self.open_count_in(section))
            .sum()
    }

    fn open_count_in(&self, section: SectionKind) -> usize {
        self.backlog
            .section(section)
            .iter()
            .filter(|task| !task.checked)
            .count()
    }

    /// Where a `(section, index)` row sits in the view's one scrolling list.
    fn flat_index(&self, selection: TaskSelection) -> usize {
        self.sections()
            .iter()
            .take_while(|section| **section != selection.section)
            .map(|section| self.visible_rows(*section).len())
            .sum::<usize>()
            + selection.index
    }

    /// The selection resolved against the current parse: same column, index
    /// clamped to what that column renders now. `None` once the column is
    /// empty, so the highlight never points at a row that isn't there.
    fn selection(&self) -> Option<TaskSelection> {
        let selected = self.selected?;
        let len = self.visible_rows(selected.section).len();
        len.checked_sub(1).map(|last| TaskSelection {
            section: selected.section,
            index: selected.index.min(last),
        })
    }

    fn selected_task(&self) -> Option<(SectionKind, BacklogTask)> {
        let selection = self.selection()?;
        match self.visible_rows(selection.section).get(selection.index)? {
            BacklogRow::Task { task } => Some((selection.section, (*task).clone())),
            BacklogRow::Category { .. } | BacklogRow::Lane { .. } => None,
        }
    }

    /// The group the cursor is in: a selected header's own category, or the
    /// one holding the selected task. `None` on a loose task or a lane title.
    fn selected_category(&self) -> Option<(SectionKind, String)> {
        let selection = self.selection()?;
        match self.visible_rows(selection.section).get(selection.index)? {
            BacklogRow::Category { name, .. } => Some((selection.section, (*name).to_string())),
            BacklogRow::Task { task } => {
                task.category.clone().map(|name| (selection.section, name))
            }
            BacklogRow::Lane { .. } => None,
        }
    }

    /// Whether the selected row is a category header, and if so whether it is
    /// closed — read as one step so the borrow ends before a mutation.
    fn selected_header_collapsed(&self) -> Option<bool> {
        let selection = self.selection()?;
        match self.visible_rows(selection.section).get(selection.index)? {
            BacklogRow::Category { collapsed, .. } => Some(*collapsed),
            BacklogRow::Task { .. } | BacklogRow::Lane { .. } => None,
        }
    }

    /// Whether the selected row is a lane title, and if so whether the lane
    /// is folded.
    fn selected_lane_folded(&self) -> Option<bool> {
        let selection = self.selection()?;
        match self.visible_rows(selection.section).get(selection.index)? {
            BacklogRow::Lane { folded, .. } => Some(*folded),
            _ => None,
        }
    }

    fn set_lane_folded(&mut self, section: SectionKind, folded: bool, cx: &mut Context<Self>) {
        let changed = if folded {
            self.folded_lanes.insert(section)
        } else {
            self.folded_lanes.remove(&section)
        };
        if !changed {
            return;
        }
        if folded {
            self.select_row(section, 0, cx);
        }
        self.serialize(cx);
        cx.notify();
    }

    fn collapse_category(
        &mut self,
        _: &CollapseBacklogCategory,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_category_collapsed(true, cx);
    }

    fn expand_category(
        &mut self,
        _: &ExpandBacklogCategory,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_category_collapsed(false, cx);
    }

    fn set_category_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.selected_lane_folded().is_some()
            && let Some(selection) = self.selection()
        {
            self.set_lane_folded(selection.section, collapsed, cx);
            return;
        }
        let Some((section, name)) = self.selected_category() else {
            return;
        };
        let changed = if collapsed {
            self.collapsed.insert((section, name.clone()))
        } else {
            self.collapsed.remove(&(section, name.clone()))
        };
        if !changed {
            return;
        }
        // A closed group swallows its tasks, so the cursor moves to the
        // header rather than sliding onto whatever took the index.
        if collapsed {
            self.select_category_header(section, &name);
        }
        self.serialize(cx);
        cx.notify();
    }

    fn select_category_header(&mut self, section: SectionKind, name: &str) {
        if let Some(index) = self.visible_rows(section).iter().position(
            |row| matches!(row, BacklogRow::Category { name: header, .. } if *header == name),
        ) {
            self.selected = Some(TaskSelection { section, index });
            let flat = self.flat_index(TaskSelection { section, index });
            self.scroll_handles[self.view.index()].scroll_to_item(flat);
        }
    }

    /// Where the cursor lands when a motion arrives with nothing selected: the
    /// first lane of the view that has anything in it.
    fn first_populated_column(&self) -> Option<SectionKind> {
        self.sections()
            .iter()
            .copied()
            .find(|section| !self.visible_rows(*section).is_empty())
    }

    fn last_populated_column(&self) -> Option<SectionKind> {
        self.sections()
            .iter()
            .rev()
            .copied()
            .find(|section| !self.visible_rows(*section).is_empty())
    }

    /// The populated lane after or before `section` in the view.
    fn neighbour_column(&self, section: SectionKind, forward: bool) -> Option<SectionKind> {
        let sections = self.sections();
        let position = sections
            .iter()
            .position(|candidate| *candidate == section)?;
        let candidates: Vec<SectionKind> = if forward {
            sections[position + 1..].to_vec()
        } else {
            sections[..position].iter().rev().copied().collect()
        };
        candidates
            .into_iter()
            .find(|section| !self.visible_rows(*section).is_empty())
    }

    fn select_row(&mut self, section: SectionKind, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(TaskSelection { section, index });
        let flat = self.flat_index(TaskSelection { section, index });
        self.scroll_handles[self.view.index()].scroll_to_item(flat);
        cx.notify();
    }

    /// Down moves through the lane and on into the next lane's title; the
    /// last row of the view holds.
    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        match self.selection() {
            Some(selection) => {
                let last = self.visible_rows(selection.section).len().saturating_sub(1);
                if selection.index < last {
                    self.select_row(selection.section, selection.index + 1, cx);
                } else if let Some(next) = self.neighbour_column(selection.section, true) {
                    self.select_row(next, 0, cx);
                }
            }
            None => {
                if let Some(section) = self.first_populated_column() {
                    self.select_row(section, 0, cx);
                }
            }
        }
    }

    /// Up moves through the lane and on into the previous lane's last row.
    /// From the very first row, the selection is released to whatever the
    /// Desk keeps above the lanes, when it told us there is something.
    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selection() {
            Some(selection) => {
                if selection.index > 0 {
                    self.select_row(selection.section, selection.index - 1, cx);
                } else if let Some(previous) = self.neighbour_column(selection.section, false) {
                    let last = self.visible_rows(previous).len().saturating_sub(1);
                    self.select_row(previous, last, cx);
                } else if self.row_above {
                    self.selected = None;
                    cx.notify();
                    cx.propagate();
                }
            }
            None => {
                if let Some(section) = self.last_populated_column() {
                    let last = self.visible_rows(section).len().saturating_sub(1);
                    self.select_row(section, last, cx);
                }
            }
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(section) = self.first_populated_column() {
            self.select_row(section, 0, cx);
        }
    }

    fn select_last(&mut self, _: &SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(section) = self.last_populated_column() {
            let last = self.visible_rows(section).len().saturating_sub(1);
            self.select_row(section, last, cx);
        }
    }

    fn select_next_column(
        &mut self,
        _: &SelectNextBacklogColumn,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_column(true, cx);
    }

    fn select_previous_column(
        &mut self,
        _: &SelectPreviousBacklogColumn,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_column(false, cx);
    }

    /// Jumps to the title of the nearest lane in `forward`'s direction, or
    /// to the current lane's own title when there is none that way.
    fn select_column(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(current) = self.selection() else {
            if let Some(section) = self.first_populated_column() {
                self.select_row(section, 0, cx);
            }
            return;
        };
        match self.neighbour_column(current.section, forward) {
            Some(destination) => self.select_row(destination, 0, cx),
            None if !forward => self.select_row(current.section, 0, cx),
            None => {}
        }
    }

    fn edit_selected_task(
        &mut self,
        _: &EditBacklogTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Completed is an audit trail, not an editable column (§4.3).
        let Some((section, task)) = self
            .selected_task()
            .filter(|(section, _)| *section != SectionKind::Completed)
        else {
            return;
        };
        self.start_edit(
            EditTarget::Existing {
                section,
                line: task.line,
                original_text: task.text.clone(),
            },
            &task.text,
            window,
            cx,
        );
    }

    fn add_task(&mut self, _: &AddBacklogTask, window: &mut Window, cx: &mut Context<Self>) {
        let section = match self.selection().map(|selection| selection.section) {
            Some(SectionKind::Completed) => return,
            Some(section) => section,
            None => SectionKind::Soon,
        };
        let category = self.selected_category().map(|(_, name)| name);
        self.start_add(section, category, window, cx);
    }

    /// Opens the new-task editor at the end of a group (or of the column's
    /// uncategorized region). A closed group is opened first — otherwise the
    /// editor would be typing into nothing.
    fn start_add(
        &mut self,
        section: SectionKind,
        category: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(name) = &category
            && self.collapsed.remove(&(section, name.clone()))
        {
            self.serialize(cx);
        }
        self.start_edit(EditTarget::New { section, category }, "", window, cx);
    }

    fn complete_selected_task(
        &mut self,
        _: &CompleteBacklogTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((section, task)) = self
            .selected_task()
            .filter(|(section, _)| *section != SectionKind::Completed)
        else {
            return;
        };
        self.mark_done(section, task.line, task.text, window, cx);
    }

    fn move_selected_task_to_today(
        &mut self,
        _: &MoveBacklogTaskToToday,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((section, task)) = self
            .selected_task()
            .filter(|(section, _)| *section != SectionKind::Completed)
        else {
            return;
        };
        self.send_to_today(section, task.line, task.text, TodayWrite::Move, window, cx);
    }

    fn move_selected_task_right(
        &mut self,
        _: &MoveBacklogTaskRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selected_task(SectionKind::Someday, cx);
    }

    fn move_selected_task_left(
        &mut self,
        _: &MoveBacklogTaskLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selected_task(SectionKind::Soon, cx);
    }

    /// Moves the selected task to `destination` (the Soon ↔ Someday pair only —
    /// completing a task is its own gesture), and lets the cursor follow it.
    fn move_selected_task(&mut self, destination: SectionKind, cx: &mut Context<Self>) {
        let Some((section, task)) = self.selected_task() else {
            return;
        };
        if section == destination || section == SectionKind::Completed {
            return;
        }
        let moved_text = task.text.clone();
        if !self.move_task(section, task.line, task.text, cx) {
            return;
        }
        // The task keeps its category, so it lands at the end of that group
        // rather than at the end of the column.
        let moved = self
            .visible_rows(destination)
            .iter()
            .position(|row| matches!(row, BacklogRow::Task { task } if task.text == moved_text));
        if let Some(index) = moved {
            self.select_row(destination, index, cx);
        }
    }

    fn copy_selected_task(
        &mut self,
        _: &CopyBacklogTask,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((selection, (section, task))) = self.selection().zip(self.selected_task()) else {
            return;
        };
        // Prefer the file's own text for the task — checkbox marker, trailing
        // completion stamp and indented children included — re-located in the
        // live buffer so an edit since the last parse can't yield a stale span.
        let from_buffer = self
            .buffer
            .as_ref()
            .map(|buffer| buffer.read(cx).text())
            .and_then(|text| {
                let located = parse_backlog(&text, &self.headings())
                    .locate_task(section, task.line, &task.text)
                    .map(|located| located.span.clone())?;
                Some(text.get(located)?.trim_end().to_string())
            });
        let markdown = from_buffer.unwrap_or_else(|| {
            format!("- [{}] {}", if task.checked { 'x' } else { ' ' }, task.text)
        });
        // Leading newline so pasting lands the task on its own line, the way
        // vim's linewise `yy` does.
        cx.write_to_clipboard(ClipboardItem::new_string(format!("\n{markdown}")));
        self.flash_row(selection, cx);
    }

    /// Lights the row for a moment — the panel's answer to vim's yank
    /// highlight, since a copy is otherwise completely silent.
    fn flash_row(&mut self, row: TaskSelection, cx: &mut Context<Self>) {
        self.copy_flash = Some(row);
        cx.notify();
        // Replacing the task cancels any running flash, which is what we want:
        // the newest copy owns the highlight.
        self.copy_flash_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPY_FLASH_DURATION).await;
            this.update(cx, |this, cx| {
                this.copy_flash = None;
                cx.notify();
            })
            .log_err();
        }));
    }

    fn reveal_selected_task(
        &mut self,
        _: &RevealBacklogTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, task)) = self.selected_task() else {
            return;
        };
        self.reveal_task(task.line, window, cx);
    }

    /// Escape hands focus back to the note the user was writing in — a dock
    /// panel must never trap the keyboard.
    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer_in(window, move |_, window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    if let Some(item) = workspace.active_item(cx) {
                        item.item_focus_handle(cx).focus(window, cx);
                    }
                })
                .log_err();
        });
    }

    // --- Inline editing (spec §6.2) ---

    fn start_edit(
        &mut self,
        target: EditTarget,
        initial_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            // The hidden trailing comment stays out of the editor too;
            // `commit_edit` re-attaches it so a rename can't destroy it.
            editor.set_text(strip_trailing_metadata(initial_text), window, cx);
            if let EditTarget::New { .. } = target {
                editor.set_placeholder_text("New task", window, cx);
            }
            editor
        });
        editor.update(cx, |editor, cx| {
            editor.select_all(&editor::actions::SelectAll, window, cx);
        });
        let subscription = cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &EditorEvent, window, cx| {
                if matches!(event, EditorEvent::Blurred) {
                    this.commit_edit(window, cx);
                }
            },
        );
        editor.read(cx).focus_handle(cx).focus(window, cx);
        if let EditTarget::New { section, category } = &target {
            let row = self.new_task_row_index(*section, category.as_deref());
            self.scroll_handles[self.view.index()].scroll_to_item(row);
        }
        self.edit_state = Some(EditState {
            target,
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// The flat row the new-task editor renders at: after the last row of
    /// the group it is adding to, where `render_open_view` places it.
    fn new_task_row_index(&self, section: SectionKind, category: Option<&str>) -> usize {
        let offset = self.flat_index(TaskSelection { section, index: 0 });
        let rows = self.visible_rows(section);
        let mut group: Option<&str> = None;
        for (index, row) in rows.iter().enumerate() {
            if let BacklogRow::Category { name, .. } = row {
                if group == category {
                    return offset + index;
                }
                group = Some(name);
            }
        }
        offset + rows.len()
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_state.is_some() {
            self.commit_edit(window, cx);
            return;
        }
        if self.selection().is_none() {
            // Nothing here to act on; the Desk may have a row above.
            cx.propagate();
            return;
        }
        if let Some(folded) = self.selected_lane_folded()
            && let Some(selection) = self.selection()
        {
            self.set_lane_folded(selection.section, !folded, cx);
            return;
        }
        if let Some(collapsed) = self.selected_header_collapsed() {
            self.set_category_collapsed(!collapsed, cx);
            return;
        }
        self.edit_selected_task(&EditBacklogTask, window, cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_state.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
            return;
        }
        self.focus_editor(window, cx);
    }

    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.edit_state.take() else {
            return;
        };
        cx.notify();
        let was_focused = state.editor.read(cx).focus_handle(cx).is_focused(window);
        if was_focused {
            self.focus_handle.focus(window, cx);
        }
        let new_text = state.editor.read(cx).text(cx).trim().to_string();
        match state.target {
            EditTarget::Existing {
                section,
                line,
                original_text,
            } => {
                // Committing an empty string is a revert, not a delete (§6.2).
                // The editor showed the text without its hidden trailing
                // comment, so "unchanged" is judged against that view.
                if new_text.is_empty() || new_text == strip_trailing_metadata(&original_text) {
                    return;
                }
                let new_text = restore_hidden_suffix(&original_text, &new_text);
                let Some(buffer) = self.buffer.clone() else {
                    self.show_error(
                        "backlog.md is no longer open, so the edit wasn't applied.".to_string(),
                        cx,
                    );
                    return;
                };
                let text = buffer.read(cx).text();
                let backlog = parse_backlog(&text, &self.headings());
                let Some(task) = backlog.locate_task(section, line, &original_text) else {
                    // The line changed under the edit; dropping beats guessing
                    // (spec §6.5).
                    self.show_error(
                        "That task changed outside the panel, so the edit wasn't applied."
                            .to_string(),
                        cx,
                    );
                    return;
                };
                let edit = backlog::rename_task_edit(task, &new_text);
                self.write_edits(buffer, vec![edit], "Couldn't update backlog.md", cx);
            }
            EditTarget::New { section, category } => {
                if new_text.is_empty() {
                    return;
                }
                self.append_new_task(section, category, &new_text, cx);
            }
        }
    }

    fn append_new_task(
        &mut self,
        section: SectionKind,
        category: Option<String>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let block = backlog::new_task_block(text);
        let headings = self.headings();
        if let Some(buffer) = self.buffer.clone() {
            let current = buffer.read(cx).text();
            let edit = backlog::append_to_section_edit(
                &current,
                section,
                category.as_deref(),
                &block,
                &headings,
            );
            self.write_edits(
                buffer,
                vec![edit],
                "Couldn't add the task to backlog.md",
                cx,
            );
            return;
        }
        // No file yet: create it around the new task (create-on-first-write,
        // spec §6.5), then adopt its buffer.
        let Some(path) = self.vault().map(Vault::backlog_path) else {
            return;
        };
        let write = cx.background_spawn(async move {
            let current = match std::fs::read_to_string(&path) {
                Ok(current) => current,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    backlog::DEFAULT_BACKLOG.to_string()
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("reading {}", path.display()));
                }
            };
            let edit = backlog::append_to_section_edit(
                &current,
                section,
                category.as_deref(),
                &block,
                &headings,
            );
            let new_text = backlog::apply_edits(&current, vec![edit]);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&path, new_text)
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => this.ensure_buffer(cx),
                Err(error) => {
                    this.show_error(format!("Couldn't create backlog.md: {error}"), cx);
                }
            })
            .log_err();
        })
        .detach();
    }

    // --- Mark done / move to today (spec §6.3) ---

    /// Checking a task runs two ordered writes: append `- [x] …` to today's
    /// daily note (created from template if missing), then move the task to
    /// the backlog's Completed section. If the note write fails the backlog
    /// is left untouched, so it never claims a completion no note records.
    fn mark_done(
        &mut self,
        section: SectionKind,
        line: u32,
        task_text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send_to_today(section, line, task_text, TodayWrite::Done, window, cx);
    }

    /// Sends a task to today's daily note with the same two ordered writes as
    /// `mark_done`: the note write first, and only if it succeeds is the
    /// backlog touched — completed for a mark-done, removed for a move.
    fn send_to_today(
        &mut self,
        section: SectionKind,
        line: u32,
        task_text: String,
        write: TodayWrite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mark_in_flight || self.buffer.is_none() {
            return;
        }
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        // A moved task lands in the note verbatim — children included — so
        // the block is captured before the backlog loses it.
        let note_block = match write {
            TodayWrite::Done => format!("- [x] {task_text}\n"),
            TodayWrite::Move => {
                let Some(buffer) = self.buffer.as_ref() else {
                    return;
                };
                let text = buffer.read(cx).text();
                let backlog = parse_backlog(&text, &self.headings());
                let Some(task) = backlog.locate_task(section, line, &task_text) else {
                    self.show_error(
                        "That task changed outside the panel, so it wasn't moved.".to_string(),
                        cx,
                    );
                    return;
                };
                backlog::task_block(&text, task)
            }
        };
        self.mark_in_flight = true;
        cx.notify();

        let project = self.project.clone();
        let heading = vault.config.day_planner.heading_names();
        let now = Local::now();
        let today = now.date_naive();
        let time = now.time();
        let fs = project.read(cx).fs().clone();
        let ensure = cx.background_spawn(async move {
            ensure_note(&fs, &vault, NoteKind::Daily, today, time).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let note_result = async {
                let (note_path, outcome) = ensure.await?;
                if outcome != EnsureNoteOutcome::AlreadyExisted {
                    this.update(cx, |_, cx| {
                        crate::notes::announce_note(&project, &note_path, cx)
                    })?;
                }
                if outcome == EnsureNoteOutcome::CreatedWithoutTemplate {
                    this.update(cx, |this, cx| {
                        this.show_error(
                            "The daily template is missing, so today's note was created empty."
                                .to_string(),
                            cx,
                        );
                    })?;
                }
                let note_buffer = project
                    .update(cx, |project, cx| project.open_local_buffer(&note_path, cx))
                    .await?;
                note_buffer.update(cx, |buffer, cx| {
                    let edit =
                        backlog::append_block_to_note_edit(&buffer.text(), &heading, &note_block);
                    buffer.edit([(edit.range, edit.new_text)], None, cx);
                });
                project
                    .update(cx, |project, cx| project.save_buffer(note_buffer, cx))
                    .await?;
                anyhow::Ok(())
            }
            .await;
            if let Err(error) = note_result {
                this.update(cx, |this, cx| {
                    this.mark_in_flight = false;
                    let message = match write {
                        TodayWrite::Done => {
                            format!("Couldn't record the task in today's note: {error}")
                        }
                        TodayWrite::Move => {
                            format!("Couldn't move the task to today's note: {error}")
                        }
                    };
                    this.show_error(message, cx);
                    cx.notify();
                })
                .log_err();
                return;
            }

            let backlog_save = this.update(cx, |this, cx| {
                let buffer = this
                    .buffer
                    .clone()
                    .context("backlog.md is no longer open")?;
                let text = buffer.read(cx).text();
                let headings = this.headings();
                let backlog = parse_backlog(&text, &headings);
                let task = backlog
                    .locate_task(section, line, &task_text)
                    .context("the task changed while it was being sent to today")?;
                let mut edits = match write {
                    TodayWrite::Done => backlog::complete_task_edits(&text, task, today, &headings),
                    TodayWrite::Move => vec![backlog::Edit {
                        range: task.span.clone(),
                        new_text: String::new(),
                    }],
                };
                edits.sort_by_key(|edit| edit.range.start);
                buffer.update(cx, |buffer, cx| {
                    buffer.edit(
                        edits.into_iter().map(|edit| (edit.range, edit.new_text)),
                        None,
                        cx,
                    );
                });
                this.reparse(cx);
                anyhow::Ok(
                    this.project
                        .update(cx, |project, cx| project.save_buffer(buffer, cx)),
                )
            });
            let backlog_result = match backlog_save {
                Ok(Ok(save)) => save.await,
                Ok(Err(error)) | Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.mark_in_flight = false;
                if let Err(error) = backlog_result {
                    // Retrying is safe: the panel re-renders from the file,
                    // which still shows the task open (spec §6.5).
                    let message = match write {
                        TodayWrite::Done => format!(
                            "The task was recorded in today's note, but the backlog couldn't \
                             be updated: {error}"
                        ),
                        TodayWrite::Move => format!(
                            "The task was added to today's note, but the backlog couldn't \
                             be updated: {error}"
                        ),
                    };
                    this.show_error(message, cx);
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    /// Moves a task to the other open section (Soon ↔ Someday). Returns whether
    /// the move was written, so a caller can follow the task with the cursor.
    fn move_task(
        &mut self,
        section: SectionKind,
        line: u32,
        task_text: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let destination = match section {
            SectionKind::Soon => SectionKind::Someday,
            SectionKind::Someday => SectionKind::Soon,
            SectionKind::Completed => return false,
        };
        let Some(buffer) = self.buffer.clone() else {
            return false;
        };
        let text = buffer.read(cx).text();
        let headings = self.headings();
        let backlog = parse_backlog(&text, &headings);
        let Some(task) = backlog.locate_task(section, line, &task_text) else {
            self.show_error(
                "That task changed outside the panel, so it wasn't moved.".to_string(),
                cx,
            );
            return false;
        };
        let edits = backlog::move_task_edits(&text, task, destination, &headings);
        self.write_edits(buffer, edits, "Couldn't update backlog.md", cx);
        true
    }

    /// Opens `backlog.md` in the editor at the task's line (the Day Planner's
    /// reveal pattern) for anything beyond a text tweak.
    fn reveal_task(&mut self, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.vault().map(Vault::backlog_path) else {
            return;
        };
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let item = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_abs_path(
                        path,
                        OpenOptions {
                            visible: Some(OpenVisible::All),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                })?
                .await?;
            if let Some(editor) = item.downcast::<Editor>() {
                editor.update_in(cx, |editor, window, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let point = snapshot.clip_point(Point::new(line, 0), Bias::Left);
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()).nav_history(true),
                        window,
                        cx,
                        |selections| selections.select_ranges([point..point]),
                    );
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    // --- Rendering ---

    fn render_hint(&self, text: impl Into<SharedString>) -> Div {
        v_flex().p_3().child(
            Label::new(text.into())
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
    }

    fn is_editing(&self, section: SectionKind, line: u32, task_text: &str) -> bool {
        self.edit_state.as_ref().is_some_and(|state| {
            matches!(
                &state.target,
                EditTarget::Existing {
                    section: editing_section,
                    line: editing_line,
                    original_text,
                } if *editing_section == section
                    && *editing_line == line
                    && original_text == task_text
            )
        })
    }

    fn is_adding_to(&self, section: SectionKind, category: Option<&str>) -> bool {
        self.edit_state.as_ref().is_some_and(|state| {
            matches!(
                &state.target,
                EditTarget::New {
                    section: adding,
                    category: adding_category,
                } if *adding == section && adding_category.as_deref() == category
            )
        })
    }

    fn render_editor_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let state = self.edit_state.as_ref()?;
        Some(
            div()
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(cx.theme().colors().border_focused)
                .rounded_sm()
                .child(state.editor.clone())
                .into_any_element(),
        )
    }

    /// A task's text with its Markdown links rendered as clickable labels.
    /// Returns the text element only — the caller wraps it in the `LabelLike`
    /// that carries the row's size, color and truncation.
    fn render_task_text(&self, id: ElementId, text: &str, cx: &Context<Self>) -> AnyElement {
        render_markdown_row(id, text, &self.project, &self.workspace, cx)
    }

    /// A row's background: the copy flash outranks the selection highlight, so
    /// the pulse is visible even on the row the cursor is already sitting on.
    fn row_background(&self, row: TaskSelection, cx: &Context<Self>) -> Option<Hsla> {
        if self.copy_flash == Some(row) {
            return Some(cx.theme().colors().text_accent.alpha(0.2));
        }
        (self.selection() == Some(row)).then(|| cx.theme().colors().element_selected)
    }

    /// What a task row says under its text (V43 §8.3, V45 §8.5): its
    /// reminder's chip, then the date of the linked note it came from and the
    /// folder that note lives in. Only what the line already carries; a task
    /// with neither has no meta line. The flag is set when the reminder is
    /// overdue, for the warning colour.
    fn task_meta(&self, task: &BacklogTask, cx: &App) -> Option<(String, bool)> {
        let mut parts: Vec<String> = Vec::new();
        let today = ReminderClock::global(cx).today();
        let chip = thock_sync_core::parse_line(&task.text, None)
            .and_then(|reminder| reminder_chip(&reminder, today));
        if let Some(chip) = &chip {
            parts.push(format!("⏰ {}", chip.label));
        }
        if let Some(link) = task_link(&task.text) {
            if let Some(date) = link.date {
                parts.push(format!("from {}", crate::notes::format_date(date, "MMM D")));
            }
            if let Some(Some(folder)) = self.link_folders.get(&link.target) {
                parts.push(folder.clone());
            }
        }
        if parts.is_empty() {
            return None;
        }
        let overdue = chip.is_some_and(|chip| chip.state == ChipState::Overdue);
        Some((parts.join(" · "), overdue))
    }

    /// Resolves every linked note's folder in one pass over the vault's
    /// file list, so a render never walks the worktree.
    fn refresh_link_folders(&mut self, cx: &App) {
        let targets: HashSet<String> = [SectionKind::Soon, SectionKind::Someday]
            .into_iter()
            .flat_map(|section| self.backlog.section(section))
            .filter_map(|task| task_link(&task.text))
            .map(|link| link.target)
            .collect();
        let Some(worktree) = self.project.read(cx).visible_worktrees(cx).next() else {
            self.link_folders.clear();
            return;
        };
        let snapshot = worktree.read(cx).snapshot();
        let files: Vec<&RelPath> = snapshot
            .files(false, 0)
            .map(|entry| entry.path.as_ref())
            .collect();
        self.link_folders = targets
            .into_iter()
            .map(|target| {
                let folder = resolve_wikilink_target(&target, files.iter().copied())
                    .and_then(|path| path.parent())
                    .and_then(|parent| parent.file_name())
                    .map(|name| name.to_string());
                (target, folder)
            })
            .collect();
    }

    /// A row's frame: the selection fill with its accent edge, or a clear
    /// edge of the same width so rows stay aligned (V43 §8.3).
    fn row_frame(&self, row: TaskSelection, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors();
        let selected = self.selection() == Some(row);
        let background = self.row_background(row, cx);
        h_flex()
            .w_full()
            .group("backlog-row")
            .gap_2()
            .pl(px(10.))
            .pr_2()
            .py_1()
            .border_l_2()
            .border_color(if selected {
                colors.text_accent
            } else {
                gpui::transparent_black()
            })
            .when_some(background, |row, background| row.bg(background))
    }

    fn render_open_task_row(
        &self,
        section: SectionKind,
        index: usize,
        task: &BacklogTask,
        cx: &Context<Self>,
    ) -> AnyElement {
        if self.is_editing(section, task.line, &task.text) {
            if let Some(editor_row) = self.render_editor_row(cx) {
                return editor_row;
            }
        }
        let colors = cx.theme().colors();
        let section_key = section.id();
        let task_text = task.text.clone();
        let today_task_text = task.text.clone();
        let task_line = task.line;
        let meta = self.task_meta(task, cx);
        // Chevrons, matching the `>` / `<` keys that do the same thing.
        let (move_icon, move_tooltip, move_action) = match section {
            SectionKind::Someday => (
                IconName::ChevronLeft,
                "Move to Soon",
                MoveBacklogTaskLeft.boxed_clone(),
            ),
            _ => (
                IconName::ChevronRight,
                "Move to Someday",
                MoveBacklogTaskRight.boxed_clone(),
            ),
        };
        let tools = h_flex()
            .flex_none()
            .gap_0p5()
            .opacity(0.)
            .group_hover("backlog-row", |this| this.opacity(1.))
            .child(
                IconButton::new(
                    ElementId::Name(format!("backlog-move-{section_key}-{index}").into()),
                    move_icon,
                )
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .tooltip({
                    let focus_handle = self.focus_handle.clone();
                    move |_, cx| {
                        Tooltip::for_action_in(
                            move_tooltip,
                            move_action.as_ref(),
                            &focus_handle,
                            cx,
                        )
                    }
                })
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.select_row(section, index, cx);
                    this.move_selected_task(
                        match section {
                            SectionKind::Someday => SectionKind::Soon,
                            _ => SectionKind::Someday,
                        },
                        cx,
                    );
                })),
            )
            .child(
                IconButton::new(
                    ElementId::Name(format!("backlog-today-{section_key}-{index}").into()),
                    IconName::ListTodo,
                )
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .disabled(self.mark_in_flight)
                .tooltip({
                    let focus_handle = self.focus_handle.clone();
                    move |_, cx| {
                        Tooltip::for_action_in(
                            "Move to today",
                            &MoveBacklogTaskToToday,
                            &focus_handle,
                            cx,
                        )
                    }
                })
                .on_click(cx.listener({
                    let task_text = today_task_text;
                    move |this, _, window, cx| {
                        this.select_row(section, index, cx);
                        this.send_to_today(
                            section,
                            task_line,
                            task_text.clone(),
                            TodayWrite::Move,
                            window,
                            cx,
                        );
                    }
                })),
            )
            .child(
                IconButton::new(
                    ElementId::Name(format!("backlog-reveal-{section_key}-{index}").into()),
                    IconName::Notepad,
                )
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .tooltip({
                    let focus_handle = self.focus_handle.clone();
                    move |_, cx| {
                        Tooltip::for_action_in(
                            "Reveal in backlog.md",
                            &RevealBacklogTask,
                            &focus_handle,
                            cx,
                        )
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_row(section, index, cx);
                    this.reveal_task(task_line, window, cx);
                })),
            );

        self.row_frame(TaskSelection { section, index }, cx)
            .items_start()
            .child(
                div().pt_0p5().child(
                    Checkbox::new(
                        ElementId::Name(format!("backlog-check-{section_key}-{index}").into()),
                        ToggleState::Unselected,
                    )
                    .disabled(self.mark_in_flight)
                    .tooltip({
                        let focus_handle = self.focus_handle.clone();
                        move |_, cx| {
                            Tooltip::for_action_in(
                                "Mark done",
                                &CompleteBacklogTask,
                                &focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click(cx.listener({
                        let task_text = task_text.clone();
                        move |this, _, window, cx| {
                            this.select_row(section, index, cx);
                            this.mark_done(section, task_line, task_text.clone(), window, cx);
                        }
                    })),
                ),
            )
            .child(
                v_flex()
                    .id(ElementId::Name(
                        format!("backlog-task-{section_key}-{index}").into(),
                    ))
                    .flex_1()
                    .min_w_0()
                    .cursor_text()
                    .tooltip(Tooltip::text(thock_sync_core::strip_reminder(&task.text)))
                    .child(
                        div()
                            .text_size(rems(0.875))
                            .text_color(colors.text)
                            .line_clamp(2)
                            .child(self.render_task_text(
                                ElementId::Name(
                                    format!("backlog-text-{section_key}-{index}").into(),
                                ),
                                &task.text,
                                cx,
                            )),
                    )
                    .when_some(meta, |this, (meta, overdue)| {
                        this.child(
                            div()
                                .text_size(rems(0.71875))
                                .text_color(if overdue {
                                    cx.theme().status().warning
                                } else {
                                    colors.text_placeholder
                                })
                                .child(SharedString::from(meta)),
                        )
                    })
                    .on_click(cx.listener({
                        move |this, _, window, cx| {
                            this.select_row(section, index, cx);
                            this.start_edit(
                                EditTarget::Existing {
                                    section,
                                    line: task_line,
                                    original_text: task_text.clone(),
                                },
                                &task_text,
                                window,
                                cx,
                            );
                        }
                    })),
            )
            .child(tools)
            .into_any_element()
    }

    /// A category caption (V43 §8.3): the group's name and open count in
    /// small caps, with its own `+` so a task can be added straight into the
    /// group (V17 §5.1). Click to open or close it.
    fn render_category_row(
        &self,
        section: SectionKind,
        index: usize,
        name: &str,
        count: usize,
        collapsed: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let section_key = section.id();
        let name = name.to_string();
        let caption = format!("{} · {count}", name.to_uppercase());
        self.row_frame(TaskSelection { section, index }, cx)
            .pt_2()
            .child(
                h_flex()
                    .id(ElementId::Name(
                        format!("backlog-group-{section_key}-{index}").into(),
                    ))
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .cursor_pointer()
                    .child(
                        div()
                            .text_size(rems(0.6875))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text_placeholder)
                            .child(SharedString::from(caption)),
                    )
                    .when(collapsed, |this| {
                        this.child(
                            Icon::new(IconName::ChevronRight)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                    })
                    .on_click(cx.listener({
                        let name = name.clone();
                        move |this, _, _window, cx| {
                            this.select_category_header(section, &name);
                            this.set_category_collapsed(!collapsed, cx);
                        }
                    })),
            )
            .child(
                IconButton::new(
                    ElementId::Name(format!("backlog-group-add-{section_key}-{index}").into()),
                    IconName::Plus,
                )
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .tooltip({
                    let focus_handle = self.focus_handle.clone();
                    let label = SharedString::from(format!("Add to {name}"));
                    move |_, cx| {
                        Tooltip::for_action_in(label.clone(), &AddBacklogTask, &focus_handle, cx)
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_category_header(section, &name);
                    this.start_add(section, Some(name.clone()), window, cx);
                })),
            )
            .into_any_element()
    }

    /// A lane's title row (V43 §8.3): the heading in the display face, its
    /// open count, and a chevron while it is folded. Click to fold or unfold.
    fn render_lane_row(
        &self,
        section: SectionKind,
        count: usize,
        folded: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let section_key = section.id();
        let heading = section.heading(&self.headings()).to_string();
        self.row_frame(TaskSelection { section, index: 0 }, cx)
            .pt_3()
            .child(
                h_flex()
                    .id(ElementId::Name(
                        format!("backlog-lane-{section_key}").into(),
                    ))
                    .flex_1()
                    .min_w_0()
                    .justify_between()
                    .cursor_pointer()
                    .child(
                        div()
                            .font_family("Petrona")
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(rems(1.0625))
                            .text_color(colors.text)
                            .child(SharedString::from(heading)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(rems(0.71875))
                                    .text_color(colors.text_placeholder)
                                    .child(SharedString::from(count.to_string())),
                            )
                            .when(folded, |this| {
                                this.child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.select_row(section, 0, cx);
                        this.set_lane_folded(section, !folded, cx);
                    })),
            )
            .child(
                IconButton::new(
                    ElementId::Name(format!("backlog-add-{section_key}").into()),
                    IconName::Plus,
                )
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .tooltip({
                    let focus_handle = self.focus_handle.clone();
                    let label = match section {
                        SectionKind::Soon => "Add to Soon",
                        _ => "Add to Someday",
                    };
                    move |_, cx| Tooltip::for_action_in(label, &AddBacklogTask, &focus_handle, cx)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_add(section, None, window, cx);
                })),
            )
            .into_any_element()
    }

    /// The Backlog tab: the open lanes stacked in one scrolling list, so the
    /// cursor's flat index and `scroll_to_item` agree.
    fn render_open_view(&self, cx: &Context<Self>) -> AnyElement {
        let mut list = v_flex().py_0p5();
        for section in BacklogView::Open.sections() {
            let section = *section;
            let rows = self.visible_rows(section);
            // The new-task editor belongs at the end of the group it is
            // adding to, so it is flushed as each next header starts a new one.
            let mut group: Option<&str> = None;
            for (index, row) in rows.iter().enumerate() {
                match row {
                    BacklogRow::Lane { count, folded, .. } => {
                        list = list.child(self.render_lane_row(section, *count, *folded, cx));
                    }
                    BacklogRow::Category {
                        name,
                        count,
                        collapsed,
                    } => {
                        if self.is_adding_to(section, group)
                            && let Some(editor_row) = self.render_editor_row(cx)
                        {
                            list = list.child(editor_row);
                        }
                        group = Some(name);
                        list = list.child(
                            self.render_category_row(section, index, name, *count, *collapsed, cx),
                        );
                    }
                    BacklogRow::Task { task } => {
                        list = list.child(self.render_open_task_row(section, index, task, cx));
                    }
                }
            }
            if self.is_adding_to(section, group)
                && let Some(editor_row) = self.render_editor_row(cx)
            {
                list = list.child(editor_row);
            }
        }
        list.id("backlog-open-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handles[BacklogView::Open.index()])
            .into_any_element()
    }

    /// The Done tab: completed tasks newest first, each with its date.
    fn render_done_view(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let completed = self.completed_tasks();
        let mut reminders = self.done_reminders(cx).into_iter().peekable();
        let mut list = v_flex().py_1();
        if completed.is_empty() && reminders.peek().is_none() {
            list = list.child(self.render_hint("Nothing finished yet."));
        }
        for (index, task) in completed.into_iter().enumerate() {
            let task_date = backlog::completion_date(&task.text);
            // Both lists run newest first; a reminder closed on a later day
            // than this task goes above it.
            while let Some(reminder) = reminders
                .next_if(|reminder| task_date.is_none_or(|date| done_date(reminder) > Some(date)))
            {
                list = list.child(self.render_done_reminder(reminder, cx));
            }
            let (label, date) = split_completion(&task.text);
            let row = TaskSelection {
                section: SectionKind::Completed,
                index,
            };
            list = list.child(
                self.row_frame(row, cx)
                    .items_start()
                    .child(
                        div().pt_0p5().child(
                            Icon::new(IconName::TodoComplete)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(rems(0.875))
                                    .text_color(colors.text_muted)
                                    .line_clamp(2)
                                    .child(LabelLike::new().strikethrough().child(
                                        self.render_task_text(
                                            ElementId::Name(
                                                format!("backlog-text-completed-{index}").into(),
                                            ),
                                            label,
                                            cx,
                                        ),
                                    )),
                            )
                            .when_some(date, |this, date| {
                                this.child(
                                    div()
                                        .text_size(rems(0.71875))
                                        .text_color(colors.text_placeholder)
                                        .child(SharedString::from(date.to_string())),
                                )
                            }),
                    ),
            );
        }
        for reminder in reminders {
            list = list.child(self.render_done_reminder(reminder, cx));
        }
        list.id("backlog-completed-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handles[BacklogView::Done.index()])
            .into_any_element()
    }

    /// Closed and dropped reminders from the index's `## Done`, newest first.
    pub(crate) fn done_reminders<'a>(&self, cx: &'a App) -> Vec<&'a IndexedReminder> {
        let Some(service) = &self.reminder_service else {
            return Vec::new();
        };
        let mut reminders: Vec<&IndexedReminder> = service
            .read(cx)
            .reminders()
            .iter()
            .filter(|reminder| {
                reminder.section == IndexSection::Done
                    && matches!(reminder.state, IndexedState::Closed | IndexedState::Dropped)
            })
            .collect();
        reminders.sort_by_key(|reminder| Reverse(done_date(reminder)));
        reminders
    }

    /// A finished reminder among the finished tasks: its words struck, over
    /// the day it was closed or dropped. It is read-only here; the line
    /// lives in its home note.
    fn render_done_reminder(&self, reminder: &IndexedReminder, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let text = reminder.text.clone().unwrap_or_default();
        let dropped = reminder.state == IndexedState::Dropped
            || reminder
                .stamp
                .is_some_and(|stamp| stamp.kind == StampKind::Dropped);
        let meta = match (done_date(reminder), dropped) {
            (Some(date), true) => format!("{date} · dropped"),
            (Some(date), false) => date.to_string(),
            (None, true) => "dropped".to_string(),
            (None, false) => String::new(),
        };
        h_flex()
            .w_full()
            .gap_2()
            .pl(px(12.))
            .pr_2()
            .py_1()
            .items_start()
            .child(
                div().pt_0p5().child(
                    Icon::new(IconName::Bell)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(rems(0.875))
                            .text_color(colors.text_muted)
                            .line_clamp(2)
                            .child(
                                LabelLike::new()
                                    .strikethrough()
                                    .child(SharedString::from(text)),
                            ),
                    )
                    .when(!meta.is_empty(), |this| {
                        this.child(
                            div()
                                .text_size(rems(0.71875))
                                .text_color(colors.text_placeholder)
                                .child(SharedString::from(meta)),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The connector rows kept at the top of the view (V32 §4.2): Gmail,
    /// Inbox, Readwise, and the phone, each only while it needs the user.
    /// Everything else lives in the sync icon's popover.
    fn render_status_rows(&self, cx: &App) -> Vec<AnyElement> {
        let gmail = self.gmail_service.as_ref().map(|service| service.read(cx));
        sync_status::inline_statuses([
            gmail.and_then(sync_status::gmail_status),
            self.inbox_service
                .as_ref()
                .and_then(|service| sync_status::inbox_status(service.read(cx), gmail)),
            self.readwise_service
                .as_ref()
                .and_then(|service| sync_status::readwise_status(service.read(cx))),
            self.vault_sync_service
                .as_ref()
                .and_then(|service| sync_status::phone_status(service.read(cx))),
        ])
        .iter()
        .map(|status| sync_status::render_inline_row(status, cx))
        .collect()
    }

    fn render_body(&self, cx: &Context<Self>) -> AnyElement {
        match &self.vault_status {
            VaultStatus::NotAVault => self
                .render_hint("Open a Thock vault to use the backlog.")
                .into_any_element(),
            VaultStatus::Invalid { .. } => self
                .render_hint("This vault's config couldn't be read, so the backlog is unavailable.")
                .into_any_element(),
            VaultStatus::Valid(_) => {
                let mut body = v_flex().size_full();
                if self.buffer.is_none() {
                    body = body.child(
                        self.render_hint(
                            "No backlog.md yet — it will be created the first time a task \
                             is added.",
                        )
                        .border_b_1()
                        .border_color(cx.theme().colors().border_variant),
                    );
                } else if self.view == BacklogView::Open && self.backlog.is_empty() {
                    body = body.child(
                        self.render_hint(
                            "Nothing in the backlog. Wrap skills can move unfinished tasks here.",
                        )
                        .border_b_1()
                        .border_color(cx.theme().colors().border_variant),
                    );
                }
                body.child(match self.view {
                    BacklogView::Open => self.render_open_view(cx),
                    BacklogView::Done => self.render_done_view(cx),
                })
                .into_any_element()
            }
        }
    }
}

/// The note a task links to, when it links one: the `[[target]]` and the
/// date in its name, for the row's meta line.
struct TaskLink {
    target: String,
    date: Option<NaiveDate>,
}

fn task_link(text: &str) -> Option<TaskLink> {
    let target = parse_inline_spans(text)
        .into_iter()
        .find_map(|span| match span.target {
            Some(LinkTarget::Note(target)) => Some(target),
            _ => None,
        })?;
    let name = target.rsplit('/').next().unwrap_or(&target);
    let date = name
        .get(..10)
        .and_then(|head| NaiveDate::parse_from_str(head, "%Y-%m-%d").ok());
    Some(TaskLink { target, date })
}

/// The completed audit trail in render order. The file stays append-ordered
/// (oldest first); reversing before the sort puts same-day completions
/// newest-first too, and the sort is stable so that survives.
/// The day a finished reminder was closed or dropped, from its stamp.
fn done_date(reminder: &IndexedReminder) -> Option<NaiveDate> {
    reminder.stamp.map(|stamp| stamp.date)
}

fn completed_tasks(backlog: &Backlog) -> Vec<&BacklogTask> {
    let mut tasks: Vec<&BacklogTask> = backlog.completed.iter().rev().collect();
    tasks.sort_by_key(|task| Reverse(backlog::completion_date(&task.text)));
    tasks
}

/// The rows a lane renders, in render order: an open lane starts with its
/// title, then — unless the lane is folded — its uncategorized open tasks in
/// file order and one header per category followed by that group's tasks
/// unless it is closed. Completed is flat.
fn column_rows<'a>(
    backlog: &'a Backlog,
    section: SectionKind,
    collapsed: &HashSet<(SectionKind, String)>,
    folded_lanes: &HashSet<SectionKind>,
) -> Vec<BacklogRow<'a>> {
    if section == SectionKind::Completed {
        return completed_tasks(backlog)
            .into_iter()
            .map(|task| BacklogRow::Task { task })
            .collect();
    }
    let open: Vec<&BacklogTask> = backlog
        .section(section)
        .iter()
        .filter(|task| !task.checked)
        .collect();
    let folded = folded_lanes.contains(&section);
    let mut rows = vec![BacklogRow::Lane {
        count: open.len(),
        folded,
    }];
    if folded {
        return rows;
    }
    rows.extend(
        open.iter()
            .copied()
            .filter(|task| task.category.is_none())
            .map(|task| BacklogRow::Task { task }),
    );
    for category in backlog.categories(section) {
        let name = category.name.as_str();
        let members: Vec<&BacklogTask> = open
            .iter()
            .copied()
            .filter(|task| task.category.as_deref() == Some(name))
            .collect();
        let is_collapsed = collapsed.contains(&(section, name.to_string()));
        rows.push(BacklogRow::Category {
            name,
            count: members.len(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            rows.extend(members.into_iter().map(|task| BacklogRow::Task { task }));
        }
    }
    rows
}

/// Re-attaches the trailing HTML comment `strip_trailing_metadata` hid from
/// the inline editor, so renaming a captured task keeps its identity marker.
fn restore_hidden_suffix(original: &str, edited: &str) -> String {
    let visible = strip_trailing_metadata(original);
    let hidden = original[visible.len()..].trim();
    if hidden.is_empty() {
        edited.to_string()
    } else {
        format!("{edited} {hidden}")
    }
}

impl Render for BacklogPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("ThockBacklogPanel");
        // The inline editor is a descendant of this element, so the view's
        // single-key bindings would shadow typing into it; `editing` is what
        // the keymap gates them on.
        if self.edit_state.is_some() {
            key_context.add("editing");
        } else {
            key_context.add("menu");
        }
        v_flex()
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::select_next_column))
            .on_action(cx.listener(Self::select_previous_column))
            .on_action(cx.listener(Self::edit_selected_task))
            .on_action(cx.listener(Self::add_task))
            .on_action(cx.listener(Self::complete_selected_task))
            .on_action(cx.listener(Self::move_selected_task_right))
            .on_action(cx.listener(Self::move_selected_task_left))
            .on_action(cx.listener(Self::move_selected_task_to_today))
            .on_action(cx.listener(Self::copy_selected_task))
            .on_action(cx.listener(Self::reveal_selected_task))
            .on_action(cx.listener(Self::collapse_category))
            .on_action(cx.listener(Self::expand_category))
            .size_full()
            .children(self.render_status_rows(cx))
            .child(self.render_body(cx))
    }
}

impl Focusable for BacklogPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{BacklogRow, SectionKind, column_rows, restore_hidden_suffix, task_link};
    use crate::backlog::{BacklogHeadings, parse_backlog};
    use chrono::NaiveDate;
    use std::collections::HashSet;

    const CATEGORIZED: &str = "\
## Soon

- [ ] Loose task
- [x] Checked by hand

### OpenCue

- [ ] Fix the scheduler
- [ ] Review PR #2489

### Empty group

## Someday
";

    /// The rendered rows as `("category", count)` / `"task text"` labels.
    fn labels(source: &str, collapsed: &[(SectionKind, &str)]) -> Vec<String> {
        let backlog = parse_backlog(source, &BacklogHeadings::default());
        let collapsed: HashSet<(SectionKind, String)> = collapsed
            .iter()
            .map(|(section, name)| (*section, (*name).to_string()))
            .collect();
        column_rows(&backlog, SectionKind::Soon, &collapsed, &HashSet::default())
            .into_iter()
            .map(|row| match row {
                BacklogRow::Lane { count, folded, .. } => {
                    format!("## Soon ({count}{})", if folded { ", folded" } else { "" })
                }
                BacklogRow::Category { name, count, .. } => format!("# {name} ({count})"),
                BacklogRow::Task { task } => task.text.clone(),
            })
            .collect()
    }

    #[test]
    fn loose_tasks_render_above_the_groups() {
        assert_eq!(
            labels(CATEGORIZED, &[]),
            vec![
                "## Soon (3)",
                "Loose task",
                "# OpenCue (2)",
                "Fix the scheduler",
                "Review PR #2489",
                "# Empty group (0)",
            ]
        );
    }

    #[test]
    fn a_collapsed_group_keeps_its_header_and_count() {
        assert_eq!(
            labels(CATEGORIZED, &[(SectionKind::Soon, "OpenCue")]),
            vec![
                "## Soon (3)",
                "Loose task",
                "# OpenCue (2)",
                "# Empty group (0)"
            ]
        );
    }

    #[test]
    fn a_stale_collapse_entry_is_harmless() {
        assert_eq!(labels(CATEGORIZED, &[(SectionKind::Soon, "Gone")]).len(), 6);
    }

    #[test]
    fn a_backlog_without_categories_renders_as_a_flat_list() {
        assert_eq!(
            labels("## Soon\n- [ ] One\n- [ ] Two\n", &[]),
            vec!["## Soon (2)", "One", "Two"]
        );
    }

    #[test]
    fn a_folded_lane_is_one_row() {
        let backlog = parse_backlog(CATEGORIZED, &BacklogHeadings::default());
        let rows = column_rows(
            &backlog,
            SectionKind::Soon,
            &HashSet::default(),
            &HashSet::from_iter([SectionKind::Soon]),
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            BacklogRow::Lane {
                count: 3,
                folded: true,
                ..
            }
        ));
    }

    #[test]
    fn a_task_link_carries_its_date() {
        let link = task_link("Study for DNV elections [[2026-09-28-dnv-ballot]]").unwrap();
        assert_eq!(link.target, "2026-09-28-dnv-ballot");
        assert_eq!(link.date, NaiveDate::from_ymd_opt(2026, 9, 28));
        let nested = task_link("Reply [[inbox/2026-10-02 Jimmy]]").unwrap();
        assert_eq!(nested.date, NaiveDate::from_ymd_opt(2026, 10, 2));
        assert!(task_link("Apps to try: vorssaint").is_none());
        assert!(task_link("See [docs](https://a.example)").is_none());
        assert_eq!(task_link("Read [[Maestro]]").unwrap().date, None);
    }

    #[test]
    fn renames_keep_the_hidden_trailing_comment() {
        assert_eq!(
            restore_hidden_suffix("Pay invoice <!--gmail:9f2c1ab4e7d0-->", "Pay invoice today"),
            "Pay invoice today <!--gmail:9f2c1ab4e7d0-->"
        );
        assert_eq!(
            restore_hidden_suffix("Plain task", "Renamed task"),
            "Renamed task"
        );
        // A mid-line comment is visible content, not identity — untouched.
        assert_eq!(
            restore_hidden_suffix("a <!-- note --> b", "a <!-- note --> c"),
            "a <!-- note --> c"
        );
    }
}
