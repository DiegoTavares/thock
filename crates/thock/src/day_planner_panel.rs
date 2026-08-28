//! The Day Planner Context panel (spec `v4-day-planner-panel.md`): a
//! right-dock panel that follows the active editor item. When that item is a
//! daily note of the current vault, its checklist is parsed into a vertical
//! day grid — timed tasks as duration-scaled blocks, unscheduled tasks as
//! chips. When no daily note is active it falls back to today's note,
//! opened as a background buffer. Read-only; the one interaction is
//! reveal-on-click into the editor.

use anyhow::Result;
use chrono::{Local, NaiveDate, Timelike as _};
use editor::{Editor, EditorEvent, RowHighlightOptions, SelectionEffects, scroll::Autoscroll};
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Pixels, Subscription, Task, WeakEntity, Window, actions, div, px, relative,
};
use language::{Buffer, BufferEvent};
use multi_buffer::MultiBufferRow;
use project::Project;
use std::time::Duration;
use text::{Bias, Point};
use ui::prelude::*;
use ui::{Icon, IconSize, Label, LabelLike};
use util::ResultExt as _;
use workspace::Workspace;
use workspace::dock::{DockPosition, Panel, PanelEvent};

use crate::calendar_service::{
    self, CalendarService, ConnectGoogleWorkspace, SyncCalendarNow, SyncState,
};
use crate::day_plan::{self, DayPlan, PlacedBlock, PlanItem, parse_day_plan};
use crate::markdown_text::render_markdown_row;
use crate::notes::{NoteKind, format_date};
use crate::vault::VaultStatus;

const DAY_PLANNER_PANEL_KEY: &str = "ThockDayPlannerPanel";
const HOUR_HEIGHT: f32 = 48.0;
const MIN_BLOCK_PX: f32 = 18.0;
const BLOCK_CAPTION_PX: f32 = 18.0;
const BLOCK_LABEL_LINE_PX: f32 = 16.0;
const GUTTER_WIDTH: f32 = 44.0;
/// Width of the lane that holds calendar status blocks (focus time, out of
/// office). Narrow on purpose: they mark hours, they don't compete for them.
const STATUS_LANE_WIDTH: f32 = 52.0;
const REPARSE_DEBOUNCE: Duration = Duration::from_millis(150);

/// Marker type isolating the panel's transient reveal highlight from other
/// row-highlight owners in the editor.
enum DayPlannerHighlight {}

actions!(
    thock,
    [
        /// Toggles focus on the Thock day planner panel.
        ToggleDayPlannerFocus
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleDayPlannerFocus, window, cx| {
            workspace.toggle_panel_focus::<DayPlannerPanel>(window, cx);
        });
    })
    .detach();
}

/// How a plan item reads in the panel. A struck-through item is finished the
/// same way a ticked one is, but takes the dimmer disabled tone so a dropped
/// task stays distinguishable from a completed one.
#[derive(Clone, Copy, PartialEq)]
enum ItemState {
    Open,
    Done,
    Struck,
}

impl ItemState {
    fn of(item: &PlanItem) -> Self {
        match (item.struck, item.done) {
            (true, _) => Self::Struck,
            (false, true) => Self::Done,
            (false, false) => Self::Open,
        }
    }

    fn finished(self) -> bool {
        self != Self::Open
    }

    /// The muted colour a finished item's label and icon take; `None` while
    /// the item is still open.
    fn finished_color(self) -> Option<Color> {
        match self {
            Self::Open => None,
            Self::Done => Some(Color::Muted),
            Self::Struck => Some(Color::Disabled),
        }
    }
}

/// Where the mirrored note's text comes from: the active editor when it is
/// a daily note, or today's note opened as a background buffer otherwise.
enum NoteSource {
    Editor(WeakEntity<Editor>),
    Buffer(Entity<Buffer>),
}

/// The daily note currently mirrored by the panel.
struct ActiveNote {
    source: NoteSource,
    date: NaiveDate,
    plan: DayPlan,
    _source_subscription: Subscription,
}

