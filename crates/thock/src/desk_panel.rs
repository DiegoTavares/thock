//! The Desk panel (V43 §8): the right-dock home of the Backlog, Today and
//! Done views, with a card for today's next timed entry above whichever is
//! showing. The Backlog and Day Planner views keep their models, parsing,
//! writes and keys; the Desk only lays them out and switches between them.

use crate::backlog_panel::{
    AddBacklogTask, CompleteBacklogTask, MoveBacklogTaskRight, MoveBacklogTaskToToday,
    RevealBacklogTask,
};
use crate::backlog_panel::{BacklogFolds, BacklogPanel, BacklogView, ToggleBacklogFocus};
use crate::day_plan::{DayPlan, ItemTiming, ItemWeight, parse_day_plan};
use crate::day_planner_panel::{DayPlannerPanel, ToggleDayPlannerFocus};
use crate::notes::NoteKind;
use crate::vault::{Vault, VaultStatus};
use anyhow::Result;
use chrono::{Local, NaiveDate, NaiveDateTime, Timelike as _};
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, KeyContext, Pixels, Subscription, Task, WeakEntity, Window, actions,
    div, px, rems,
};
use language::{Buffer, BufferEvent};
use menu::{Confirm, SelectPrevious};
use project::Project;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use ui::prelude::*;
use ui::{KeyBinding, Label, LabelSize};
use util::ResultExt as _;
use workspace::Workspace;
use workspace::dock::{DockPosition, Panel, PanelEvent};

actions!(
    thock,
    [
        /// Opens the Desk panel, or returns to the note when it is focused.
        ToggleDeskFocus,
        /// Shows the next Desk view (Backlog → Today → Done).
        ActivateNextDeskTab,
        /// Shows the previous Desk view (Done → Today → Backlog).
        ActivatePreviousDeskTab
    ]
);

/// Shows one of the Desk's views, for keybindings:
/// `["thock::ActivateDeskTab", { "tab": "today" }]`.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = thock)]
#[serde(deny_unknown_fields)]
pub struct ActivateDeskTab {
    pub tab: DeskTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DeskTab {
    Backlog,
    Today,
    Done,
}

impl DeskTab {
    const ALL: [DeskTab; 3] = [Self::Backlog, Self::Today, Self::Done];

    fn label(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Today => "Today",
            Self::Done => "Done",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Backlog => Self::Today,
            Self::Today => Self::Done,
            Self::Done => Self::Backlog,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Backlog => Self::Done,
            Self::Today => Self::Backlog,
            Self::Done => Self::Today,
        }
    }
}

/// How far ahead the Next card looks (§8.2).
const NEXT_WINDOW_MINUTES: u32 = 3 * 60;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleDeskFocus, window, cx| {
            workspace.toggle_panel_focus::<DeskPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &ToggleBacklogFocus, window, cx| {
            focus_tab(workspace, DeskTab::Backlog, window, cx);
        });
        workspace.register_action(|workspace, _: &ToggleDayPlannerFocus, window, cx| {
            focus_tab(workspace, DeskTab::Today, window, cx);
        });
        workspace.register_action(|workspace, action: &ActivateDeskTab, window, cx| {
            activate_tab(workspace, action.tab, window, cx);
        });
        workspace.register_action(|workspace, _: &ActivateNextDeskTab, window, cx| {
            if let Some(desk) = workspace.panel::<DeskPanel>(cx) {
                let tab = desk.read(cx).tab.next();
                activate_tab(workspace, tab, window, cx);
            }
        });
        workspace.register_action(|workspace, _: &ActivatePreviousDeskTab, window, cx| {
            if let Some(desk) = workspace.panel::<DeskPanel>(cx) {
                let tab = desk.read(cx).tab.previous();
                activate_tab(workspace, tab, window, cx);
            }
        });
    })
    .detach();
}

/// Shows `tab` and focuses it; when it is already showing and focused,
/// hands focus back to the note, the way a panel's own toggle does.
fn focus_tab(
    workspace: &mut Workspace,
    tab: DeskTab,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(desk) = workspace.panel::<DeskPanel>(cx) else {
        return;
    };
    let showing =
        desk.read(cx).tab == tab && desk.read(cx).focus_handle(cx).contains_focused(window, cx);
    if showing {
        workspace.toggle_panel_focus::<DeskPanel>(window, cx);
        return;
    }
    desk.update(cx, |desk, cx| desk.show_tab(tab, cx));
    workspace.focus_panel::<DeskPanel>(window, cx);
}

