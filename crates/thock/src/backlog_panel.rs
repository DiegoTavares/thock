//! The Backlog panel (spec `v6-backlog.md`): a bottom-dock checklist over the
//! vault's `backlog.md`, grouped by Soon and Someday alongside the Completed
//! history. Task text is editable inline; checking a task off
//! records it as done in today's daily note and files it under Completed with
//! the date. The file stays the single source of truth: edits go through the
//! open buffer (so an editor tab and the panel never fight) and the panel
//! re-parses on every buffer change.

use anyhow::{Context as _, Result};
use chrono::Local;
use editor::{Editor, EditorEvent, SelectionEffects, scroll::Autoscroll};
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, ClipboardItem, Context, Entity, EventEmitter,
    FocusHandle, Focusable, HighlightStyle, Hsla, InteractiveText, KeyContext, Pixels, StyledText,
    Subscription, Task, UnderlineStyle, WeakEntity, Window, actions, div, px,
};
use language::{Buffer, BufferEvent};
use menu::{Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::Project;
use std::cmp::Reverse;
use std::path::PathBuf;
use std::time::Duration;
use text::{Bias, Point};
use ui::prelude::*;
use ui::{Checkbox, Icon, IconButton, IconSize, Label, LabelLike, ToggleState, Tooltip};
use util::ResultExt as _;
use workspace::dock::{DockPosition, Panel, PanelEvent};
use workspace::{OpenOptions, OpenVisible, Workspace};

use crate::backlog::{self, Backlog, BacklogTask, SectionKind, parse_backlog, split_completion};
use crate::calendar_service::{ConnectGoogleWorkspace, SyncState};
use crate::day_plan::strip_trailing_comment;
use crate::gmail_service::{self, GmailService, SyncGmailNow};
use crate::inbox_service::{self, InboxService, OpenInbox, SyncInboxNow};
use crate::markdown_text::{InlineSpan, parse_inline_links};
use crate::notes::{EnsureNoteOutcome, NoteKind, ensure_note};
use crate::vault::{Vault, VaultStatus};

const BACKLOG_PANEL_KEY: &str = "ThockBacklogPanel";
const REPARSE_DEBOUNCE: Duration = Duration::from_millis(150);
/// How long a copied row stays lit — vim's `highlight_on_yank_duration`
/// default, so a yank in the panel feels like a yank in the editor.
const COPY_FLASH_DURATION: Duration = Duration::from_millis(200);

actions!(
    thock,
    [
        /// Toggles focus on the Thock backlog panel.
        ToggleBacklogFocus,
        /// Selects the backlog column to the right (Soon → Someday → Completed).
        SelectNextBacklogColumn,
        /// Selects the backlog column to the left (Completed → Someday → Soon).
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
        /// Copies the selected task to the clipboard as Markdown.
        CopyBacklogTask,
        /// Opens backlog.md at the selected task's line.
        RevealBacklogTask
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleBacklogFocus, window, cx| {
            workspace.toggle_panel_focus::<BacklogPanel>(window, cx);
        });
    })
    .detach();
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
    /// The `+` affordance: a brand-new task appended to the section.
    New { section: SectionKind },
}

struct EditState {
    target: EditTarget,
    editor: Entity<Editor>,
    _subscription: Subscription,
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
    position: DockPosition,
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
    selected: Option<TaskSelection>,
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
    _subscriptions: Vec<Subscription>,
}

