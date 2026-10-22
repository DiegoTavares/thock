//! The Desk's Reminders tab (V45 §9): every open reminder of the vault,
//! grouped Overdue, Today and Upcoming and sorted by due, read from the
//! reminders service. Rows close, reschedule and drop a reminder on its home
//! line through the service's sync-core edits; the index follows.

use crate::reminder_calendar::pick_due;
use crate::reminder_index::{HomeEdit, IndexSection, IndexedReminder, IndexedState};
use crate::reminder_service::{self, ReminderEvent, ReminderService};
use crate::reminders::{ReminderClock, ReminderDay, ReminderSettings, day_label, due_label};
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use gpui::{
    Action, AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyContext, ScrollHandle,
    Subscription, Task, WeakEntity, Window, actions, div, px, rems,
};
use menu::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::Project;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use text::{Bias, Point};
use thock_sync_core::{Due, Group, note_link};
use ui::prelude::*;
use ui::{Icon, IconSize, Label, LabelSize};
use util::ResultExt as _;
use workspace::{OpenOptions, OpenVisible, Workspace};

actions!(
    thock,
    [
        /// Show your reminders.
        ToggleRemindersFocus,
        /// Mark this reminder done.
        CloseReminder,
        /// Move this reminder to another day or time.
        RescheduleReminder,
        /// Stop reminding me, keep the line.
        DropReminder,
        /// Add a new reminder to your reminders list.
        NewReminder
    ]
);

/// Picks a choice of the reschedule menu by its position, for its number
/// keys: `["thock::ChooseRescheduleOption", { "index": 0 }]`.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = thock)]
#[serde(deny_unknown_fields)]
pub struct ChooseRescheduleOption {
    pub index: usize,
}

const KEY_CONTEXT: &str = "ThockRemindersPanel";
const RESCHEDULE_OPTIONS: usize = 3;

/// Where a row is listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowGroup {
    Overdue,
    Today,
    Upcoming,
    /// An open reminder whose line isn't where the index says, yet.
    NotFound,
}

impl RowGroup {
    fn title(self) -> &'static str {
        match self {
            Self::Overdue => "Overdue",
            Self::Today => "Today",
            Self::Upcoming => "Upcoming",
            Self::NotFound => "Line not found",
        }
    }
}

/// The tab's rows on `today`: each open reminder with its group, the groups
/// in order and each sorted by when it rings. Closed, dropped and removed
/// reminders belong to Done.
pub fn group_rows(
    reminders: &[IndexedReminder],
    today: NaiveDate,
    date_only_at: NaiveTime,
) -> Vec<(RowGroup, &IndexedReminder)> {
    let mut rows: Vec<(RowGroup, Option<NaiveDateTime>, &IndexedReminder)> = reminders
        .iter()
        .filter_map(|reminder| {
            let group = match reminder.group(today) {
                Some(Group::Overdue) => RowGroup::Overdue,
                Some(Group::Today) => RowGroup::Today,
                Some(Group::Upcoming) => RowGroup::Upcoming,
                Some(Group::Done) => return None,
                None if reminder.state == IndexedState::Missing
                    && reminder.section == IndexSection::Open =>
                {
                    RowGroup::NotFound
                }
                None => return None,
            };
            Some((group, reminder.rings_at(date_only_at), reminder))
        })
        .collect();
    rows.sort_by_key(|(group, rings_at, _)| (*group, *rings_at));
    rows.into_iter()
        .map(|(group, _, reminder)| (group, reminder))
        .collect()
}

/// What a row says under its text: the due and the home note,
/// *Tue 20 Oct · 09:00 · backlog*.
pub fn row_meta(reminder: &IndexedReminder, today: NaiveDate) -> String {
    let mut parts = Vec::new();
    if let Some(due) = reminder.due {
        parts.push(day_label(due.date, today));
        if let Some(time) = due.time {
            parts.push(time.format("%H:%M").to_string());
        }
    }
    parts.push(note_link(&reminder.home).to_string());
    parts.join(" · ")
}