/// Shows `tab`, keeping focus in the panel when it was there.
fn activate_tab(
    workspace: &mut Workspace,
    tab: DeskTab,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(desk) = workspace.panel::<DeskPanel>(cx) else {
        return;
    };
    let focused = desk.read(cx).focus_handle(cx).contains_focused(window, cx);
    desk.update(cx, |desk, cx| desk.show_tab(tab, cx));
    if focused {
        workspace.focus_panel::<DeskPanel>(window, cx);
    }
}

/// What the Next card says (§8.2), when there is something left today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextCard {
    /// `NEXT · IN 25 MIN` or `NOW · UNTIL 15:30`.
    pub label: String,
    /// `15:00 · Bi-weekly · Dev Infra Leads`.
    pub line: String,
    /// The entry's row in today's note.
    pub row: u32,
}

/// The card for today's plan at `now` (minutes since midnight): an entry
/// happening now, else the next one starting within three hours. All-day
/// and unscheduled entries, finished ones, and calendar status blocks (focus
/// time, out of office) never make the card.
pub fn next_card(plan: &DayPlan, now: u32) -> Option<NextCard> {
    let timed = plan.items.iter().filter_map(|item| match item.timing {
        ItemTiming::Timed { start_min, end_min }
            if !item.done && !item.struck && item.weight == ItemWeight::Normal =>
        {
            Some((item, start_min, end_min))
        }
        _ => None,
    });
    let line = |start: u32, label: &str| format!("{} · {label}", format_time(start));
    if let Some((item, start, end)) = timed
        .clone()
        .filter(|(_, start, end)| *start <= now && now < *end)
        .max_by_key(|(_, start, _)| *start)
    {
        return Some(NextCard {
            label: format!("NOW · UNTIL {}", format_time(end)),
            line: line(start, &item.label),
            row: item.row,
        });
    }
    let (item, start, _) = timed
        .filter(|(_, start, _)| *start > now && *start - now <= NEXT_WINDOW_MINUTES)
        .min_by_key(|(_, start, _)| *start)?;
    Some(NextCard {
        label: format!("NEXT · IN {}", format_wait(start - now)),
        line: line(start, &item.label),
        row: item.row,
    })
}

fn format_time(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn format_wait(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} MIN"),
        (h, 0) => format!("{h} H"),
        (h, m) => format!("{h} H {m} MIN"),
    }
}

/// Today's note, mirrored for the card whatever day the planner shows.
struct TodayPlan {
    date: NaiveDate,
    plan: DayPlan,
    _buffer: Entity<Buffer>,
    _subscription: Subscription,
}