impl BacklogPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            BacklogPanel::new(workspace, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
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
            let mut subscriptions = vec![project_subscription];
            if let Some(service) = &gmail_service {
                subscriptions.push(cx.observe(service, |_, _, cx| cx.notify()));
            }
            if let Some(service) = &inbox_service {
                subscriptions.push(cx.observe(service, |_, _, cx| cx.notify()));
            }
            let mut this = Self {
                workspace: weak_workspace,
                project,
                focus_handle: cx.focus_handle(),
                position: DockPosition::Bottom,
                vault_status: VaultStatus::NotAVault,
                buffer: None,
                buffer_path: None,
                _buffer_subscription: None,
                backlog: Backlog::default(),
                selected: None,
                copy_flash: None,
                copy_flash_task: None,
                edit_state: None,
                mark_in_flight: false,
                load_buffer_task: None,
                reparse_task: None,
                gmail_service,
                inbox_service,
                _subscriptions: subscriptions,
            };
            this.vault_status = this.detect_vault_status(cx);
            this.ensure_buffer(cx);
            this
        })
    }

    fn vault(&self) -> Option<&Vault> {
        match &self.vault_status {
            VaultStatus::Valid(vault) => Some(vault),
            _ => None,
        }
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
        let status = self.detect_vault_status(cx);
        if status != self.vault_status {
            self.vault_status = status;
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
            Some(buffer) => parse_backlog(&buffer.read(cx).text()),
            None => Backlog::default(),
        };
        if backlog != self.backlog {
            self.backlog = backlog;
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

    /// The tasks a column renders, in render order. Soon and Someday show only
    /// open tasks, in file order; Completed shows its whole audit trail newest
    /// first. Both the renderer and the keyboard cursor index into this, so
    /// there is one ordering to keep in step.
    fn visible_tasks(&self, section: SectionKind) -> Vec<&BacklogTask> {
        match section {
            SectionKind::Completed => {
                // The file stays append-ordered (oldest first); reversing before
                // the sort puts same-day completions newest-first too, and the
                // sort is stable so that survives.
                let mut tasks: Vec<&BacklogTask> = self.backlog.completed.iter().rev().collect();
                tasks.sort_by_key(|task| Reverse(backlog::completion_date(&task.text)));
                tasks
            }
            _ => self
                .backlog
                .section(section)
                .iter()
                .filter(|task| !task.checked)
                .collect(),
        }
    }

    /// The selection resolved against the current parse: same column, index
    /// clamped to what that column renders now. `None` once the column is
    /// empty, so the highlight never points at a row that isn't there.
    fn selection(&self) -> Option<TaskSelection> {
        let selected = self.selected?;
        let len = self.visible_tasks(selected.section).len();
        len.checked_sub(1).map(|last| TaskSelection {
            section: selected.section,
            index: selected.index.min(last),
        })
    }

    fn selected_task(&self) -> Option<(SectionKind, BacklogTask)> {
        let selection = self.selection()?;
        let task = self
            .visible_tasks(selection.section)
            .get(selection.index)
            .copied()?;
        Some((selection.section, task.clone()))
    }

    /// Where the cursor lands when a motion arrives with nothing selected: the
    /// first column that has anything in it.
    fn first_populated_column(&self) -> Option<SectionKind> {
        SectionKind::ALL
            .into_iter()
            .find(|section| !self.visible_tasks(*section).is_empty())
    }

    fn select_row(&mut self, section: SectionKind, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(TaskSelection { section, index });
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        match self.selection() {
            Some(selection) => {
                let last = self
                    .visible_tasks(selection.section)
                    .len()
                    .saturating_sub(1);
                self.select_row(selection.section, (selection.index + 1).min(last), cx);
            }
            None => {
                if let Some(section) = self.first_populated_column() {
                    self.select_row(section, 0, cx);
                }
            }
        }
    }

    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selection() {
            Some(selection) => {
                self.select_row(selection.section, selection.index.saturating_sub(1), cx);
            }
            None => {
                if let Some(section) = self.first_populated_column() {
                    let last = self.visible_tasks(section).len().saturating_sub(1);
                    self.select_row(section, last, cx);
                }
            }
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _window: &mut Window, cx: &mut Context<Self>) {
        let section = self
            .selection()
            .map(|selection| selection.section)
            .or_else(|| self.first_populated_column());
        if let Some(section) = section
            && !self.visible_tasks(section).is_empty()
        {
            self.select_row(section, 0, cx);
        }
    }

    fn select_last(&mut self, _: &SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        let section = self
            .selection()
            .map(|selection| selection.section)
            .or_else(|| self.first_populated_column());
        if let Some(section) = section {
            let len = self.visible_tasks(section).len();
            if let Some(last) = len.checked_sub(1) {
                self.select_row(section, last, cx);
            }
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

    /// Moves the cursor to the nearest column in `forward`'s direction that has
    /// rows, keeping the row index (clamped). Empty columns are skipped rather
    /// than swallowing the selection.
    fn select_column(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(current) = self.selection() else {
            if let Some(section) = self.first_populated_column() {
                self.select_row(section, 0, cx);
            }
            return;
        };
        let Some(position) = SectionKind::ALL
            .iter()
            .position(|section| *section == current.section)
        else {
            return;
        };
        let candidates: Vec<SectionKind> = if forward {
            SectionKind::ALL[position + 1..].to_vec()
        } else {
            SectionKind::ALL[..position].iter().rev().copied().collect()
        };
        let Some((destination, len)) = candidates
            .into_iter()
            .map(|section| (section, self.visible_tasks(section).len()))
            .find(|(_, len)| *len > 0)
        else {
            return;
        };
        self.select_row(destination, current.index.min(len - 1), cx);
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
        self.start_edit(EditTarget::New { section }, "", window, cx);
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
        if !self.move_task(section, task.line, task.text, cx) {
            return;
        }
        // `move_task_edits` appends to the destination, so the moved task is
        // now its last row.
        let len = self.visible_tasks(destination).len();
        if let Some(last) = len.checked_sub(1) {
            self.select_row(destination, last, cx);
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
                let located = parse_backlog(&text)
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
            editor.set_text(strip_trailing_comment(initial_text), window, cx);
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
        self.edit_state = Some(EditState {
            target,
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_state.is_some() {
            self.commit_edit(window, cx);
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
                if new_text.is_empty() || new_text == strip_trailing_comment(&original_text) {
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
                let backlog = parse_backlog(&text);
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
            EditTarget::New { section } => {
                if new_text.is_empty() {
                    return;
                }
                self.append_new_task(section, &new_text, cx);
            }
        }
    }

    fn append_new_task(&mut self, section: SectionKind, text: &str, cx: &mut Context<Self>) {
        let block = backlog::new_task_block(text);
        if let Some(buffer) = self.buffer.clone() {
            let current = buffer.read(cx).text();
            let edit = backlog::append_to_section_edit(&current, section, &block);
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
            let edit = backlog::append_to_section_edit(&current, section, &block);
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

    // --- Mark done (spec §6.3) ---

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
        if self.mark_in_flight || self.buffer.is_none() {
            return;
        }
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        self.mark_in_flight = true;
        cx.notify();

        let project = self.project.clone();
        let heading = vault.config.day_planner.heading.clone();
        let now = Local::now();
        let today = now.date_naive();
        let time = now.time();
        let note_line_text = task_text.clone();
        let ensure =
            cx.background_spawn(async move { ensure_note(&vault, NoteKind::Daily, today, time) });
        cx.spawn_in(window, async move |this, cx| {
            let note_result = async {
                let (note_path, outcome) = ensure.await?;
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
                    let edit = backlog::append_done_to_note_edit(
                        &buffer.text(),
                        &heading,
                        &note_line_text,
                    );
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
                    this.show_error(
                        format!("Couldn't record the task in today's note: {error}"),
                        cx,
                    );
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
                let backlog = parse_backlog(&text);
                let task = backlog
                    .locate_task(section, line, &task_text)
                    .context("the task changed while it was being completed")?;
                let mut edits = backlog::complete_task_edits(&text, task, today);
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
                    // Re-checking is safe: the panel re-renders from the file,
                    // which still shows the task open (spec §6.5).
                    this.show_error(
                        format!(
                            "The task was recorded in today's note, but the backlog couldn't \
                             be updated: {error}"
                        ),
                        cx,
                    );
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
        let backlog = parse_backlog(&text);
        let Some(task) = backlog.locate_task(section, line, &task_text) else {
            self.show_error(
                "That task changed outside the panel, so it wasn't moved.".to_string(),
                cx,
            );
            return false;
        };
        let edits = backlog::move_task_edits(&text, task, destination);
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

    fn is_adding_to(&self, section: SectionKind) -> bool {
        self.edit_state.as_ref().is_some_and(|state| {
            matches!(&state.target, EditTarget::New { section: adding } if *adding == section)
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

    /// A task's text with `[name](url)` rendered as a clickable link. Returns
    /// the text element only — the caller wraps it in the `LabelLike` that
    /// carries the row's size, color and truncation.
    fn render_task_text(&self, id: ElementId, text: &str, cx: &Context<Self>) -> AnyElement {
        // A trailing `<!-- … -->` (e.g. a capture marker) is identity, not
        // content — hidden here exactly as the Day Planner hides it (v8
        // §11.4); the file keeps it.
        let text = strip_trailing_comment(text);
        let spans = parse_inline_links(text);
        if !spans
            .iter()
            .any(|span| matches!(span, InlineSpan::Link { .. }))
        {
            return SharedString::from(text.to_string()).into_any_element();
        }
        let mut display = String::new();
        let mut link_ranges = Vec::new();
        let mut urls = Vec::new();
        for span in spans {
            match span {
                InlineSpan::Text(literal) => display.push_str(&literal),
                InlineSpan::Link { text, url } => {
                    let start = display.len();
                    display.push_str(&text);
                    link_ranges.push(start..display.len());
                    urls.push(url);
                }
            }
        }
        let link_style = HighlightStyle {
            color: Some(cx.theme().colors().text_accent),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            }),
            ..Default::default()
        };
        let highlights: Vec<_> = link_ranges
            .iter()
            .map(|range| (range.clone(), link_style))
            .collect();
        InteractiveText::new(id, StyledText::new(display).with_highlights(highlights))
            .on_click(link_ranges, move |index, _window, cx| {
                if let Some(url) = urls.get(index) {
                    cx.open_url(url);
                }
                // Without this the row's click-to-edit fires too, and the
                // inline editor opens over the link the user just followed.
                cx.stop_propagation();
            })
            .into_any_element()
    }

    /// A row's background: the copy flash outranks the selection highlight, so
    /// the pulse is visible even on the row the cursor is already sitting on.
    fn row_background(&self, row: TaskSelection, cx: &Context<Self>) -> Option<Hsla> {
        if self.copy_flash == Some(row) {
            return Some(cx.theme().colors().text_accent.alpha(0.2));
        }
        (self.selection() == Some(row)).then(|| cx.theme().colors().element_selected)
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
        let section_key = section.heading();
        let task_text = task.text.clone();
        let task_line = task.line;
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
        h_flex()
            .w_full()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded_sm()
            .when_some(
                self.row_background(TaskSelection { section, index }, cx),
                |row, background| row.bg(background),
            )
            .child(
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
            )
            .child(
                div()
                    .id(ElementId::Name(
                        format!("backlog-task-{section_key}-{index}").into(),
                    ))
                    .flex_1()
                    .min_w_0()
                    .cursor_text()
                    .child(LabelLike::new().size(LabelSize::Small).truncate().child(
                        self.render_task_text(
                            ElementId::Name(format!("backlog-text-{section_key}-{index}").into()),
                            &task.text,
                            cx,
                        ),
                    ))
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
            )
            .into_any_element()
    }

    fn render_open_section(&self, section: SectionKind, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let tasks = self.visible_tasks(section);
        let section_key = section.heading();
        let header = h_flex()
            .justify_between()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(colors.border_variant)
            .child(
                h_flex()
                    .gap_1()
                    .child(Label::new(section.heading()).size(LabelSize::Small))
                    .child(
                        Label::new(tasks.len().to_string())
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
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
                    move |_, cx| {
                        Tooltip::for_action_in(label, &AddBacklogTask, &focus_handle, cx)
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_edit(EditTarget::New { section }, "", window, cx);
                })),
            );

        let mut list = v_flex().py_0p5().gap_0p5();
        for (index, task) in tasks.into_iter().enumerate() {
            list = list.child(self.render_open_task_row(section, index, task, cx));
        }
        if self.is_adding_to(section)
            && let Some(editor_row) = self.render_editor_row(cx)
        {
            list = list.child(editor_row);
        }

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .border_r_1()
            .border_color(colors.border_variant)
            .child(header)
            .child(
                div()
                    .id(ElementId::Name(
                        format!("backlog-section-{section_key}").into(),
                    ))
                    .flex_1()
                    .overflow_y_scroll()
                    .child(list),
            )
            .into_any_element()
    }

    fn render_completed_section(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let completed = self.visible_tasks(SectionKind::Completed);
        let header = h_flex()
            .justify_between()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(colors.border_variant)
            .child(Label::new("Completed").size(LabelSize::Small));

        let mut list = v_flex().py_0p5().gap_0p5();
        for (index, task) in completed.into_iter().enumerate() {
            let (label, date) = split_completion(&task.text);
            let background = self.row_background(
                TaskSelection {
                    section: SectionKind::Completed,
                    index,
                },
                cx,
            );
            let mut row = h_flex()
                .w_full()
                .gap_1()
                .px_1()
                .py_0p5()
                .rounded_sm()
                .when_some(background, |row, background| row.bg(background))
                .child(
                    Icon::new(IconName::TodoComplete)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        LabelLike::new()
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .strikethrough()
                            .truncate()
                            .child(self.render_task_text(
                                ElementId::Name(format!("backlog-text-completed-{index}").into()),
                                label,
                                cx,
                            )),
                    ),
                );
            if let Some(date) = date {
                row = row.child(
                    Label::new(date.to_string())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                );
            }
            list = list.child(row);
        }

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .id("backlog-completed-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(list),
            )
            .into_any_element()
    }

    /// The email-capture status row (spec v9 §10.3), shown only when the
    /// vault has a Gmail config, so a vault without one looks exactly as it
    /// does today (G5). The actions it triggers are also in the command
    /// palette (`thock: connect google workspace`, `thock: sync gmail now`).
    fn render_status_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let service = self.gmail_service.as_ref()?.read(cx);
        if !service.has_config() {
            return None;
        }
        let muted = |text: String| {
            Label::new(text)
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element()
        };
        let connect_button = |id: &'static str, label: &'static str| {
            Button::new(id, label)
                .label_size(LabelSize::Small)
                .on_click(|_, window, cx| {
                    window.dispatch_action(ConnectGoogleWorkspace.boxed_clone(), cx);
                })
                .into_any_element()
        };
        let content = match service.state() {
            SyncState::NoConfig => return None,
            SyncState::NeverConnected => vec![connect_button(
                "thock-connect-google-workspace-backlog",
                "Connect Google Workspace",
            )],
            SyncState::Connecting => vec![muted("Gmail · connecting…".to_string())],
            SyncState::Idle => vec![muted("Gmail · waiting for first check".to_string())],
            // Running on V9's flat label: a visible transitional state with
            // a rename hint, never a silent alias (V13 §7.1).
            SyncState::Synced { .. } if service.using_legacy_label() => vec![
                div()
                    .id("thock-gmail-legacy-label")
                    .tooltip(Tooltip::text(
                        "Capture still works — rename the label to \"thock/backlog\" in Gmail \
                         to finish the move.",
                    ))
                    .child(muted("Backlog · using the old \"backlog\" label".to_string()))
                    .into_any_element(),
            ],
            SyncState::Synced { at } => {
                vec![muted(format!("Gmail · checked {}", format_ago(at.elapsed())))]
            }
            SyncState::Holding { reason } => vec![muted(format!("Gmail · {reason}"))],
            SyncState::Failing { error } => vec![
                muted("Gmail · sync failed".to_string()),
                Button::new("thock-retry-gmail-sync", "Retry")
                    .label_size(LabelSize::Small)
                    .tooltip(Tooltip::text(error.clone()))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(SyncGmailNow.boxed_clone(), cx);
                    })
                    .into_any_element(),
            ],
            SyncState::Disconnected => vec![
                muted("Gmail · sign-in needed".to_string()),
                connect_button("thock-reconnect-google-workspace", "Reconnect"),
            ],
        };
        Some(
            h_flex()
                .px_2()
                .py_1()
                .gap_2()
                .justify_between()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .children(content)
                .into_any_element(),
        )
    }

    /// The inbox-capture status row (V13 §10.4), shown only when the vault
    /// has `.thock/inbox.toml`. A healthy row with items waiting is the way
    /// into triage: activating it runs the Triage Inbox ritual, falling back
    /// to revealing the folder when the Inbox Routine was removed.
    fn render_inbox_status_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let service = self.inbox_service.as_ref()?.read(cx);
        if !service.has_config() {
            return None;
        }
        let muted = |text: String| {
            Label::new(text)
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element()
        };
        let vault_root = self.vault().map(|vault| vault.root.clone());
        let content = match service.state() {
            SyncState::NoConfig => return None,
            SyncState::NeverConnected => {
                // The Gmail row already offers the connect button when both
                // configs exist; avoid two identical buttons.
                if self
                    .gmail_service
                    .as_ref()
                    .is_some_and(|gmail| gmail.read(cx).has_config())
                {
                    return None;
                }
                vec![
                    Button::new("thock-connect-google-workspace-inbox", "Connect Google Workspace")
                        .label_size(LabelSize::Small)
                        .on_click(|_, window, cx| {
                            window.dispatch_action(
                                crate::calendar_service::ConnectGoogleWorkspace.boxed_clone(),
                                cx,
                            );
                        })
                        .into_any_element(),
                ]
            }
            SyncState::Connecting => vec![muted("Inbox · connecting…".to_string())],
            SyncState::Idle => vec![muted("Inbox · waiting for first check".to_string())],
            SyncState::Synced { .. } if service.queue_depth() == 0 => {
                vec![muted("Inbox · empty".to_string())]
            }
            SyncState::Synced { .. } => {
                let depth = service.queue_depth();
                vec![
                    Button::new(
                        "thock-triage-inbox",
                        format!("Inbox · {depth} waiting — triage"),
                    )
                    .label_size(LabelSize::Small)
                    .on_click(move |_, window, cx| {
                        dispatch_triage(vault_root.clone(), window, cx);
                    })
                    .into_any_element(),
                ]
            }
            SyncState::Holding { reason } => vec![muted(format!("Inbox · {reason}"))],
            SyncState::Failing { error } => vec![
                muted("Inbox · sync failed".to_string()),
                Button::new("thock-retry-inbox-sync", "Retry")
                    .label_size(LabelSize::Small)
                    .tooltip(Tooltip::text(error.clone()))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(SyncInboxNow.boxed_clone(), cx);
                    })
                    .into_any_element(),
            ],
            SyncState::Disconnected => vec![
                muted("Inbox · sign-in expired".to_string()),
                Button::new("thock-reconnect-google-workspace-inbox", "Reconnect")
                    .label_size(LabelSize::Small)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(
                            crate::calendar_service::ConnectGoogleWorkspace.boxed_clone(),
                            cx,
                        );
                    })
                    .into_any_element(),
            ],
        };
        Some(
            h_flex()
                .px_2()
                .py_1()
                .gap_2()
                .justify_between()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .children(content)
                .into_any_element(),
        )
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
                } else if self.backlog.is_empty() {
                    body = body.child(
                        self.render_hint(
                            "Nothing in the backlog. Wrap skills can move unfinished tasks here.",
                        )
                        .border_b_1()
                        .border_color(cx.theme().colors().border_variant),
                    );
                }
                body.child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .items_stretch()
                        .child(self.render_open_section(SectionKind::Soon, cx))
                        .child(self.render_open_section(SectionKind::Someday, cx))
                        .child(self.render_completed_section(cx)),
                )
                .into_any_element()
            }
        }
    }
}