/// The reschedule menu's choices on `today`, dated: *Tomorrow 09:00*,
/// *Mon 26 Oct 09:00*, *Pick a date…*.
pub fn reschedule_labels(settings: &ReminderSettings, today: NaiveDate) -> [String; 3] {
    [
        due_label(settings.tomorrow_due(today), today),
        due_label(settings.monday_due(today), today),
        "Pick a date…".to_string(),
    ]
}

pub struct RemindersPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    service: Option<Entity<ReminderService>>,
    /// The selected reminder's id, kept across reloads.
    selected: Option<String>,
    /// Where the selection was, for when its reminder leaves the list.
    selected_position: usize,
    /// The reschedule menu's cursor while it is open.
    reschedule: Option<usize>,
    /// Whether the Desk keeps a row above this view (the Next card).
    row_above: bool,
    scroll_handle: ScrollHandle,
    _service_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl RemindersPanel {
    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = workspace.weak_handle();
        cx.new(|cx| {
            let project_subscription =
                cx.subscribe_in(&project, window, |this: &mut Self, _, event, window, cx| {
                    if matches!(event, project::Event::WorktreeAdded(_)) {
                        this.ensure_service(window, cx);
                    }
                });
            // Due labels say Today and Tomorrow, which move at midnight.
            let reminder_day = ReminderDay::global(cx);
            let day_subscription = cx.observe(&reminder_day, |_, _, cx| cx.notify());
            let mut this = Self {
                workspace: weak_workspace,
                project,
                focus_handle: cx.focus_handle(),
                service: None,
                selected: None,
                selected_position: 0,
                reschedule: None,
                row_above: false,
                scroll_handle: ScrollHandle::new(),
                _service_subscriptions: Vec::new(),
                _subscriptions: vec![project_subscription, day_subscription],
            };
            this.ensure_service(window, cx);
            this
        })
    }

    /// Finds the vault's reminders service, which appears when the vault's
    /// worktree does.
    fn ensure_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.is_some() {
            return;
        }
        let Some(service) = reminder_service::service_for_project(&self.project, cx) else {
            return;
        };
        self._service_subscriptions = vec![cx.subscribe_in(
            &service,
            window,
            |this, _, event: &ReminderEvent, window, cx| match event {
                ReminderEvent::IndexChanged
                | ReminderEvent::DayChanged { .. }
                | ReminderEvent::SettingsChanged => {
                    this.keep_selection(cx);
                    cx.notify();
                }
                ReminderEvent::ShowReminders { window: target } => {
                    if target.is_none_or(|target| target == window.window_handle()) {
                        this.show_on_desk(window, cx);
                    }
                }
                ReminderEvent::NoteCreated { .. } | ReminderEvent::NoteChanged { .. } => {}
            },
        )];
        service.update(cx, |service, _| {
            service.register_window(window.window_handle())
        });
        self.service = Some(service);
        cx.notify();
    }

    fn show_on_desk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    crate::desk_panel::show_reminders(workspace, window, cx)
                })
                .log_err();
        });
    }

    fn today(&self, cx: &App) -> NaiveDate {
        ReminderClock::global(cx).today()
    }

    fn rows<'a>(&self, cx: &'a App) -> Vec<(RowGroup, &'a IndexedReminder)> {
        let Some(service) = &self.service else {
            return Vec::new();
        };
        let service = service.read(cx);
        group_rows(
            service.reminders(),
            self.today(cx),
            service.settings().date_only_at,
        )
    }

    /// The number of overdue reminders, for the tab's count.
    pub fn overdue_count(&self, cx: &App) -> usize {
        self.rows(cx)
            .iter()
            .filter(|(group, _)| *group == RowGroup::Overdue)
            .count()
    }

    pub fn set_row_above(&mut self, row_above: bool) {
        self.row_above = row_above;
    }

    pub fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

    /// The selected row's position, by id.
    fn selection(&self, cx: &App) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows(cx)
            .iter()
            .position(|(_, reminder)| &reminder.id == selected)
    }

    fn selected_reminder<'a>(&self, cx: &'a App) -> Option<&'a IndexedReminder> {
        let selected = self.selected.as_ref()?;
        self.rows(cx)
            .into_iter()
            .map(|(_, reminder)| reminder)
            .find(|reminder| &reminder.id == selected)
    }

    /// After a reload, a reminder that left the list (closed, dropped)
    /// hands the selection to the row now in its place.
    fn keep_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_none() || self.selection(cx).is_some() {
            return;
        }
        let rows = self.rows(cx);
        let position = self.selected_position.min(rows.len().saturating_sub(1));
        let id = rows.get(position).map(|(_, reminder)| reminder.id.clone());
        self.reschedule = None;
        self.select_id(id, position);
    }

    fn select_id(&mut self, id: Option<String>, position: usize) {
        self.selected_position = position;
        self.selected = id;
    }

    fn select_position(&mut self, position: usize, cx: &mut Context<Self>) {
        let id = self
            .rows(cx)
            .get(position)
            .map(|(_, reminder)| reminder.id.clone());
        if id.is_some() {
            self.select_id(id, position);
            self.reschedule = None;
            self.scroll_handle.scroll_to_item(position);
            cx.notify();
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(option) = self.reschedule {
            self.reschedule = Some((option + 1).min(RESCHEDULE_OPTIONS - 1));
            cx.notify();
            return;
        }
        let count = self.rows(cx).len();
        let next = match self.selection(cx) {
            Some(position) => (position + 1).min(count.saturating_sub(1)),
            None => 0,
        };
        self.select_position(next, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(option) = self.reschedule {
            self.reschedule = Some(option.saturating_sub(1));
            cx.notify();
            return;
        }
        match self.selection(cx) {
            Some(0) if self.row_above => {
                self.selected = None;
                cx.notify();
                cx.propagate();
            }
            Some(position) => self.select_position(position.saturating_sub(1), cx),
            None => {
                let last = self.rows(cx).len().saturating_sub(1);
                self.select_position(last, cx);
            }
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        if self.reschedule.is_some() {
            self.reschedule = Some(0);
            cx.notify();
            return;
        }
        self.select_position(0, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        if self.reschedule.is_some() {
            self.reschedule = Some(RESCHEDULE_OPTIONS - 1);
            cx.notify();
            return;
        }
        let last = self.rows(cx).len().saturating_sub(1);
        self.select_position(last, cx);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(option) = self.reschedule {
            self.choose(option, window, cx);
            return;
        }
        let Some(reminder) = self.selected_reminder(cx) else {
            // Nothing here to act on; the Desk may have a row above.
            cx.propagate();
            return;
        };
        let Some(service) = &self.service else {
            return;
        };
        let path = service.read(cx).abs_path(&reminder.home);
        let row = reminder.line.and_then(|line| u32::try_from(line).ok());
        self.open_at(path, row, false, window, cx);
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.reschedule.take().is_some() {
            cx.notify();
            return;
        }
        // A dock panel never traps the keyboard: escape goes back to the note.
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

    fn close_reminder(&mut self, _: &CloseReminder, _: &mut Window, cx: &mut Context<Self>) {
        // While the reschedule menu is open, keys belong to it.
        if self.reschedule.is_none() {
            self.edit_selected(HomeEdit::Close, cx);
        }
    }

    fn drop_reminder(&mut self, _: &DropReminder, _: &mut Window, cx: &mut Context<Self>) {
        if self.reschedule.is_none() {
            self.edit_selected(HomeEdit::Drop, cx);
        }
    }

    fn reschedule_reminder(
        &mut self,
        _: &RescheduleReminder,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_reminder(cx).is_some() {
            self.reschedule = Some(0);
            cx.notify();
        }
    }

    fn choose_option(
        &mut self,
        action: &ChooseRescheduleOption,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reschedule.is_some() && action.index < RESCHEDULE_OPTIONS {
            self.choose(action.index, window, cx);
        }
    }

    /// *Tomorrow 09:00*, *Monday 09:00* or *Pick a date…*.
    fn choose(&mut self, option: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.reschedule = None;
        cx.notify();
        let Some(service) = self.service.clone() else {
            return;
        };
        let today = self.today(cx);
        let settings = service.read(cx).settings();
        let due = match option {
            0 => settings.tomorrow_due(today),
            1 => settings.monday_due(today),
            _ => {
                self.pick_date(window, cx);
                return;
            }
        };
        self.edit_selected(HomeEdit::Rewrite(due), cx);
    }

    fn pick_date(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reminder) = self.selected_reminder(cx) else {
            return;
        };
        let id = reminder.id.clone();
        let start = reminder.due;
        let focus = self.focus_handle.clone();
        let this = cx.weak_entity();
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    pick_due(
                        workspace,
                        start,
                        focus,
                        move |due: Due, _, cx| {
                            this.update(cx, |this, cx| {
                                this.edit_reminder(&id, HomeEdit::Rewrite(due), cx)
                            })
                            .log_err();
                        },
                        window,
                        cx,
                    )
                })
                .log_err();
        });
    }

    fn edit_selected(&mut self, edit: HomeEdit, cx: &mut Context<Self>) {
        let Some(id) = self
            .selected_reminder(cx)
            .map(|reminder| reminder.id.clone())
        else {
            return;
        };
        self.edit_reminder(&id, edit, cx);
    }

    fn edit_reminder(&mut self, id: &str, edit: HomeEdit, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        let task = service.update(cx, |service, cx| service.edit_reminder(id, edit, cx));
        self.report_failure(task, cx);
    }

    fn new_reminder(&mut self, _: &NewReminder, window: &mut Window, cx: &mut Context<Self>) {
        if self.reschedule.is_some() {
            return;
        }
        let Some(service) = self.service.clone() else {
            self.show_error(
                "This workspace isn't a Thock vault, so it has no reminders.",
                cx,
            );
            return;
        };
        let task = service.update(cx, |service, cx| service.new_loose_line(cx));
        cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok((path, row)) => {
                    this.update_in(cx, |this, window, cx| {
                        this.open_at(path, Some(row), true, window, cx)
                    })?;
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.show_error(&format!("Couldn't add a reminder: {error:#}"), cx)
                    })?;
                }
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    /// Surfaces a failed write as a workspace error.
    fn report_failure(&self, task: Task<Result<()>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.show_error(&format!("Couldn't change the reminder: {error:#}"), cx)
                })
                .log_err();
            }
        })
        .detach();
    }

    fn show_error(&self, message: &str, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let message = message.to_string();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.show_error(message, cx))
                .log_err();
        });
    }

    /// Opens `path` with the cursor on `row`: at its start, or its end for
    /// a line the person is about to type on.
    fn open_at(
        &mut self,
        path: PathBuf,
        row: Option<u32>,
        at_end: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            if let (Some(editor), Some(row)) = (item.downcast::<Editor>(), row) {
                editor.update_in(cx, |editor, window, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let column = if at_end { u32::MAX } else { 0 };
                    let point = snapshot.clip_point(Point::new(row, column), Bias::Left);
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()).nav_history(true),
                        window,
                        cx,
                        |selections| selections.select_ranges([point..point]),
                    );
                    window.focus(&editor.focus_handle(cx), cx);
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn render_hint(&self, text: &'static str) -> AnyElement {
        v_flex()
            .p_3()
            .child(Label::new(text).size(LabelSize::Small).color(Color::Muted))
            .into_any_element()
    }

    fn render_row(
        &self,
        position: usize,
        group: RowGroup,
        reminder: &IndexedReminder,
        today: NaiveDate,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let selected = self.selected.as_ref() == Some(&reminder.id);
        let text = reminder
            .text
            .clone()
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "line not found".to_string());
        let meta_color = match group {
            RowGroup::Overdue => cx.theme().status().warning,
            _ => colors.text_placeholder,
        };
        let id = reminder.id.clone();
        v_flex()
            .id(SharedString::from(format!(
                "thock-reminder-{}",
                reminder.id
            )))
            .w_full()
            .pl(px(10.))
            .pr_2()
            .py_1()
            .border_l_2()
            .border_color(if selected {
                colors.text_accent
            } else {
                gpui::transparent_black()
            })
            .when(selected, |this| this.bg(colors.element_selected))
            .cursor_pointer()
            .child(
                div()
                    .text_size(rems(0.875))
                    .text_color(if group == RowGroup::NotFound {
                        colors.text_muted
                    } else {
                        colors.text
                    })
                    .line_clamp(2)
                    .child(SharedString::from(text)),
            )
            .child(
                div()
                    .text_size(rems(0.71875))
                    .text_color(meta_color)
                    .child(SharedString::from(row_meta(reminder, today))),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_id(Some(id.clone()), position);
                this.focus_handle.focus(window, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_reschedule_menu(&self, option: usize, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let settings = self
            .service
            .as_ref()
            .map(|service| service.read(cx).settings().clone())
            .unwrap_or_default();
        let labels = reschedule_labels(&settings, self.today(cx));
        v_flex()
            .mx_3()
            .my_1()
            .p_1()
            .rounded(px(6.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            .children(labels.into_iter().enumerate().map(|(index, label)| {
                h_flex()
                    .id(("thock-reschedule-option", index))
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(index == option, |this| this.bg(colors.element_selected))
                    .child(
                        Label::new((index + 1).to_string())
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(Label::new(label).size(LabelSize::Small))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose(index, window, cx);
                    }))
            }))
            .into_any_element()
    }
}

impl Focusable for RemindersPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RemindersPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add(KEY_CONTEXT);
        key_context.add("menu");
        if self.reschedule.is_some() {
            key_context.add("rescheduling");
        }
        let today = self.today(cx);
        let body = if self.service.is_none() {
            self.render_hint("Open a Thock vault to see its reminders.")
        } else {
            let rows = self.rows(cx);
            if rows.is_empty() {
                self.render_hint("Nothing to be reminded of. Add (@tomorrow 9) to any line.")
            } else {
                let mut list = v_flex().py_1();
                let mut group = None;
                for (position, (row_group, reminder)) in rows.into_iter().enumerate() {
                    if group != Some(row_group) {
                        group = Some(row_group);
                        list = list.child(
                            h_flex()
                                .px_3()
                                .pt_2()
                                .pb_1()
                                .gap_1()
                                .when(row_group == RowGroup::Overdue, |this| {
                                    this.child(
                                        Icon::new(IconName::Warning)
                                            .size(IconSize::XSmall)
                                            .color(Color::Warning),
                                    )
                                })
                                .child(
                                    Label::new(row_group.title())
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                        );
                    }
                    let selected = self.selected.as_ref() == Some(&reminder.id);
                    list = list.child(self.render_row(position, row_group, reminder, today, cx));
                    if selected && let Some(option) = self.reschedule {
                        list = list.child(self.render_reschedule_menu(option, cx));
                    }
                }
                list.id("thock-reminders-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .into_any_element()
            }
        };
        v_flex()
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::close_reminder))
            .on_action(cx.listener(Self::drop_reminder))
            .on_action(cx.listener(Self::reschedule_reminder))
            .on_action(cx.listener(Self::choose_option))
            .on_action(cx.listener(Self::new_reminder))
            .size_full()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog_panel::BacklogFolds;
    use crate::desk_panel::{DeskPanel, DeskTab};
    use crate::reminder_index::{EntryKind, IndexedState};
    use crate::reminders::TestClock;
    use fs::{FakeFs, Fs as _};
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use std::sync::Arc;
    use std::time::Duration;
    use util::paths::PathStyle;
    use workspace::MultiWorkspace;

    const HOUSE: &str = "# House\n\
        - [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->\n\
        - [ ] Renew passport (@2026-10-20 09:00) <!--rem:3f9a1c0d4e21-->\n\
        - [ ] Book flights (@2026-10-25) <!--rem:2f6b08e19a5c-->\n";
    const INDEX: &str = "# Reminders\n\n## Open\n\
        - [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
        - [[projects/house]] <!--rem:3f9a1c0d4e21-->\n\
        - [[projects/house]] <!--rem:2f6b08e19a5c-->\n\
        - [[backlog]] <!--rem:aaaaaaaaaaaa-->\n\n## Done\n";
    const BACKLOG: &str = "## Soon\n\n## Completed\n\n\
        - [x] Pay the bill (@2026-10-19) ✅ 2026-10-19 <!--rem:aaaaaaaaaaaa-->\n";

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
                // `vim::MenuSelectNext` lives in the vim crate, which this
                // crate doesn't link; it dispatches `menu::SelectNext`.
                cx.bind_keys([
                    KeyBinding::new("j", SelectNext, Some(KEY_CONTEXT)),
                    KeyBinding::new("k", SelectPrevious, Some(KEY_CONTEXT)),
                ]);
            }
            let now = date(2026, 10, 21).and_time(time(10, 0));
            TestClock::install(now, cx);
            crate::reminders::init(cx);
            reminder_service::init(cx);
            crate::desk_panel::init(cx);
        });
    }

    struct Setup {
        workspace: Entity<Workspace>,
        desk: Entity<DeskPanel>,
        fs: Arc<FakeFs>,
        root: PathBuf,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    async fn setup(cx: &mut TestAppContext, vim: bool) -> Setup {
        init_test(cx, vim);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        // `backlog.md` must exist on disk for the Backlog view to adopt it.
        std::fs::write(root.join("backlog.md"), BACKLOG).unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({
                ".thock": { "config.toml": "schema = 1\n" },
                "projects": { "house.md": HOUSE },
                "reminders.md": INDEX,
                "backlog.md": BACKLOG,
                "notes.md": "# Notes\n",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [root.as_path()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        let project_path = project
            .read_with(&mut cx, |project, cx| {
                project.find_project_path(root.join("notes.md"), cx)
            })
            .unwrap();
        workspace
            .update_in(&mut cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap();
        let desk = workspace.update_in(&mut cx, |workspace, window, cx| {
            let desk = DeskPanel::new(workspace, BacklogFolds::default(), window, cx);
            workspace.add_panel(desk.clone(), window, cx);
            desk
        });
        let mut setup = Setup {
            workspace,
            desk,
            fs,
            root,
            cx,
            _vault_dir: vault_dir,
        };
        settle(&mut setup);
        setup
    }

    fn settle(setup: &mut Setup) {
        for _ in 0..12 {
            setup
                .cx
                .executor()
                .advance_clock(Duration::from_millis(500));
            setup.cx.run_until_parked();
        }
    }

    fn panel(setup: &Setup) -> Entity<RemindersPanel> {
        setup
            .desk
            .read_with(&setup.cx, |desk, _| desk.reminders().clone())
    }

    fn rows(setup: &Setup) -> Vec<(RowGroup, String)> {
        let panel = panel(setup);
        panel.read_with(&setup.cx, |panel, cx| {
            panel
                .rows(cx)
                .into_iter()
                .map(|(group, reminder)| (group, reminder.text.clone().unwrap_or_default()))
                .collect()
        })
    }

    fn selected_text(setup: &Setup) -> Option<String> {
        let panel = panel(setup);
        panel.read_with(&setup.cx, |panel, cx| {
            panel
                .selected_reminder(cx)
                .and_then(|reminder| reminder.text.clone())
        })
    }

    fn panel_focused(setup: &mut Setup) -> bool {
        let panel = panel(setup);
        setup
            .cx
            .update(|window, cx| panel.read(cx).focus_handle.contains_focused(window, cx))
    }

    async fn house(setup: &Setup) -> String {
        setup
            .fs
            .load(&setup.root.join("projects/house.md"))
            .await
            .unwrap()
    }

    fn active_note(setup: &mut Setup) -> String {
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx))
                .map(|path| path.path.display(PathStyle::Unix).into_owned())
                .unwrap_or_default()
        })
    }

    #[gpui::test]
    async fn the_tab_closes_reschedules_and_drops_by_keyboard(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("ctrl-alt-r");
        setup.cx.run_until_parked();
        assert_eq!(
            setup.desk.read_with(&setup.cx, |desk, _| desk.tab()),
            DeskTab::Reminders
        );
        assert!(panel_focused(&mut setup));
        assert_eq!(
            rows(&setup),
            vec![
                (RowGroup::Overdue, "Renew passport".to_string()),
                (RowGroup::Today, "Call the plumber".to_string()),
                (RowGroup::Upcoming, "Book flights".to_string()),
            ]
        );
        let overdue = panel(&setup).read_with(&setup.cx, |panel, cx| panel.overdue_count(cx));
        assert_eq!(overdue, 1, "the tab counts the overdue one");

        setup.cx.simulate_keystrokes("down");
        assert_eq!(selected_text(&setup).as_deref(), Some("Renew passport"));
        setup.cx.simulate_keystrokes("x");
        settle(&mut setup);
        assert!(
            house(&setup)
                .await
                .contains("- [x] Renew passport (@2026-10-20 09:00)")
        );
        assert_eq!(
            selected_text(&setup).as_deref(),
            Some("Call the plumber"),
            "the next row takes the selection of a closed one"
        );

        setup.cx.simulate_keystrokes("s");
        setup.cx.simulate_keystrokes("1");
        settle(&mut setup);
        assert!(
            house(&setup)
                .await
                .contains("- [ ] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->"),
            "{}",
            house(&setup).await
        );
        assert_eq!(
            selected_text(&setup).as_deref(),
            Some("Call the plumber"),
            "the selection follows the reminder by id"
        );
        assert_eq!(
            rows(&setup),
            vec![
                (RowGroup::Upcoming, "Call the plumber".to_string()),
                (RowGroup::Upcoming, "Book flights".to_string()),
            ]
        );

        // A file event that moves the line keeps the selection on it.
        let edited = house(&setup)
            .await
            .replace("# House\n", "# House\n- [ ] A new task\n");
        setup
            .fs
            .atomic_write(setup.root.join("projects/house.md"), edited)
            .await
            .unwrap();
        settle(&mut setup);
        assert_eq!(selected_text(&setup).as_deref(), Some("Call the plumber"));

        setup.cx.simulate_keystrokes("d");
        settle(&mut setup);
        assert!(
            house(&setup)
                .await
                .contains("- [ ] Call the plumber <!--rem:7d3e55a1c0f2-->")
        );
        assert_eq!(selected_text(&setup).as_deref(), Some("Book flights"));

        // *Pick a date…* opens the calendar on the reminder's day.
        setup.cx.simulate_keystrokes("s 3");
        setup.cx.run_until_parked();
        let calendar = setup.workspace.read_with(&setup.cx, |workspace, cx| {
            workspace.active_modal::<crate::reminder_calendar::ReminderCalendar>(cx)
        });
        assert!(calendar.is_some(), "the calendar opens");
        setup.cx.simulate_keystrokes("right 1 0 enter");
        settle(&mut setup);
        assert!(
            house(&setup)
                .await
                .contains("- [ ] Book flights (@2026-10-26 10:00) <!--rem:2f6b08e19a5c-->"),
            "{}",
            house(&setup).await
        );
        assert!(panel_focused(&mut setup), "focus comes back to the tab");

        // Done lists the closed reminder beside the finished tasks.
        let mut done = setup.desk.read_with(&setup.cx, |desk, cx| {
            desk.backlog()
                .read(cx)
                .done_reminders()
                .iter()
                .map(|reminder| reminder.text.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        });
        done.sort();
        assert_eq!(
            done,
            vec!["Call the plumber".to_string(), "Renew passport".to_string()]
        );

        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "projects/house.md");

        // On Done the cursor walks the finished reminders, and enter opens
        // the home line.
        let notes = setup.root.join("notes.md");
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.open_abs_path(notes, OpenOptions::default(), window, cx)
            })
            .await
            .unwrap();
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "notes.md");
        setup.cx.simulate_keystrokes("ctrl-alt-r 4 down");
        let selected = setup
            .desk
            .read_with(&setup.cx, |desk, _| desk.backlog().clone());
        let selected = selected.read_with(&setup.cx, |backlog, _| {
            backlog.selected_reminder_for_tests()
        });
        assert!(selected.is_some(), "the first Done row is a reminder");
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "projects/house.md");
    }

    #[gpui::test]
    async fn vim_motions_escape_and_a_new_reminder(cx: &mut TestAppContext) {
        let mut setup = setup(cx, true).await;
        setup.cx.simulate_keystrokes("ctrl-alt-r");
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("j j");
        assert_eq!(selected_text(&setup).as_deref(), Some("Call the plumber"));
        setup.cx.simulate_keystrokes("k");
        assert_eq!(selected_text(&setup).as_deref(), Some("Renew passport"));
        setup.cx.simulate_keystrokes("shift-g");
        assert_eq!(selected_text(&setup).as_deref(), Some("Book flights"));

        setup.cx.simulate_keystrokes("s x d o");
        settle(&mut setup);
        assert_eq!(
            house(&setup).await,
            HOUSE,
            "row keys do nothing while the reschedule menu is open"
        );
        assert_eq!(active_note(&mut setup), "notes.md");
        setup.cx.simulate_keystrokes("escape");
        assert!(panel_focused(&mut setup), "escape closes the menu first");
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(!panel_focused(&mut setup), "then goes back to the note");
        assert_eq!(active_note(&mut setup), "notes.md");

        setup.cx.simulate_keystrokes("ctrl-alt-r");
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("o");
        settle(&mut setup);
        assert_eq!(active_note(&mut setup), "reminders.md");
        let index = setup
            .fs
            .load(&setup.root.join("reminders.md"))
            .await
            .unwrap();
        assert!(index.contains("- [ ] \n"), "{index}");
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    fn reminder(
        id: &str,
        due: Option<Due>,
        state: IndexedState,
        section: IndexSection,
    ) -> IndexedReminder {
        IndexedReminder {
            id: id.to_string(),
            section,
            kind: EntryKind::Link {
                home: "projects/house.md".into(),
            },
            stamp: None,
            home: "projects/house.md".into(),
            line: Some(0),
            line_text: None,
            text: Some(id.to_string()),
            due,
            task: None,
            state,
        }
    }

    #[test]
    fn rows_group_by_day_and_sort_by_when_they_ring() {
        let today = date(2026, 10, 21);
        let open =
            |id: &str, due: Due| reminder(id, Some(due), IndexedState::Open, IndexSection::Open);
        let reminders = vec![
            open("upcoming", Due::new(date(2026, 10, 22), Some(time(8, 0)))),
            open("today late", Due::new(today, Some(time(17, 0)))),
            open("today date only", Due::new(today, None)),
            open("overdue", Due::new(date(2026, 10, 20), Some(time(9, 0)))),
            reminder(
                "closed",
                Some(Due::new(today, None)),
                IndexedState::Closed,
                IndexSection::Done,
            ),
            reminder("missing", None, IndexedState::Missing, IndexSection::Open),
        ];
        let rows: Vec<(RowGroup, &str)> = group_rows(&reminders, today, time(9, 0))
            .into_iter()
            .map(|(group, reminder)| (group, reminder.id.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (RowGroup::Overdue, "overdue"),
                (RowGroup::Today, "today date only"),
                (RowGroup::Today, "today late"),
                (RowGroup::Upcoming, "upcoming"),
                (RowGroup::NotFound, "missing"),
            ]
        );
    }

    #[test]
    fn the_reschedule_choices_are_dated() {
        let settings = ReminderSettings::default();
        assert_eq!(
            reschedule_labels(&settings, date(2026, 10, 21)),
            [
                "Tomorrow 09:00".to_string(),
                "Mon 26 Oct 09:00".to_string(),
                "Pick a date…".to_string()
            ]
        );
        assert_eq!(
            reschedule_labels(&settings, date(2026, 10, 25))[1],
            "Tomorrow 09:00",
            "on a Sunday, Monday is tomorrow"
        );
    }

    #[test]
    fn the_meta_line_names_the_due_and_the_home() {
        let today = date(2026, 10, 21);
        let mut overdue = reminder(
            "a",
            Some(Due::new(date(2026, 10, 20), Some(time(9, 0)))),
            IndexedState::Open,
            IndexSection::Open,
        );
        assert_eq!(
            row_meta(&overdue, today),
            "Yesterday · 09:00 · projects/house"
        );
        overdue.due = Some(Due::new(date(2026, 12, 2), None));
        overdue.home = "backlog.md".into();
        assert_eq!(row_meta(&overdue, today), "Wed 2 Dec · backlog");
    }
}