pub struct DeskPanel {
    project: Entity<Project>,
    backlog: Entity<BacklogPanel>,
    planner: Entity<DayPlannerPanel>,
    tab: DeskTab,
    position: DockPosition,
    vault: Option<Vault>,
    today: Option<TodayPlan>,
    /// The open of today's note in flight, keyed by the day it is for.
    today_open: Option<(NaiveDate, Task<()>)>,
    /// The Next card is the first selectable row (§8.2): true while the
    /// cursor sits on it rather than in the view below.
    card_selected: bool,
    now: Arc<dyn Fn() -> NaiveDateTime + Send + Sync>,
    _tick: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl DeskPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        let folds = BacklogPanel::load_folds(workspace.clone(), &mut cx).await;
        workspace.update_in(&mut cx, |workspace, window, cx| {
            DeskPanel::new(workspace, folds, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        folds: BacklogFolds,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let backlog = BacklogPanel::new(workspace, folds, window, cx);
        let planner = DayPlannerPanel::new(workspace, window, cx);
        let project = workspace.project().clone();
        cx.new(|cx| {
            let mut subscriptions = Vec::new();
            subscriptions.push(cx.subscribe(
                &project,
                |this: &mut Self, _, event, cx| match event {
                    project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_) => {
                        this.refresh_vault(cx);
                    }
                    project::Event::WorktreeUpdatedEntries(..) => this.refresh_today(cx),
                    _ => {}
                },
            ));
            // A row selected in a view takes the cursor off the card.
            subscriptions.push(cx.observe(&backlog, |this, backlog, cx| {
                if backlog.read(cx).has_selection() {
                    this.card_selected = false;
                }
                cx.notify();
            }));
            subscriptions.push(cx.observe(&planner, |this, planner, cx| {
                if planner.read(cx).has_selection() {
                    this.card_selected = false;
                }
                cx.notify();
            }));
            // The card's minutes count down, and "today" moves at midnight.
            let tick = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_secs(60))
                        .await;
                    let tick = this.update(cx, |this, cx| {
                        this.refresh_today(cx);
                        cx.notify();
                    });
                    if tick.is_err() {
                        break;
                    }
                }
            });
            let mut this = Self {
                project,
                backlog,
                planner,
                tab: DeskTab::Backlog,
                position: DockPosition::Right,
                vault: None,
                today: None,
                today_open: None,
                card_selected: false,
                now: Arc::new(|| Local::now().naive_local()),
                _tick: tick,
                _subscriptions: subscriptions,
            };
            this.refresh_vault(cx);
            this
        })
    }

    pub fn planner(&self) -> &Entity<DayPlannerPanel> {
        &self.planner
    }

    pub fn backlog(&self) -> &Entity<BacklogPanel> {
        &self.backlog
    }

    pub fn tab(&self) -> DeskTab {
        self.tab
    }

    /// Replaces the clock, so tests can pick the moment.
    #[cfg(test)]
    pub(crate) fn set_now(&mut self, now: NaiveDateTime, cx: &mut Context<Self>) {
        self.now = Arc::new(move || now);
        self.refresh_today(cx);
        cx.notify();
    }

    fn now(&self) -> NaiveDateTime {
        (self.now)()
    }

    pub fn show_tab(&mut self, tab: DeskTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.backlog.update(cx, |backlog, cx| {
            backlog.set_view(
                match tab {
                    DeskTab::Done => BacklogView::Done,
                    _ => BacklogView::Open,
                },
                cx,
            )
        });
        self.card_selected = false;
        cx.notify();
    }

    fn refresh_vault(&mut self, cx: &mut Context<Self>) {
        self.vault = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .and_then(
                |worktree| match Vault::detect(&worktree.read(cx).abs_path()) {
                    VaultStatus::Valid(vault) => Some(vault),
                    _ => None,
                },
            );
        self.today = None;
        self.refresh_today(cx);
    }

    /// Keeps today's note open as a background buffer so the card reads
    /// the day the planner may not be showing; a note that does not exist
    /// yet parses as an empty day.
    fn refresh_today(&mut self, cx: &mut Context<Self>) {
        let today = self.now().date();
        let Some(vault) = &self.vault else {
            self.today = None;
            return;
        };
        if self.today.as_ref().is_some_and(|plan| plan.date == today) {
            return;
        }
        if self
            .today_open
            .as_ref()
            .is_some_and(|(date, _)| *date == today)
        {
            return;
        }
        let note_path = vault.note_path(NoteKind::Daily, today);
        let Some(project_path) = self
            .project
            .read(cx)
            .project_path_for_absolute_path(&note_path, cx)
        else {
            self.today = None;
            return;
        };
        let open_buffer = self
            .project
            .update(cx, |project, cx| project.open_buffer(project_path, cx));
        // Replacing an open for an earlier day cancels it; the guard is
        // cleared however this one ends, so a failed open is retried.
        let task = cx.spawn(async move |this, cx| {
            let buffer = open_buffer.await;
            this.update(cx, |this, cx| {
                this.today_open = None;
                let buffer = match buffer {
                    Ok(buffer) => buffer,
                    Err(error) => {
                        log::error!("failed to open today's note {note_path:?}: {error:#}");
                        return;
                    }
                };
                let subscription =
                    cx.subscribe(&buffer, |this, buffer, event: &BufferEvent, cx| {
                        if matches!(event, BufferEvent::Edited { .. } | BufferEvent::Reloaded) {
                            this.reparse_today(&buffer, cx);
                        }
                    });
                this.today = Some(TodayPlan {
                    date: today,
                    plan: DayPlan::default(),
                    _buffer: buffer.clone(),
                    _subscription: subscription,
                });
                this.reparse_today(&buffer, cx);
            })
            .log_err();
        });
        self.today_open = Some((today, task));
    }

    fn reparse_today(&mut self, buffer: &Entity<Buffer>, cx: &mut Context<Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let plan = parse_day_plan(&buffer.read(cx).text(), &vault.config.day_planner);
        if let Some(today) = &mut self.today
            && today.plan != plan
        {
            today.plan = plan;
            cx.notify();
        }
    }

    fn card(&self) -> Option<NextCard> {
        let now = self.now();
        let minutes = now.hour() * 60 + now.minute();
        next_card(&self.today.as_ref()?.plan, minutes)
    }

    /// Up from a view's first row lands here only when the view released
    /// the selection, which it does when there is a card to land on.
    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if self.card().is_some() {
            self.card_selected = true;
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if !self.card_selected {
            return;
        }
        self.reveal_card(window, cx);
    }

    fn reveal_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.card() else {
            return;
        };
        self.planner.update(cx, |planner, cx| {
            planner.reveal_today_row(card.row, window, cx);
        });
    }

    fn render_tabs(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let counts = [
            Some(self.backlog.read(cx).open_count()),
            Some(self.planner.read(cx).open_count()),
            None,
        ];
        h_flex()
            .pt_3()
            .px_3()
            .gap(px(18.))
            .border_b_1()
            .border_color(colors.border_variant)
            .children(DeskTab::ALL.into_iter().zip(counts).map(|(tab, count)| {
                let active = tab == self.tab;
                h_flex()
                    .id(SharedString::from(format!(
                        "thock-desk-tab-{}",
                        tab.label()
                    )))
                    .gap_1()
                    .pb_2()
                    .items_baseline()
                    .border_b_2()
                    .border_color(if active {
                        colors.text_accent
                    } else {
                        gpui::transparent_black()
                    })
                    .cursor_pointer()
                    .child(
                        div()
                            .text_size(rems(0.8125))
                            .when(active, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .text_color(if active {
                                colors.text
                            } else {
                                colors.text_muted
                            })
                            .child(tab.label()),
                    )
                    .when_some(count, |this, count| {
                        this.child(
                            div()
                                .text_size(rems(0.6875))
                                .text_color(colors.text_placeholder)
                                .child(SharedString::from(count.to_string())),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_tab(tab, cx);
                        this.focus_handle(cx).focus(window, cx);
                    }))
            }))
            .into_any_element()
    }

    fn render_card(&self, card: &NextCard, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        v_flex()
            .id("thock-desk-next-card")
            .mx_3()
            .mt_3()
            .px(px(10.))
            .py_2()
            .gap_0p5()
            .rounded(px(6.))
            .bg(colors.element_selected)
            .border_1()
            .border_color(if self.card_selected {
                colors.text_accent
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .child(
                div()
                    .text_size(rems(0.6875))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.text_accent)
                    .child(SharedString::from(card.label.clone())),
            )
            .child(
                div()
                    .text_size(rems(0.875))
                    .text_color(colors.text)
                    .child(SharedString::from(card.line.clone())),
            )
            .on_click(cx.listener(|this, _, window, cx| this.reveal_card(window, cx)))
            .into_any_element()
    }

    /// The key hints under the Backlog tab (§8.4): rendered from the
    /// keymap, so they show the user's actual keys.
    fn render_footer(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let focus = self.backlog.read(cx).focus_handle(cx);
        let hint = |action: &dyn Action, label: &'static str| {
            h_flex()
                .gap_1()
                .items_center()
                .child(KeyBinding::for_action_in(action, &focus, cx).size(rems(0.6875)))
                .child(
                    Label::new(label)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
        };
        h_flex()
            .flex_none()
            .h(px(28.))
            .px_3()
            .gap_3()
            .items_center()
            .bg(colors.panel_background)
            .border_t_1()
            .border_color(colors.border)
            .child(hint(&AddBacklogTask, "add"))
            .child(hint(&CompleteBacklogTask, "done"))
            .child(hint(&RevealBacklogTask, "open"))
            .child(hint(&MoveBacklogTaskRight, "to Someday"))
            .child(hint(&MoveBacklogTaskToToday, "to today"))
            .into_any_element()
    }
}

impl Focusable for DeskPanel {
    /// The showing view's handle, so focusing the panel lands where its
    /// keys apply.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.tab {
            DeskTab::Backlog | DeskTab::Done => self.backlog.read(cx).focus_handle(cx),
            DeskTab::Today => self.planner.read(cx).focus_handle(cx),
        }
    }
}