/// Activating the Inbox row runs the Triage Inbox ritual (the generic
/// `thock::RunSkill`, per the repo's rule about dynamic content). A vault
/// whose Inbox Routine was removed must not get a dead row (V13 §10.4), so
/// with no `triage-inbox` skill registered this falls back to
/// `thock::OpenInbox`, which reveals the landing zone in the project panel.
fn dispatch_triage(vault_root: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let has_triage_skill = vault_root
        .and_then(|root| match crate::vault::Vault::detect(&root) {
            crate::vault::VaultStatus::Valid(vault) => Some(vault),
            _ => None,
        })
        .is_some_and(|vault| {
            crate::routines::enabled_routine_manifests(&vault)
                .iter()
                .flat_map(|manifest| &manifest.skills)
                .any(|skill| skill.id == "triage-inbox")
        });
    if has_triage_skill {
        window.dispatch_action(
            crate::agent_panel::RunSkill {
                skill: Some("triage-inbox".to_string()),
            }
            .boxed_clone(),
            cx,
        );
    } else {
        window.dispatch_action(OpenInbox.boxed_clone(), cx);
    }
}

/// Re-attaches the trailing HTML comment `strip_trailing_comment` hid from
/// the inline editor, so renaming a captured task keeps its identity marker.
fn restore_hidden_suffix(original: &str, edited: &str) -> String {
    let visible = strip_trailing_comment(original);
    let hidden = original[visible.len()..].trim();
    if hidden.is_empty() {
        edited.to_string()
    } else {
        format!("{edited} {hidden}")
    }
}