pub struct DayPlannerPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    position: DockPosition,
    vault_status: VaultStatus,
    calendar_service: Option<Entity<CalendarService>>,
    active: Option<ActiveNote>,
    /// Panel-local UI state: the last clicked block/chip (spec §8).
    selected_item: Option<usize>,
    reparse_task: Option<Task<()>>,
    fallback_open_task: Option<Task<()>>,
    /// Coarse repaint driver for the "now" line.
    _now_tick: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl DayPlannerPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            DayPlannerPanel::new(workspace, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        _window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = workspace.weak_handle();
        let workspace_entity = cx.entity();
        cx.new(|cx| {
            let project_subscription =
                cx.subscribe(&project, |this: &mut Self, _, event, cx| {
                    if matches!(
                        event,
                        project::Event::WorktreeAdded(_)
                            | project::Event::WorktreeRemoved(_)
                            | project::Event::WorktreeUpdatedEntries(..)
                    ) {
                        this.refresh_vault_status(cx);
                    }
                });
            let workspace_subscription = cx.subscribe(
                &workspace_entity,
                |this: &mut Self, _, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::ActiveItemChanged) {
                        this.update_active_item(false, cx);
                    }
                },
            );
            let now_tick = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(60)).await;
                    let tick = this.update(cx, |this, cx| {
                        this.update_active_item(false, cx);
                        cx.notify();
                    });
                    if tick.is_err() {
                        break;
                    }
                }
            });
            let calendar_service = calendar_service::service_for_project(&project, cx);
            let mut subscriptions = vec![project_subscription, workspace_subscription];
            if let Some(service) = &calendar_service {
                subscriptions.push(cx.observe(service, |_, _, cx| cx.notify()));
            }
            let mut this = Self {
                workspace: weak_workspace,
                project,
                focus_handle: cx.focus_handle(),
                position: DockPosition::Right,
                vault_status: VaultStatus::NotAVault,
                calendar_service,
                active: None,
                selected_item: None,
                reparse_task: None,
                fallback_open_task: None,
                _now_tick: now_tick,
                _subscriptions: subscriptions,
            };
            this.vault_status = this.detect_vault_status(cx);
            // Resolving the active item reads the workspace entity, which is
            // still leased by the `workspace.update_in` that is constructing
            // this panel — reading it here would panic. Defer until the
            // current effect cycle returns the workspace to the app.
            let panel = cx.weak_entity();
            cx.defer(move |cx| {
                panel
                    .update(cx, |this, cx| this.update_active_item(false, cx))
                    .log_err();
            });
            this
        })
    }

    fn detect_vault_status(&self, cx: &App) -> VaultStatus {
        match self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        {
            Some(root) => crate::vault::Vault::detect(&root),
            None => VaultStatus::NotAVault,
        }
    }

    fn refresh_vault_status(&mut self, cx: &mut Context<Self>) {
        let status = self.detect_vault_status(cx);
        let vault_changed = status != self.vault_status;
        if vault_changed {
            self.vault_status = status;
            cx.notify();
        }
        // The vault config feeds both parsing (heading, default duration)
        // and the note's daily-note-ness (the daily directory), so a
        // changed vault must re-parse even when the active editor is the
        // same.
        self.update_active_item(vault_changed, cx);
    }

    /// Re-resolves the active editor item (spec §9.1): when it is a daily
    /// note of the vault, mirror it; otherwise fall back to today's note.
    /// `force_reparse` re-parses even when the active note is unchanged,
    /// for when the vault config changed under it.
    fn update_active_item(&mut self, force_reparse: bool, cx: &mut Context<Self>) {
        let Some((editor, date)) = self.resolve_active_daily_note(cx) else {
            self.fall_back_to_today(force_reparse, cx);
            return;
        };
        self.fallback_open_task = None;
        if let Some(active) = &self.active
            && let NoteSource::Editor(existing) = &active.source
            && existing.entity_id() == editor.entity_id()
            && active.date == date
        {
            if force_reparse {
                self.reparse(cx);
            }
            return;
        }
        let subscription = cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                this.schedule_reparse(cx);
            }
        });
        let plan = self.parse_editor_plan(&editor, cx);
        self.set_active(
            Some(ActiveNote {
                source: NoteSource::Editor(editor.downgrade()),
                date,
                plan,
                _source_subscription: subscription,
            }),
            cx,
        );
    }

    /// Mirror today's note when the active item is not a daily note. The
    /// note is opened as a background buffer, so edits made in other panes
    /// and disk changes (calendar sync, agents) still reach the panel; a
    /// note that does not exist yet renders as an empty day.
    fn fall_back_to_today(&mut self, force_reparse: bool, cx: &mut Context<Self>) {
        let today = Local::now().date_naive();
        let note_path = match &self.vault_status {
            VaultStatus::Valid(vault) => vault.note_path(NoteKind::Daily, today),
            _ => {
                self.fallback_open_task = None;
                if self.active.is_some() {
                    self.set_active(None, cx);
                }
                return;
            }
        };
        if let Some(active) = &self.active
            && matches!(active.source, NoteSource::Buffer(_))
            && active.date == today
        {
            if force_reparse {
                self.reparse(cx);
            }
            return;
        }
        let Some(project_path) = self
            .project
            .read(cx)
            .project_path_for_absolute_path(&note_path, cx)
        else {
            self.fallback_open_task = None;
            if self.active.is_some() {
                self.set_active(None, cx);
            }
            return;
        };
        let open_buffer = self
            .project
            .update(cx, |project, cx| project.open_buffer(project_path, cx));
        self.fallback_open_task = Some(cx.spawn(async move |this, cx| {
            let Some(buffer) = open_buffer.await.log_err() else {
                return;
            };
            this.update(cx, |this, cx| {
                // The active item may have become a daily note while the
                // open was in flight; it wins over the fallback.
                if this.resolve_active_daily_note(cx).is_some() {
                    return;
                }
                let subscription =
                    cx.subscribe(&buffer, |this, _, event: &BufferEvent, cx| {
                        if matches!(event, BufferEvent::Edited { .. } | BufferEvent::Reloaded) {
                            this.schedule_reparse(cx);
                        }
                    });
                let plan = this.parse_text_plan(&buffer.read(cx).text());
                this.set_active(
                    Some(ActiveNote {
                        source: NoteSource::Buffer(buffer),
                        date: today,
                        plan,
                        _source_subscription: subscription,
                    }),
                    cx,
                );
            })
            .log_err();
        }));
    }

    fn set_active(&mut self, active: Option<ActiveNote>, cx: &mut Context<Self>) {
        self.clear_transient_highlight(cx);
        self.selected_item = None;
        self.reparse_task = None;
        self.active = active;
        cx.notify();
    }

    fn resolve_active_daily_note(&self, cx: &App) -> Option<(Entity<Editor>, NaiveDate)> {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return None;
        };
        let workspace = self.workspace.upgrade()?;
        let item = workspace.read(cx).active_item(cx)?;
        let editor = item.downcast::<Editor>()?;
        let project_path = item.project_path(cx)?;
        let abs_path = self.project.read(cx).absolute_path(&project_path, cx)?;
        let date = vault.daily_note_date(&abs_path)?;
        Some((editor, date))
    }

    fn parse_editor_plan(&self, editor: &Entity<Editor>, cx: &App) -> DayPlan {
        let text = editor.read(cx).buffer().read(cx).snapshot(cx).text();
        self.parse_text_plan(&text)
    }

    fn parse_text_plan(&self, text: &str) -> DayPlan {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return DayPlan::default();
        };
        parse_day_plan(text, &vault.config.day_planner)
    }

    fn schedule_reparse(&mut self, cx: &mut Context<Self>) {
        self.reparse_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REPARSE_DEBOUNCE).await;
            this.update(cx, |this, cx| this.reparse(cx)).log_err();
        }));
    }

    fn reparse(&mut self, cx: &mut Context<Self>) {
        let Some(active) = &self.active else {
            return;
        };
        let plan = match &active.source {
            NoteSource::Editor(editor) => {
                let Some(editor) = editor.upgrade() else {
                    return;
                };
                self.parse_editor_plan(&editor, cx)
            }
            NoteSource::Buffer(buffer) => self.parse_text_plan(&buffer.read(cx).text()),
        };
        if let Some(active) = &mut self.active
            && active.plan != plan
        {
            active.plan = plan;
            // Item indices may have shifted; a stale selection would
            // outline the wrong block.
            self.selected_item = None;
            cx.notify();
        }
    }

    fn clear_transient_highlight(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = &self.active
            && let NoteSource::Editor(editor) = &active.source
            && let Some(editor) = editor.upgrade()
        {
            editor.update(cx, |editor, cx| {
                editor.clear_row_highlights::<DayPlannerHighlight>();
                cx.notify();
            });
        }
    }

    /// Reveal-on-click (spec §8): select + scroll to the item's source line
    /// in the editor and paint the transient row highlight. Never modifies
    /// the note.
    fn reveal_item(&mut self, item_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = &self.active else {
            return;
        };
        let Some(item) = active.plan.items.get(item_index) else {
            return;
        };
        let row = item.row;
        let editor = match &active.source {
            NoteSource::Editor(editor) => {
                let Some(editor) = editor.upgrade() else {
                    return;
                };
                editor
            }
            NoteSource::Buffer(_) => {
                self.open_today_and_reveal(row, item_index, window, cx);
                return;
            }
        };
        Self::reveal_in_editor(&editor, row, window, cx);
        self.selected_item = Some(item_index);
        cx.notify();
    }

    /// The fallback note has no editor to reveal into: open today's note in
    /// the workspace, then reveal once the editor exists. Deferred to a task
    /// so the workspace is never re-entered from inside a panel update.
    fn open_today_and_reveal(
        &mut self,
        row: u32,
        item_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active) = &self.active else {
            return;
        };
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        let note_path = vault.note_path(NoteKind::Daily, active.date);
        let Some(project_path) = self
            .project
            .read(cx)
            .project_path_for_absolute_path(&note_path, cx)
        else {
            return;
        };
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let open_task = workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })?;
            let item = open_task.await?;
            if let Some(editor) = item.downcast::<Editor>() {
                this.update_in(cx, |this, window, cx| {
                    Self::reveal_in_editor(&editor, row, window, cx);
                    this.selected_item = Some(item_index);
                    cx.notify();
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn reveal_in_editor(editor: &Entity<Editor>, row: u32, window: &mut Window, cx: &mut App) {
        editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            // Clip, don't index: the note may have shrunk since the parse.
            let start_point = snapshot.clip_point(Point::new(row, 0), Bias::Left);
            let mut end_point = Point::new(
                start_point.row,
                snapshot.line_len(MultiBufferRow(start_point.row)),
            );
            if end_point == start_point {
                // Force a non-empty range so the row still paints.
                end_point = snapshot.clip_point(Point::new(start_point.row + 1, 0), Bias::Left);
            }
            let start = snapshot.anchor_before(start_point);
            let end = snapshot.anchor_after(end_point);
            editor.clear_row_highlights::<DayPlannerHighlight>();
            editor.highlight_rows::<DayPlannerHighlight>(
                start..end,
                |cx| cx.theme().colors().editor_highlighted_line_background,
                RowHighlightOptions {
                    autoscroll: true,
                    ..Default::default()
                },
                cx,
            );
            editor.change_selections(
                SelectionEffects::scroll(Autoscroll::center()).nav_history(true),
                window,
                cx,
                |selections| selections.select_anchor_ranges([start..start]),
            );
            editor.focus_handle(cx).focus(window, cx);
        });
    }

    /// The calendar-sync status row (spec v8 §10.3), shown only when the
    /// vault has a Calendar config, so an unconnected vault looks exactly as
    /// it does today (G6). The actions it triggers are also in the command
    /// palette (`thock: connect google workspace`, `thock: sync calendar
    /// now`).
    fn render_status_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let service = self.calendar_service.as_ref()?.read(cx);
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
                "thock-connect-google-workspace",
                "Connect Google Workspace",
            )],
            SyncState::Connecting => vec![muted("Calendar · connecting…".to_string())],
            SyncState::Idle => vec![muted("Calendar · waiting for first sync".to_string())],
            SyncState::Synced { at } => {
                vec![muted(format!("Calendar · synced {}", format_ago(at.elapsed())))]
            }
            SyncState::Holding { reason } => vec![muted(format!("Calendar · {reason}"))],
            SyncState::Failing { error } => vec![
                muted("Calendar · sync failed".to_string()),
                Button::new("thock-retry-calendar-sync", "Retry")
                    .label_size(LabelSize::Small)
                    .tooltip(ui::Tooltip::text(error.clone()))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(SyncCalendarNow.boxed_clone(), cx);
                    })
                    .into_any_element(),
            ],
            SyncState::Disconnected => vec![
                muted("Calendar · sign-in expired".to_string()),
                connect_button("thock-reconnect-calendar", "Reconnect"),
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

    /// The theme colour for an item's subsection, from the `players()`
    /// palette with index 0 skipped — that slot is the local-user colour and
    /// stays associated with root-level items (spec v8 §11.3). `None` for
    /// root-level items, which keep the accent treatment.
    fn item_section_color(&self, item: &PlanItem, cx: &App) -> Option<gpui::Hsla> {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return None;
        };
        let section = item.section.as_deref()?;
        let players = &cx.theme().players().0;
        let palette = players.get(1..).filter(|palette| !palette.is_empty())?;
        let slot =
            day_plan::section_palette_slot(section, &vault.config.day_planner, palette.len());
        palette.get(slot).map(|color| color.cursor)
    }

    fn render_hint(&self, text: &'static str) -> Div {
        v_flex().p_3().child(
            Label::new(text)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
    }

    fn render_planner(&self, cx: &Context<Self>) -> AnyElement {
        let (VaultStatus::Valid(vault), Some(active)) = (&self.vault_status, &self.active)
        else {
            return self
                .render_hint("Open a daily note to see its schedule.")
                .into_any_element();
        };
        let config = &vault.config.day_planner;
        let plan = &active.plan;

        let header = div()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(Label::new(format_date(active.date, "ddd, MMM D")));

        let mut content = v_flex().size_full().child(header);
        if let Some(strip) = self.render_unscheduled_strip(plan, cx) {
            content = content.child(strip);
        }
        if plan.items.is_empty() {
            content = content.child(self.render_hint(
                "No tasks yet. Add `- [ ] 09:00 – 10:00 Task` under your Day planner heading.",
            ));
        }
        content
            .child(self.render_grid(plan, config, active.date, cx))
            .into_any_element()
    }

    fn render_unscheduled_strip(
        &self,
        plan: &DayPlan,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let unscheduled: Vec<usize> = plan.unscheduled_indices().collect();
        if unscheduled.is_empty() {
            return None;
        }
        let colors = cx.theme().colors();
        Some(
            h_flex()
                .flex_wrap()
                .gap_1()
                .p_2()
                .border_b_1()
                .border_color(colors.border_variant)
                .children(unscheduled.into_iter().filter_map(|item_index| {
                    let item = plan.items.get(item_index)?;
                    Some(self.render_chip(item_index, item, cx))
                }))
                .into_any_element(),
        )
    }

    /// An item's label with its Markdown links rendered as clickable labels.
    /// An empty label still needs something to show, so it falls back to an
    /// ellipsis the way a bare chip always has.
    fn render_item_label(
        &self,
        id: ElementId,
        item: &PlanItem,
        cx: &Context<Self>,
    ) -> AnyElement {
        if item.label.is_empty() {
            return SharedString::from("…").into_any_element();
        }
        render_markdown_row(id, &item.label, &self.project, &self.workspace, cx)
    }

    fn render_chip(&self, item_index: usize, item: &PlanItem, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let selected = self.selected_item == Some(item_index);
        let state = ItemState::of(item);
        // A chip carries the same border colour as its section's blocks so
        // they read as one group (spec v8 §11.3).
        let section_border = (!state.finished())
            .then(|| self.item_section_color(item, cx))
            .flatten()
            .map(|color| color.opacity(0.4));
        let label = LabelLike::new().size(LabelSize::Small).truncate();
        let label = match state.finished_color() {
            Some(color) => label.strikethrough().color(color),
            None => label,
        };
        let label = label.child(self.render_item_label(
            ElementId::Name(format!("thock-day-planner-chip-text-{item_index}").into()),
            item,
            cx,
        ));
        h_flex()
            .id(("thock-day-planner-chip", item_index))
            .max_w_full()
            .gap_1()
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(if selected {
                colors.text_accent
            } else {
                section_border.unwrap_or(colors.border_variant)
            })
            .bg(colors.element_background)
            .cursor_pointer()
            .child(
                Icon::new(if state.finished() {
                    IconName::TodoComplete
                } else {
                    IconName::TodoPending
                })
                .size(IconSize::XSmall)
                .color(state.finished_color().unwrap_or(Color::Muted)),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.reveal_item(item_index, window, cx);
            }))
            .into_any_element()
    }

    fn render_grid(
        &self,
        plan: &DayPlan,
        config: &day_plan::DayPlannerConfig,
        date: NaiveDate,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let (grid_start, grid_end) = day_plan::grid_bounds(plan, config);
        let min_visual_minutes = (MIN_BLOCK_PX / HOUR_HEIGHT * 60.0).ceil() as u32;
        let blocks = day_plan::layout_blocks(plan, min_visual_minutes);
        let total_height = (grid_end - grid_start) as f32 / 60.0 * HOUR_HEIGHT;
        let offset = |minutes: u32| {
            px((minutes.saturating_sub(grid_start)) as f32 / 60.0 * HOUR_HEIGHT)
        };

        let mut body = div().relative().w_full().h(px(total_height));
        for hour in grid_start / 60..grid_end / 60 {
            let minutes = hour * 60;
            body = body
                .child(
                    div()
                        .absolute()
                        .top(offset(minutes))
                        .left(px(GUTTER_WIDTH))
                        .right_0()
                        .h(px(1.0))
                        .bg(colors.border_variant),
                )
                .child(
                    h_flex()
                        .absolute()
                        .top(if minutes == grid_start {
                            offset(minutes)
                        } else {
                            offset(minutes) - px(8.0)
                        })
                        .left_0()
                        .w(px(GUTTER_WIDTH - 6.0))
                        .justify_end()
                        .child(
                            Label::new(hour.to_string())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                );
        }

        // Status blocks own a narrow lane of their own, so the day's real
        // blocks lay out as if a focus-time container weren't there.
        let lane_width = if day_plan::has_status_blocks(plan) {
            STATUS_LANE_WIDTH
        } else {
            0.0
        };
        let lane_blocks = |weight: day_plan::ItemWeight| {
            blocks.iter().filter_map(move |block| {
                let item = plan.items.get(block.item_index)?;
                (item.weight == weight).then_some((block, item))
            })
        };
        if lane_width > 0.0 {
            body = body.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(GUTTER_WIDTH))
                    .w(px(lane_width))
                    .children(
                        lane_blocks(day_plan::ItemWeight::Status)
                            .map(|(block, item)| self.render_block(block, item, grid_start, cx)),
                    ),
            );
        }
        let block_area = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(GUTTER_WIDTH + lane_width))
            .right_0()
            .children(
                lane_blocks(day_plan::ItemWeight::Normal)
                    .map(|(block, item)| self.render_block(block, item, grid_start, cx)),
            );
        body = body.child(block_area);

        if let Some(now_minutes) = self.now_line_minutes(config, date)
            && (grid_start..=grid_end).contains(&now_minutes)
        {
            let accent = colors.text_accent;
            body = body
                .child(
                    div()
                        .absolute()
                        .top(offset(now_minutes) - px(1.0))
                        .left(px(GUTTER_WIDTH - 2.0))
                        .right_0()
                        .h(px(2.0))
                        .bg(accent),
                )
                .child(
                    div()
                        .absolute()
                        .top(offset(now_minutes) - px(3.0))
                        .left(px(GUTTER_WIDTH - 5.0))
                        .size(px(6.0))
                        .rounded_full()
                        .bg(accent),
                );
        }

        div()
            .id("thock-day-planner-grid")
            .flex_1()
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }

    /// Minutes since midnight for the "now" line, when it should be drawn:
    /// only on today's note, and only when enabled (spec §7.4).
    fn now_line_minutes(
        &self,
        config: &day_plan::DayPlannerConfig,
        date: NaiveDate,
    ) -> Option<u32> {
        if !config.show_now_indicator {
            return None;
        }
        let now = Local::now();
        (now.date_naive() == date).then(|| now.hour() * 60 + now.minute())
    }

    fn render_block(
        &self,
        block: &PlacedBlock,
        item: &PlanItem,
        grid_start: u32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let accent = colors.text_accent;
        let item_index = block.item_index;
        let selected = self.selected_item == Some(item_index);
        let top =
            px(block.start_min.saturating_sub(grid_start) as f32 / 60.0 * HOUR_HEIGHT);
        let height = px(
            ((block.end_min - block.start_min) as f32 / 60.0 * HOUR_HEIGHT).max(MIN_BLOCK_PX),
        );
        let width = 1.0 / block.column_count as f32;
        let left = block.column as f32 * width;
        let status = item.weight == day_plan::ItemWeight::Status;
        // The label wins over the time caption when the block is too short
        // for both: the caption is dropped unless it fits alongside at least
        // one line of label text (blocks with no label keep the caption).
        // The status lane is too narrow for a caption at any height.
        let has_label = !item.label.is_empty();
        let show_caption = !status
            && (!has_label || f32::from(height) >= BLOCK_CAPTION_PX + BLOCK_LABEL_LINE_PX);
        // Lines of wrapped label text that fit in the remaining height, so
        // the last visible line gets an ellipsis instead of a hard clip.
        let label_height = if show_caption {
            f32::from(height) - BLOCK_CAPTION_PX
        } else {
            f32::from(height)
        };
        let label_lines = ((label_height / BLOCK_LABEL_LINE_PX).floor() as usize).max(1);
        // Sectioned items take their subsection's hue with the exact alpha
        // treatment root items get from the accent, so visual weight is
        // unchanged; a finished item stays muted regardless (spec v8 §11.3).
        let base = self.item_section_color(item, cx).unwrap_or(accent);
        let state = ItemState::of(item);
        let (fill, border) = match state {
            // A status block is background, not foreground: it never takes a
            // section hue, however it is filed in the note.
            _ if status => (colors.text_muted.opacity(0.06), colors.border_variant),
            ItemState::Open => (base.opacity(0.15), base.opacity(0.4)),
            ItemState::Done => (colors.text_muted.opacity(0.08), colors.border_variant),
            ItemState::Struck => (colors.text_disabled.opacity(0.08), colors.border_variant),
        };
        let caption = format!(
            "{} – {}",
            format_minutes(block.start_min),
            format_minutes(block.end_min)
        );

        let label = (!item.label.is_empty()).then(|| {
            // `LabelLike::line_clamp` supplies the "…" affix; `line_clamp`
            // alone silently drops overflowing lines.
            let label = LabelLike::new()
                .size(if status {
                    LabelSize::XSmall
                } else {
                    LabelSize::Small
                })
                .line_clamp(label_lines);
            let label = match (state.finished_color(), status) {
                (Some(color), _) => label.strikethrough().color(color),
                (None, true) => label.color(Color::Muted),
                (None, false) => label,
            };
            label.child(self.render_item_label(
                ElementId::Name(format!("thock-day-planner-block-text-{item_index}").into()),
                item,
                cx,
            ))
        });

        div()
            .absolute()
            .top(top)
            .left(relative(left))
            .w(relative(width))
            .h(height)
            .px(px(1.0))
            .child(
                v_flex()
                    .id(("thock-day-planner-block", item_index))
                    .size_full()
                    .rounded_sm()
                    .overflow_hidden()
                    .bg(fill)
                    .border_1()
                    .border_color(if selected { base } else { border })
                    .px_1()
                    .cursor_pointer()
                    .when(show_caption, |this| {
                        this.child(
                            Label::new(caption)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                    })
                    .children(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal_item(item_index, window, cx);
                    })),
            )
            .into_any_element()
    }
}

fn format_minutes(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn format_ago(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0 => "just now".to_string(),
        1..=59 => format!("{minutes}m ago"),
        _ => format!("{}h ago", minutes / 60),
    }
}

impl Render for DayPlannerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ThockDayPlannerPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .children(self.render_status_row(cx))
            .child(self.render_planner(cx))
    }
}

impl EventEmitter<PanelEvent> for DayPlannerPanel {}

impl Focusable for DayPlannerPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for DayPlannerPanel {
    fn persistent_name() -> &'static str {
        "Thock Day Planner Panel"
    }

    fn panel_key() -> &'static str {
        DAY_PLANNER_PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
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
        px(320.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::ListTodo)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Day Planner Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        ToggleDayPlannerFocus.boxed_clone()
    }

    fn activation_priority(&self) -> u32 {
        // Must be unique across all panels; 0-7 are taken (0-3 and 5-7
        // upstream, 4 by the Timeline panel).
        8
    }
}