impl Render for DeskPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.card();
        let has_card = card.is_some();
        self.backlog
            .update(cx, |backlog, _| backlog.set_row_above(has_card));
        self.planner
            .update(cx, |planner, _| planner.set_row_above(has_card));
        if !has_card {
            self.card_selected = false;
        }
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("ThockDeskPanel");
        v_flex()
            .key_context(key_context)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .size_full()
            .child(self.render_tabs(cx))
            .children(card.as_ref().map(|card| self.render_card(card, cx)))
            // A flex column with clipping, so the view's `size_full` resolves
            // against this slot and a long list scrolls inside it instead of
            // running under the footer.
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(match self.tab {
                        DeskTab::Backlog | DeskTab::Done => self.backlog.clone().into_any_element(),
                        DeskTab::Today => self.planner.clone().into_any_element(),
                    }),
            )
            .when(self.tab == DeskTab::Backlog, |this| {
                this.child(self.render_footer(cx))
            })
    }
}

impl EventEmitter<PanelEvent> for DeskPanel {}

impl Panel for DeskPanel {
    fn persistent_name() -> &'static str {
        "Thock Desk Panel"
    }

    fn panel_key() -> &'static str {
        "ThockDeskPanel"
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
        Some("Desk Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        ToggleDeskFocus.boxed_clone()
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        true
    }

    fn activation_priority(&self) -> u32 {
        // Must be unique across all panels; 0-9 are taken (0-3 and 5-7
        // upstream, 4 Timeline, 9 Agent) and 10 was the Backlog's. 8, the
        // Day Planner's, is free.
        10
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::day_plan::DayPlannerConfig;
    use editor::Editor;
    use fs::{FakeFs, Fs as _};
    use gpui::{KeyBinding, TestAppContext, VisualTestContext, px, size};
    use menu::{SelectNext, SelectPrevious};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use std::path::PathBuf;
    use text::Point;
    use workspace::MultiWorkspace;

    fn parse(text: &str) -> DayPlan {
        parse_day_plan(text, &DayPlannerConfig::default())
    }

    const TODAY_NOTE: &str = "# Day planner\n\
        - [ ] Workout\n\
        - [ ] 09:00 – 11:00 Misc\n\
        - [x] 11:40 – 12:00 Standup meeting\n\
        - [ ] 15:00 – 15:30 Bi-weekly · Dev Infra Leads\n";

    #[test]
    fn the_card_counts_down_to_the_next_entry() {
        let plan = parse(TODAY_NOTE);
        let card = next_card(&plan, 14 * 60 + 35).unwrap();
        assert_eq!(card.label, "NEXT · IN 25 MIN");
        assert_eq!(card.line, "15:00 · Bi-weekly · Dev Infra Leads");
        assert_eq!(card.row, 4);
        assert_eq!(
            next_card(&plan, 12 * 60 + 30).unwrap().label,
            "NEXT · IN 2 H 30 MIN"
        );
        assert_eq!(next_card(&plan, 13 * 60).unwrap().label, "NEXT · IN 2 H");
    }

    #[test]
    fn the_card_shows_what_is_happening_now() {
        let plan = parse(TODAY_NOTE);
        let card = next_card(&plan, 10 * 60).unwrap();
        assert_eq!(card.label, "NOW · UNTIL 11:00");
        assert_eq!(card.line, "09:00 · Misc");
    }

    #[test]
    fn the_card_hides_when_nothing_timed_is_left() {
        let plan = parse(TODAY_NOTE);
        assert_eq!(next_card(&plan, 16 * 60), None, "the day is over");
        assert_eq!(
            next_card(&plan, 11 * 60 + 45),
            None,
            "the standup is checked and 15:00 is more than three hours away"
        );
        assert_eq!(next_card(&plan, 12 * 60).unwrap().label, "NEXT · IN 3 H");
        assert_eq!(next_card(&plan, 8 * 60).unwrap().label, "NEXT · IN 1 H");
        assert_eq!(next_card(&plan, 5 * 60), None, "nothing within three hours");
        assert_eq!(
            next_card(&parse("- [ ] Workout\n"), 9 * 60),
            None,
            "an unscheduled or all-day entry shows nothing"
        );
        let mut status = parse("- [ ] 09:00 – 17:00 Focus time\n");
        status.items[0].weight = ItemWeight::Status;
        assert_eq!(
            next_card(&status, 10 * 60),
            None,
            "a status block is not an event"
        );
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
            let bindings = keymap_bindings(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            );
            cx.bind_keys(bindings);
            if vim {
                let bindings =
                    keymap_bindings(include_str!("../../../assets/keymaps/vim.json"), cx);
                cx.bind_keys(bindings);
                // `vim::MenuSelectNext` lives in the vim crate, which this
                // crate doesn't link; it dispatches `menu::SelectNext`.
                cx.bind_keys([
                    KeyBinding::new("j", SelectNext, Some("ThockBacklogPanel && !editing")),
                    KeyBinding::new("k", SelectPrevious, Some("ThockBacklogPanel && !editing")),
                ]);
            }
            init(cx);
        });
    }

    const BACKLOG: &str = "## Soon\n\n\
        - [ ] Study for DNV elections [[2026-09-28-dnv-ballot]]\n\
        - [ ] Apps to try: vorssaint\n\n\
        ### SPI\n\n\
        - [ ] Refactor Maestro.java\n\n\
        ## Someday\n\n\
        - [ ] Learn Zig\n\n\
        ## Completed\n\n\
        - [x] Old thing (2026-10-01)\n";

    struct Setup {
        workspace: Entity<Workspace>,
        desk: Entity<DeskPanel>,
        fs: Arc<FakeFs>,
        root: PathBuf,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    /// A vault with a backlog and today's note, the Desk docked and
    /// focused on its Backlog tab, and the clock at 14:35 today.
    async fn setup(cx: &mut TestAppContext, vim: bool) -> Setup {
        init_test(cx, vim);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        // `backlog.md` must exist on disk for the view to adopt its buffer.
        std::fs::write(root.join("backlog.md"), BACKLOG).unwrap();
        let today = Local::now().date_naive();
        let today_name = format!("{}.md", today.format("%Y-%m-%d"));
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({
                ".thock": { "config.toml": "schema = 1\n" },
                "backlog.md": BACKLOG,
                "daily": {},
                "inbox": { "2026-09-28-dnv-ballot.md": "# Ballot\n" },
            }),
        )
        .await;
        fs.insert_file(
            &root.join("daily").join(&today_name),
            TODAY_NOTE.as_bytes().to_vec(),
        )
        .await;
        let project = Project::test(fs.clone(), [root.as_path()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        let desk = workspace.update_in(&mut cx, |workspace, window, cx| {
            let desk = DeskPanel::new(workspace, BacklogFolds::default(), window, cx);
            workspace.add_panel(desk.clone(), window, cx);
            desk
        });
        cx.run_until_parked();
        let now = today.and_hms_opt(14, 35, 0).unwrap();
        desk.update(&mut cx, |desk, cx| desk.set_now(now, cx));
        cx.run_until_parked();
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.toggle_panel_focus::<DeskPanel>(window, cx);
        });
        cx.run_until_parked();
        Setup {
            workspace,
            desk,
            fs,
            root,
            cx,
            _vault_dir: vault_dir,
        }
    }

    fn tab(setup: &Setup) -> DeskTab {
        setup.desk.read_with(&setup.cx, |desk, _| desk.tab)
    }

    /// The selected row as the view labels it: `(section, index)`.
    fn selected(setup: &Setup) -> Option<(&'static str, usize)> {
        setup.desk.read_with(&setup.cx, |desk, cx| {
            desk.backlog.read(cx).selected_for_tests()
        })
    }

    fn card_selected(setup: &Setup) -> bool {
        setup
            .desk
            .read_with(&setup.cx, |desk, _| desk.card_selected)
    }

    fn desk_focused(setup: &mut Setup) -> bool {
        let desk = setup.desk.clone();
        setup
            .cx
            .update(|window, cx| desk.read(cx).focus_handle(cx).contains_focused(window, cx))
    }

    #[gpui::test]
    async fn digits_switch_the_views(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        assert!(desk_focused(&mut setup));
        assert_eq!(tab(&setup), DeskTab::Backlog);
        setup.cx.simulate_keystrokes("2");
        assert_eq!(tab(&setup), DeskTab::Today);
        assert!(desk_focused(&mut setup), "focus follows the view");
        setup.cx.simulate_keystrokes("3");
        assert_eq!(tab(&setup), DeskTab::Done);
        setup.cx.simulate_keystrokes("1");
        assert_eq!(tab(&setup), DeskTab::Backlog);
    }

    #[gpui::test]
    async fn vim_cycles_the_views_with_g_t(cx: &mut TestAppContext) {
        let mut setup = setup(cx, true).await;
        setup.cx.simulate_keystrokes("g t");
        assert_eq!(tab(&setup), DeskTab::Today);
        setup.cx.simulate_keystrokes("g t");
        assert_eq!(tab(&setup), DeskTab::Done);
        setup.cx.simulate_keystrokes("g t");
        assert_eq!(tab(&setup), DeskTab::Backlog);
        setup.cx.simulate_keystrokes("g shift-t");
        assert_eq!(tab(&setup), DeskTab::Done);
    }

    #[gpui::test]
    async fn the_focus_chords_pick_a_view(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("ctrl-alt-p");
        setup.cx.run_until_parked();
        assert_eq!(tab(&setup), DeskTab::Today);
        assert!(desk_focused(&mut setup));
        setup.cx.simulate_keystrokes("ctrl-alt-u");
        setup.cx.run_until_parked();
        assert_eq!(tab(&setup), DeskTab::Backlog);
        assert!(desk_focused(&mut setup));
        setup.cx.simulate_keystrokes("ctrl-alt-u");
        setup.cx.run_until_parked();
        assert!(
            !desk_focused(&mut setup),
            "the same chord again returns to the note"
        );
    }

    #[gpui::test]
    async fn the_cursor_walks_the_stacked_lanes(cx: &mut TestAppContext) {
        let mut setup = setup(cx, true).await;
        setup.cx.simulate_keystrokes("j");
        assert_eq!(selected(&setup), Some(("soon", 0)), "the Soon title");
        setup.cx.simulate_keystrokes("j j j");
        assert_eq!(selected(&setup), Some(("soon", 3)), "the SPI caption");
        setup.cx.simulate_keystrokes("j j");
        assert_eq!(
            selected(&setup),
            Some(("someday", 0)),
            "down into the folded Someday title"
        );
        setup.cx.simulate_keystrokes("j");
        assert_eq!(
            selected(&setup),
            Some(("someday", 0)),
            "a folded lane is one row"
        );
        setup.cx.simulate_keystrokes("l");
        setup.cx.simulate_keystrokes("j");
        assert_eq!(
            selected(&setup),
            Some(("someday", 1)),
            "unfolded, the lane has its task"
        );
        setup.cx.simulate_keystrokes("[");
        assert_eq!(
            selected(&setup),
            Some(("soon", 0)),
            "[ jumps to the previous lane title"
        );
        setup.cx.simulate_keystrokes("]");
        assert_eq!(selected(&setup), Some(("someday", 0)));
        setup.cx.simulate_keystrokes("h");
        setup.cx.simulate_keystrokes("j");
        assert_eq!(
            selected(&setup),
            Some(("someday", 0)),
            "h folds the lane again"
        );
        setup.cx.simulate_keystrokes("k k");
        assert_eq!(
            selected(&setup),
            Some(("soon", 3)),
            "up crosses back into Soon's last row"
        );
    }

    #[gpui::test]
    async fn done_lists_the_completed_tasks(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("3");
        setup.cx.simulate_keystrokes("down");
        assert_eq!(selected(&setup), Some(("completed", 0)));
        setup.cx.simulate_keystrokes("down");
        assert_eq!(
            selected(&setup),
            Some(("completed", 0)),
            "one completed task"
        );
    }

    #[gpui::test]
    async fn marking_done_moves_the_task_to_done(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("down down");
        assert_eq!(selected(&setup), Some(("soon", 1)));
        setup.cx.simulate_keystrokes("x");
        setup.cx.run_until_parked();
        let open = setup
            .desk
            .read_with(&setup.cx, |desk, cx| desk.backlog.read(cx).open_count());
        assert_eq!(open, 3);
        let backlog = setup.fs.load(&setup.root.join("backlog.md")).await.unwrap();
        assert!(backlog.contains("- [x] Study for DNV elections"));
    }

    #[gpui::test]
    async fn the_selection_survives_a_change_on_disk(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        setup.cx.simulate_keystrokes("down down down");
        assert_eq!(selected(&setup), Some(("soon", 2)));
        setup
            .fs
            .save(
                &setup.root.join("backlog.md"),
                &BACKLOG
                    .replace("## Soon\n\n", "## Soon\n\n- [ ] Inserted first\n")
                    .into(),
                Default::default(),
            )
            .await
            .unwrap();
        setup.cx.run_until_parked();
        setup
            .cx
            .executor()
            .advance_clock(Duration::from_millis(500));
        setup.cx.run_until_parked();
        assert_eq!(
            selected(&setup),
            Some(("soon", 2)),
            "the cursor keeps its row"
        );
        let open = setup
            .desk
            .read_with(&setup.cx, |desk, cx| desk.backlog.read(cx).open_count());
        assert_eq!(open, 5, "the disk change was read");
    }

    #[gpui::test]
    async fn adding_a_task_scrolls_the_new_row_into_view(cx: &mut TestAppContext) {
        let mut setup = setup(cx, true).await;
        // A Soon lane far taller than the window, so its end starts out of view.
        let tasks: String = (0..40)
            .map(|index| format!("- [ ] Task {index}\n"))
            .collect();
        setup
            .fs
            .save(
                &setup.root.join("backlog.md"),
                &BACKLOG
                    .replace("## Soon\n\n", &format!("## Soon\n\n{tasks}"))
                    .into(),
                Default::default(),
            )
            .await
            .unwrap();
        setup.cx.run_until_parked();
        setup
            .cx
            .executor()
            .advance_clock(Duration::from_millis(500));
        setup.cx.run_until_parked();
        setup.cx.simulate_resize(size(px(1000.), px(400.)));
        setup.cx.run_until_parked();
        setup.cx.simulate_keystrokes("j o");
        setup.cx.run_until_parked();
        let (in_view, scrolled) = setup
            .desk
            .read_with(&setup.cx, |desk, cx| {
                desk.backlog.read(cx).new_task_row_in_view_for_tests()
            })
            .expect("the new-task editor is open and drawn");
        assert!(in_view, "the editor row is scrolled into the viewport");
        assert!(scrolled < px(0.), "the list had to scroll to show it");
    }

    #[gpui::test]
    async fn up_from_the_top_lands_on_the_card_and_enter_reveals_it(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        let card = setup.desk.read_with(&setup.cx, |desk, _| desk.card());
        assert_eq!(
            card.as_ref().map(|card| card.label.as_str()),
            Some("NEXT · IN 25 MIN")
        );
        setup.cx.simulate_keystrokes("down");
        assert_eq!(selected(&setup), Some(("soon", 0)));
        setup.cx.simulate_keystrokes("up");
        assert_eq!(selected(&setup), None);
        assert!(card_selected(&setup));
        setup.cx.simulate_keystrokes("down");
        assert!(!card_selected(&setup), "down returns to the lanes");
        setup.cx.simulate_keystrokes("up");
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        let cursor_row = setup
            .workspace
            .update_in(&mut setup.cx, |workspace, _, cx| {
                let editor = workspace
                    .active_item_as::<Editor>(cx)
                    .expect("today's note opened");
                editor.update(cx, |editor, cx| {
                    let snapshot = editor.display_snapshot(cx);
                    editor.selections.newest::<Point>(&snapshot).head().row
                })
            });
        assert_eq!(cursor_row, 4, "the cursor sits on the entry's line");
    }

    #[gpui::test]
    async fn a_failed_open_of_today_is_retried(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        let tomorrow = Local::now().date_naive().succ_opt().unwrap();
        let note_path = setup
            .root
            .join("daily")
            .join(format!("{}.md", tomorrow.format("%Y-%m-%d")));
        // A folder where the note should be makes the open fail.
        setup.fs.create_dir(&note_path).await.unwrap();
        setup.cx.run_until_parked();
        setup.desk.update(&mut setup.cx, |desk, cx| {
            let now = tomorrow.and_hms_opt(14, 35, 0).unwrap();
            desk.now = Arc::new(move || now);
            desk.refresh_vault(cx);
        });
        setup.cx.run_until_parked();
        setup.desk.read_with(&setup.cx, |desk, _| {
            assert!(desk.today.is_none(), "the open failed");
            assert!(desk.today_open.is_none(), "nothing is left in flight");
        });

        setup
            .fs
            .remove_dir(&note_path, Default::default())
            .await
            .unwrap();
        setup
            .fs
            .insert_file(&note_path, TODAY_NOTE.as_bytes().to_vec())
            .await;
        setup.cx.run_until_parked();
        setup.cx.executor().advance_clock(Duration::from_secs(60));
        setup.cx.run_until_parked();
        let card = setup.desk.read_with(&setup.cx, |desk, _| desk.card());
        assert_eq!(
            card.map(|card| card.label),
            Some("NEXT · IN 25 MIN".to_string()),
            "a later refresh opens the note"
        );
    }

    #[gpui::test]
    async fn escape_returns_to_the_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false).await;
        let project_path = setup.workspace.read_with(&setup.cx, |workspace, cx| {
            workspace
                .project()
                .read(cx)
                .find_project_path(setup.root.join("backlog.md"), cx)
                .unwrap()
        });
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap();
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.focus_panel::<DeskPanel>(window, cx);
            });
        setup.cx.run_until_parked();
        assert!(desk_focused(&mut setup));
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(!desk_focused(&mut setup));
    }
}