fn format_ago(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0 => "just now".to_string(),
        1..=59 => format!("{minutes}m ago"),
        _ => format!("{}h ago", minutes / 60),
    }
}

impl Render for BacklogPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("ThockBacklogPanel");
        // The inline editor is a descendant of this element, so the panel's
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
            .on_action(cx.listener(Self::copy_selected_task))
            .on_action(cx.listener(Self::reveal_selected_task))
            .size_full()
            .children(self.render_status_row(cx))
            .children(self.render_inbox_status_row(cx))
            .child(self.render_body(cx))
    }
}

impl EventEmitter<PanelEvent> for BacklogPanel {}

impl Focusable for BacklogPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for BacklogPanel {
    fn persistent_name() -> &'static str {
        "Thock Backlog Panel"
    }

    fn panel_key() -> &'static str {
        BACKLOG_PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Bottom)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(240.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Archive)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Backlog Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        ToggleBacklogFocus.boxed_clone()
    }

    fn activation_priority(&self) -> u32 {
        // Must be unique across all panels; 0-9 are taken (0-3 and 5-7
        // upstream, 4 Timeline, 8 Day Planner, 9 Agent).
        10
    }
}

#[cfg(test)]
mod tests {
    use super::restore_hidden_suffix;

    #[test]
    fn renames_keep_the_hidden_trailing_comment() {
        assert_eq!(
            restore_hidden_suffix("Pay invoice <!--gmail:9f2c1ab4e7d0-->", "Pay invoice today"),
            "Pay invoice today <!--gmail:9f2c1ab4e7d0-->"
        );
        assert_eq!(restore_hidden_suffix("Plain task", "Renamed task"), "Renamed task");
        // A mid-line comment is visible content, not identity — untouched.
        assert_eq!(
            restore_hidden_suffix("a <!-- note --> b", "a <!-- note --> c"),
            "a <!-- note --> c"
        );
    }
}
