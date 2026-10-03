//! The Day Planner Context panel (spec `v4-day-planner-panel.md`): a
//! right-dock panel that follows the active editor item. When that item is a
//! daily note of the current vault, its checklist is parsed into a vertical
//! day grid — timed tasks as duration-scaled blocks, unscheduled tasks as
//! chips. When no daily note is active it falls back to today's note,
//! opened as a background buffer. Read-only; the one interaction is
//! revealing an item in the editor, by click or from the keyboard.

use anyhow::Result;
use chrono::{Local, NaiveDate, Timelike as _};
use editor::{Editor, EditorEvent, RowHighlightOptions, SelectionEffects, scroll::Autoscroll};
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyContext, Pixels, ScrollHandle, Subscription, Task, WeakEntity, Window, actions, div, point,
    px, relative,
};
use language::{Buffer, BufferEvent};
use menu::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use multi_buffer::MultiBufferRow;
use project::Project;
use std::time::Duration;
use text::{Bias, Point};
use ui::prelude::*;
use ui::{Icon, IconSize, Label, LabelLike};
use util::ResultExt as _;
use workspace::Workspace;
use workspace::dock::{DockPosition, Panel, PanelEvent};

use crate::calendar_service::{self, CalendarService};
use crate::day_plan::{self, DayPlan, PlacedBlock, PlanItem, parse_day_plan};
use crate::markdown_text::render_markdown_row;
use crate::notes::{NoteKind, format_date};
use crate::sync_status;
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
    /// Panel-local UI state (spec §8): the keyboard cursor, which is also
    /// the last revealed block/chip. An index into the active plan's items.
    selected_item: Option<usize>,
    grid_scroll_handle: ScrollHandle,
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
                    cx.background_executor()
                        .timer(Duration::from_secs(60))
                        .await;
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
                grid_scroll_handle: ScrollHandle::new(),
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
                let subscription = cx.subscribe(&buffer, |this, _, event: &BufferEvent, cx| {
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
        // The same day can switch source (today's background buffer becomes
        // an open editor); only a different day loses the cursor.
        self.selected_item = match (&self.active, &active) {
            (Some(previous), Some(next)) if previous.date == next.date => {
                retained_selection(&previous.plan, self.selected_item, &next.plan)
            }
            _ => None,
        };
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
            self.selected_item = retained_selection(&active.plan, self.selected_item, &plan);
            active.plan = plan;
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

    /// Item indices in the order they are laid out, for keyboard movement.
    fn navigation_order(&self) -> Vec<usize> {
        self.active
            .as_ref()
            .map(|active| navigation_order(&active.plan))
            .unwrap_or_default()
    }

    fn select(&mut self, item_index: usize, cx: &mut Context<Self>) {
        self.selected_item = Some(item_index);
        self.scroll_selection_into_view();
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        let order = self.navigation_order();
        let position = self
            .selected_item
            .and_then(|selected| order.iter().position(|index| *index == selected));
        let next = match position {
            Some(position) => order.get(position + 1).or_else(|| order.last()),
            None => order.first(),
        };
        if let Some(&next) = next {
            self.select(next, cx);
        }
    }

    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let order = self.navigation_order();
        let position = self
            .selected_item
            .and_then(|selected| order.iter().position(|index| *index == selected));
        let previous = match position {
            Some(position) => order.get(position.saturating_sub(1)),
            None => order.last(),
        };
        if let Some(&previous) = previous {
            self.select(previous, cx);
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&first) = self.navigation_order().first() {
            self.select(first, cx);
        }
    }

    fn select_last(&mut self, _: &SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&last) = self.navigation_order().last() {
            self.select(last, cx);
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item_index) = self.selected_item {
            self.reveal_item(item_index, window, cx);
        }
    }

    /// Escape hands focus back to the note — a dock panel must never trap
    /// the keyboard.
    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Keeps a keyboard-selected block on screen; the grid is taller than
    /// the panel for most days.
    fn scroll_selection_into_view(&self) {
        let (VaultStatus::Valid(vault), Some(active), Some(selected)) =
            (&self.vault_status, &self.active, self.selected_item)
        else {
            return;
        };
        let Some(block) = day_plan::layout_blocks(&active.plan, min_visual_minutes())
            .into_iter()
            .find(|block| block.item_index == selected)
        else {
            return;
        };
        let viewport_height = self.grid_scroll_handle.bounds().size.height;
        if viewport_height <= px(0.) {
            return;
        }
        let (grid_start, _) = day_plan::grid_bounds(&active.plan, &vault.config.day_planner);
        let top = px(block.start_min.saturating_sub(grid_start) as f32 / 60.0 * HOUR_HEIGHT);
        let height =
            px(((block.end_min - block.start_min) as f32 / 60.0 * HOUR_HEIGHT).max(MIN_BLOCK_PX));
        let offset = self.grid_scroll_handle.offset();
        let scroll_top = -offset.y;
        let new_scroll_top = if top < scroll_top {
            top
        } else if top + height > scroll_top + viewport_height {
            (top + height - viewport_height).min(top)
        } else {
            return;
        };
        self.grid_scroll_handle
            .set_offset(point(offset.x, -new_scroll_top));
    }

    /// The Calendar row at the top of the panel (V32 §4.2), kept only while
    /// it needs the user; healthy status lives in the sync icon's popover.
    fn render_status_row(&self, cx: &App) -> Option<AnyElement> {
        let status = sync_status::calendar_status(self.calendar_service.as_ref()?.read(cx))?;
        status
            .needs_user()
            .then(|| sync_status::render_inline_row(&status, cx))
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
        v_flex()
            .p_3()
            .child(Label::new(text).size(LabelSize::Small).color(Color::Muted))
    }

    fn render_planner(&self, cx: &Context<Self>) -> AnyElement {
        let (VaultStatus::Valid(vault), Some(active)) = (&self.vault_status, &self.active) else {
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

    fn render_unscheduled_strip(&self, plan: &DayPlan, cx: &Context<Self>) -> Option<AnyElement> {
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
    fn render_item_label(&self, id: ElementId, item: &PlanItem, cx: &Context<Self>) -> AnyElement {
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
        let blocks = day_plan::layout_blocks(plan, min_visual_minutes());
        let total_height = (grid_end - grid_start) as f32 / 60.0 * HOUR_HEIGHT;
        let offset =
            |minutes: u32| px((minutes.saturating_sub(grid_start)) as f32 / 60.0 * HOUR_HEIGHT);

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
            .track_scroll(&self.grid_scroll_handle)
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
        let top = px(block.start_min.saturating_sub(grid_start) as f32 / 60.0 * HOUR_HEIGHT);
        let height =
            px(((block.end_min - block.start_min) as f32 / 60.0 * HOUR_HEIGHT).max(MIN_BLOCK_PX));
        let width = 1.0 / block.column_count as f32;
        let left = block.column as f32 * width;
        let status = item.weight == day_plan::ItemWeight::Status;
        // The label wins over the time caption when the block is too short
        // for both: the caption is dropped unless it fits alongside at least
        // one line of label text (blocks with no label keep the caption).
        // The status lane is too narrow for a caption at any height.
        let has_label = !item.label.is_empty();
        let show_caption =
            !status && (!has_label || f32::from(height) >= BLOCK_CAPTION_PX + BLOCK_LABEL_LINE_PX);
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

fn min_visual_minutes() -> u32 {
    (MIN_BLOCK_PX / HOUR_HEIGHT * 60.0).ceil() as u32
}

/// Item indices in the order the panel lays them out: the unscheduled strip
/// first, then the grid's blocks top to bottom, left to right within a row
/// (the status lane sits left of the day's blocks).
fn navigation_order(plan: &DayPlan) -> Vec<usize> {
    let mut blocks = day_plan::layout_blocks(plan, min_visual_minutes());
    blocks.sort_by_key(|block| {
        let lane = match plan.items.get(block.item_index).map(|item| item.weight) {
            Some(day_plan::ItemWeight::Status) => 0,
            _ => 1,
        };
        (block.start_min, lane, block.column, block.item_index)
    });
    plan.unscheduled_indices()
        .chain(blocks.into_iter().map(|block| block.item_index))
        .collect()
}

/// The selection carried across a re-parse of the same day: the item with
/// the same label nearest its old row, so an edit above it doesn't move the
/// cursor to a different task. A selection whose item vanished stays on its
/// index while that still exists, and is dropped otherwise.
fn retained_selection(
    old_plan: &DayPlan,
    selected: Option<usize>,
    new_plan: &DayPlan,
) -> Option<usize> {
    let selected = selected?;
    if let Some(old_item) = old_plan.items.get(selected)
        && let Some((index, _)) = new_plan
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.label == old_item.label)
            .min_by_key(|(_, item)| item.row.abs_diff(old_item.row))
    {
        return Some(index);
    }
    (selected < new_plan.items.len()).then_some(selected)
}

fn format_minutes(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

impl Render for DayPlannerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add(DAY_PLANNER_PANEL_KEY);
        key_context.add("menu");
        v_flex()
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
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

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};

    const NOTE: &str = "# Day planner\n\
        - [ ] Unscheduled thing\n\
        - [ ] 09:00 – 10:00 First block\n\
        - [ ] 11:00 – 12:00 Second block\n";
    const UNSCHEDULED_ROW: u32 = 1;
    const FIRST_BLOCK_ROW: u32 = 2;
    const SECOND_BLOCK_ROW: u32 = 3;

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
            // The shipped keymap, so a missing or shadowed binding fails here.
            let bindings = keymap_bindings(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            );
            cx.bind_keys(bindings);
            if vim {
                // `vim::MenuSelectNext` lives in the vim crate, which this crate
                // doesn't link; it dispatches `menu::SelectNext`, so bind that.
                // The rest of the block (`g g`, `shift-g`) loads as shipped.
                let bindings =
                    keymap_bindings(include_str!("../../../assets/keymaps/vim.json"), cx);
                cx.bind_keys(bindings);
                cx.bind_keys([
                    KeyBinding::new("j", SelectNext, Some(DAY_PLANNER_PANEL_KEY)),
                    KeyBinding::new("k", SelectPrevious, Some(DAY_PLANNER_PANEL_KEY)),
                ]);
            }
        });
    }

    struct Setup {
        panel: Entity<DayPlannerPanel>,
        editor: Entity<Editor>,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    /// A vault holding one daily note open in a real workspace, with the
    /// planner docked and focused. `Vault::detect` reads the marker from
    /// disk, so it lives in a real temp dir mirrored into the FakeFs.
    async fn setup(cx: &mut TestAppContext, vim: bool) -> Setup {
        init_test(cx, vim);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            root,
            json!({
                ".thock": { "config.toml": "schema = 1\n" },
                "daily": { "2026-01-05.md": NOTE },
            }),
        )
        .await;
        let project = Project::test(fs, [root], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let project_path = project
            .read_with(&mut cx, |project, cx| {
                project.find_project_path(root.join("daily/2026-01-05.md"), cx)
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
        let panel = workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = DayPlannerPanel::new(workspace, window, cx);
            workspace.add_panel(panel.clone(), window, cx);
            panel
        });
        cx.run_until_parked();
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.toggle_panel_focus::<DayPlannerPanel>(window, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            panel.read_with(&cx, |panel, _| panel
                .active
                .as_ref()
                .map(|active| active.plan.items.len())),
            Some(3),
            "the planner mirrors the open daily note"
        );
        Setup {
            panel,
            editor,
            cx,
            _vault_dir: vault_dir,
        }
    }

    fn selected_row(setup: &Setup) -> Option<u32> {
        setup.panel.read_with(&setup.cx, |panel, _| {
            let active = panel.active.as_ref()?;
            Some(active.plan.items.get(panel.selected_item?)?.row)
        })
    }

    fn cursor_row(setup: &mut Setup) -> u32 {
        setup.editor.update(&mut setup.cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            editor.selections.newest::<Point>(&snapshot).head().row
        })
    }

    fn panel_focused(setup: &mut Setup) -> bool {
        let panel = setup.panel.clone();
        setup
            .cx
            .update(|window, cx| panel.read(cx).focus_handle.contains_focused(window, cx))
    }

    #[test]
    fn navigation_follows_the_layout() {
        let config = day_plan::DayPlannerConfig::default();
        let plan = parse_day_plan(
            "- [ ] 11:00 – 12:00 Late\n\
             - [ ] Chip\n\
             - [ ] 09:00 – 10:00 Early\n\
             - [ ] 09:00 – 09:30 Beside early\n",
            &config,
        );
        let labels: Vec<&str> = navigation_order(&plan)
            .into_iter()
            .map(|index| plan.items[index].label.as_str())
            .collect();
        // Blocks starting together sit side by side, the shorter one left.
        assert_eq!(labels, ["Chip", "Beside early", "Early", "Late"]);
    }

    #[test]
    fn a_reparse_keeps_the_selected_task() {
        let config = day_plan::DayPlannerConfig::default();
        let before = parse_day_plan("- [ ] A\n- [ ] B\n", &config);
        let inserted_above = parse_day_plan("- [ ] New\n- [ ] A\n- [ ] B\n", &config);
        assert_eq!(
            retained_selection(&before, Some(1), &inserted_above),
            Some(2),
            "the cursor follows B down a row"
        );
        let renamed = parse_day_plan("- [ ] A\n- [ ] Bee\n", &config);
        assert_eq!(retained_selection(&before, Some(1), &renamed), Some(1));
        let removed = parse_day_plan("- [ ] A\n", &config);
        assert_eq!(retained_selection(&before, Some(1), &removed), None);
        assert_eq!(retained_selection(&before, None, &inserted_above), None);
    }

    #[gpui::test]
    async fn arrows_walk_the_day_and_enter_reveals(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        assert!(panel_focused(&mut setup));
        assert_eq!(selected_row(&setup), None);

        setup.cx.simulate_keystrokes("down");
        assert_eq!(selected_row(&setup), Some(UNSCHEDULED_ROW));
        setup.cx.simulate_keystrokes("down down down");
        assert_eq!(
            selected_row(&setup),
            Some(SECOND_BLOCK_ROW),
            "the cursor stops at the last block"
        );
        setup.cx.simulate_keystrokes("up");
        assert_eq!(selected_row(&setup), Some(FIRST_BLOCK_ROW));
        setup.cx.simulate_keystrokes("down");

        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        assert_eq!(cursor_row(&mut setup), SECOND_BLOCK_ROW);
        assert!(!panel_focused(&mut setup), "revealing moves to the note");
        assert_eq!(selected_row(&setup), Some(SECOND_BLOCK_ROW));
    }

    #[gpui::test]
    async fn up_from_nothing_starts_at_the_end(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("up");
        assert_eq!(selected_row(&setup), Some(SECOND_BLOCK_ROW));
        setup.cx.simulate_keystrokes("up up up");
        assert_eq!(selected_row(&setup), Some(UNSCHEDULED_ROW));
    }

    #[gpui::test]
    async fn vim_motions_move_the_selection(cx: &mut TestAppContext) {
        let mut setup = setup(cx, true).await;
        setup.cx.simulate_keystrokes("shift-g");
        assert_eq!(selected_row(&setup), Some(SECOND_BLOCK_ROW));
        setup.cx.simulate_keystrokes("g g");
        assert_eq!(selected_row(&setup), Some(UNSCHEDULED_ROW));
        setup.cx.simulate_keystrokes("j j");
        assert_eq!(selected_row(&setup), Some(SECOND_BLOCK_ROW));
        setup.cx.simulate_keystrokes("k");
        assert_eq!(selected_row(&setup), Some(FIRST_BLOCK_ROW));
    }

    #[gpui::test]
    async fn the_selection_survives_an_edit_to_the_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("down down");
        assert_eq!(selected_row(&setup), Some(FIRST_BLOCK_ROW));

        setup.editor.update(&mut setup.cx, |editor, cx| {
            let heading_end = Point::new(0, "# Day planner".len() as u32);
            editor.edit([(heading_end..heading_end, "\n- [ ] Added above")], cx);
        });
        setup.cx.executor().advance_clock(REPARSE_DEBOUNCE * 2);
        setup.cx.run_until_parked();

        assert_eq!(
            selected_row(&setup),
            Some(FIRST_BLOCK_ROW + 1),
            "the same block stays selected after it moved down a row"
        );
        assert!(panel_focused(&mut setup));
    }

    #[gpui::test]
    async fn escape_returns_to_the_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(!panel_focused(&mut setup));
        let editor = setup.editor.clone();
        assert!(
            setup
                .cx
                .update(|window, cx| editor.focus_handle(cx).contains_focused(window, cx))
        );
    }
}
