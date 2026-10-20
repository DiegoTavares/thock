//! The masthead (V43 §6): the row under the window title that says which
//! day you are looking at. The weekday is the largest text on screen, the
//! two-week strip shows which days have a note, and the active note's path
//! sits small on the right. Moving a day back or forward is one chord or one
//! click, and browsing never creates a note for a day that already went by.

use crate::notes::NoteKind;
use crate::routines_panel::open_vault_note;
use crate::thock_theme::on_accent_color;
use crate::vault::{Vault, VaultStatus};
use chrono::{Datelike, Days, Local, NaiveDate, Weekday};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, KeyContext, MouseButton,
    Render, SharedString, Subscription, Task, WeakEntity, Window, actions, div, prelude::*, px,
    rems,
};
use menu::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::Project;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use title_bar::TitleBar;
use ui::prelude::*;
use ui::{IconButton, IconButtonShape, IconName, IconSize, Tooltip};
use util::ResultExt as _;
use util::paths::PathStyle;
use util::rel_path::RelPath;
use workspace::Workspace;
use workspace::pane::RevealInProjectPanel;

actions!(
    thock,
    [
        /// Opens the daily note before the one you're viewing, skipping days that have none.
        OpenPreviousDay,
        /// Opens the daily note after the one you're viewing, or today's, or tomorrow's.
        OpenNextDay,
        /// Moves keyboard focus to the two-week day strip under the window title.
        FocusDayStrip,
        /// Moves the day strip's selection one day back.
        SelectPreviousStripDay,
        /// Moves the day strip's selection one day forward.
        SelectNextStripDay
    ]
);

/// The window widths below which the strip shows one week, then none (§6.4).
const ONE_WEEK_BELOW: f32 = 1000.;
const NO_STRIP_BELOW: f32 = 760.;
const CELLS: usize = 14;
const MASTHEAD_HEIGHT: f32 = 72.;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        let masthead = Masthead::new(workspace, cx);
        for_action::<OpenPreviousDay>(workspace, &masthead, |masthead, window, cx| {
            masthead.open_previous_day(window, cx)
        });
        for_action::<OpenNextDay>(workspace, &masthead, |masthead, window, cx| {
            masthead.open_next_day(window, cx)
        });
        for_action::<FocusDayStrip>(workspace, &masthead, |masthead, window, cx| {
            masthead.focus_strip(window, cx)
        });
        // The title bar is created by a later observer of this same
        // workspace; attach once this cycle has run. Without one (tests),
        // the masthead stands in as the title bar item itself.
        cx.defer_in(window, move |workspace, window, cx| {
            match TitleBar::of(workspace) {
                Some(title_bar) => title_bar.update(cx, |title_bar, cx| {
                    title_bar.set_masthead(Some(masthead.into()), cx)
                }),
                None => workspace.set_titlebar_item(masthead.into(), window, cx),
            }
        });
    })
    .detach();
}

fn for_action<A: gpui::Action>(
    workspace: &mut Workspace,
    masthead: &Entity<Masthead>,
    handler: impl Fn(&mut Masthead, &mut Window, &mut Context<Masthead>) + Copy + 'static,
) {
    let masthead = masthead.clone();
    workspace.register_action(move |_, _: &A, window, cx| {
        masthead.update(cx, |masthead, cx| handler(masthead, window, cx));
    });
}

/// What the masthead is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewed {
    Day(NaiveDate),
    /// A weekly note, by the Monday of its week.
    Week(NaiveDate),
}

impl Viewed {
    /// The day navigation starts from: the viewed day, the Monday of the
    /// viewed week, or today when nothing dated is open.
    fn anchor(self) -> NaiveDate {
        match self {
            Viewed::Day(date) | Viewed::Week(date) => date,
        }
    }
}

/// One cell of the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub date: NaiveDate,
    pub has_note: bool,
    pub today: bool,
    /// Outlined: the viewed day, or every day of the viewed week.
    pub viewed: bool,
    /// Whether the cell opens on click or `enter`: a day with a note, or
    /// today and tomorrow, which may be created. Past days without a note
    /// and later days are inert.
    pub enabled: bool,
}

impl Cell {
    pub fn weekend(&self) -> bool {
        matches!(self.date.weekday(), Weekday::Sat | Weekday::Sun)
    }
}

/// Everything the masthead draws, as a value: the title lines and the
/// fourteen cells, from the viewed note, today and the days that have one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MastheadModel {
    pub title: String,
    pub date_line: String,
    pub viewed: Viewed,
    pub cells: Vec<Cell>,
}

/// The model for `viewed` (today's day when `None`), on `today`, given the
/// days that have a daily note (§6.2).
pub fn model(
    viewed: Option<Viewed>,
    today: NaiveDate,
    notes: &BTreeSet<NaiveDate>,
) -> MastheadModel {
    let viewed = viewed.unwrap_or(Viewed::Day(today));
    let strip_start = strip_start(viewed.anchor(), today);
    let tomorrow = today + Days::new(1);
    let cells = (0..CELLS as u64)
        .map(|offset| {
            let date = strip_start + Days::new(offset);
            let has_note = notes.contains(&date);
            let viewed = match viewed {
                Viewed::Day(day) => date == day,
                Viewed::Week(monday) => date >= monday && date < monday + Days::new(7),
            };
            Cell {
                date,
                has_note,
                today: date == today,
                viewed,
                enabled: has_note || date == today || date == tomorrow,
            }
        })
        .collect();
    let (title, date_line) = match viewed {
        Viewed::Day(date) => (
            weekday_name(date).to_string(),
            format!(
                "{} {} {} · WEEK {}",
                date.day(),
                month_name(date).to_uppercase(),
                date.year(),
                date.iso_week().week()
            ),
        ),
        Viewed::Week(monday) => {
            let sunday = monday + Days::new(6);
            let span = if monday.year() != sunday.year() {
                format!(
                    "{} {} {} – {} {} {}",
                    monday.day(),
                    month_name(monday).to_uppercase(),
                    monday.year(),
                    sunday.day(),
                    month_name(sunday).to_uppercase(),
                    sunday.year()
                )
            } else if monday.month() != sunday.month() {
                format!(
                    "{} {} – {} {} {}",
                    monday.day(),
                    month_name(monday).to_uppercase(),
                    sunday.day(),
                    month_name(sunday).to_uppercase(),
                    sunday.year()
                )
            } else {
                format!(
                    "{} – {} {} {}",
                    monday.day(),
                    sunday.day(),
                    month_name(monday).to_uppercase(),
                    monday.year()
                )
            };
            (format!("Week {}", monday.iso_week().week()), span)
        }
    };
    MastheadModel {
        title,
        date_line,
        viewed,
        cells,
    }
}

impl MastheadModel {
    /// The seven cells of the viewed week, for windows too narrow to show
    /// both weeks (§6.4).
    pub fn viewed_week(&self) -> &[Cell] {
        let week = self
            .cells
            .iter()
            .position(|cell| cell.date == monday_of(self.viewed.anchor()))
            .unwrap_or(0);
        self.cells
            .get(week..week + CELLS / 2)
            .unwrap_or(&self.cells)
    }
}

/// The Monday the strip opens on. The strip shows the viewed week and the
/// week before it, unless today falls in the week after, in which case it
/// shows the viewed week and today's, so clicking into last week never
/// pushes today out of view.
fn strip_start(viewed: NaiveDate, today: NaiveDate) -> NaiveDate {
    let week_start = monday_of(viewed);
    if monday_of(today) == week_start + Days::new(7) {
        week_start
    } else {
        week_start - Days::new(7)
    }
}

/// The note `OpenPreviousDay` opens from `viewed`: the nearest day before it
/// that has one. Empty days are skipped, so browsing never creates a note
/// for a day that already went by.
pub fn previous_note(viewed: NaiveDate, notes: &BTreeSet<NaiveDate>) -> Option<NaiveDate> {
    notes.range(..viewed).next_back().copied()
}

/// The day `OpenNextDay` opens from `viewed`: the nearest day after it with
/// a note; failing that today, when today is still ahead; failing that the
/// day after, which gets created, as `OpenTomorrow` would.
pub fn next_day(viewed: NaiveDate, today: NaiveDate, notes: &BTreeSet<NaiveDate>) -> NaiveDate {
    let after = viewed + Days::new(1);
    if let Some(next) = notes.range(after..).next() {
        return *next;
    }
    if viewed < today { today } else { after }
}

fn monday_of(date: NaiveDate) -> NaiveDate {
    date - Days::new(date.weekday().num_days_from_monday() as u64)
}

fn weekday_name(date: NaiveDate) -> &'static str {
    match date.weekday() {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn month_name(date: NaiveDate) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[date.month0() as usize]
}

/// A short day for tooltips: "Wed 14".
fn short_day(date: NaiveDate) -> String {
    format!("{} {}", &weekday_name(date)[..3], date.day())
}

pub struct Masthead {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    vault: Option<Vault>,
    /// The active item's path, absolute, when it has one.
    active_path: Option<PathBuf>,
    /// The days that have a daily note, from the vault's daily folder.
    notes: BTreeSet<NaiveDate>,
    /// The strip's keyboard selection while the strip has focus.
    strip_cursor: Option<NaiveDate>,
    today: Arc<dyn Fn() -> NaiveDate + Send + Sync>,
    _midnight_tick: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Masthead {
    pub fn new(workspace: &Workspace, cx: &mut Context<Workspace>) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = workspace.weak_handle();
        let workspace_entity = cx.entity();
        cx.new(|cx| {
            let project_subscription =
                cx.subscribe(&project, |this: &mut Self, _, event, cx| match event {
                    project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_) => {
                        this.refresh_vault(cx);
                    }
                    project::Event::WorktreeUpdatedEntries(_, changes) => {
                        if this.touches_daily_folder(changes) {
                            this.refresh_notes(cx);
                        }
                    }
                    _ => {}
                });
            let workspace_subscription = cx.subscribe(
                &workspace_entity,
                |this: &mut Self, _, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::ActiveItemChanged) {
                        this.refresh_active_path(cx);
                    }
                },
            );
            // "Today" moves at midnight while the app stays open.
            let midnight_tick = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_secs(60))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            });
            let mut this = Self {
                workspace: weak_workspace,
                project,
                focus_handle: cx.focus_handle(),
                vault: None,
                active_path: None,
                notes: BTreeSet::new(),
                strip_cursor: None,
                today: Arc::new(|| Local::now().date_naive()),
                _midnight_tick: midnight_tick,
                _subscriptions: vec![project_subscription, workspace_subscription],
            };
            this.refresh_vault(cx);
            // The workspace is still leased by the observer constructing
            // this view; read its active item once that lease is returned.
            let masthead = cx.weak_entity();
            cx.defer(move |cx| {
                masthead
                    .update(cx, |this, cx| this.refresh_active_path(cx))
                    .log_err();
            });
            this
        })
    }

    /// Replaces the clock, so tests can pick the day.
    #[cfg(test)]
    pub(crate) fn set_today(&mut self, today: NaiveDate, cx: &mut Context<Self>) {
        self.today = Arc::new(move || today);
        cx.notify();
    }

    fn today(&self) -> NaiveDate {
        (self.today)()
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
        self.refresh_notes(cx);
    }

    fn daily_folder(&self) -> Option<Arc<RelPath>> {
        let vault = self.vault.as_ref()?;
        RelPath::new(Path::new(&vault.config.daily.dir), PathStyle::local())
            .ok()
            .map(|path| Arc::from(path.as_ref()))
    }

    fn touches_daily_folder(&self, changes: &worktree::UpdatedEntriesSet) -> bool {
        let Some(folder) = self.daily_folder() else {
            return false;
        };
        changes.iter().any(|(path, _, _)| path.starts_with(&folder))
    }

    /// Lists the daily folder once; the worktree keeps the listing current
    /// and the entry events above say when to look again.
    fn refresh_notes(&mut self, cx: &mut Context<Self>) {
        let mut notes = BTreeSet::new();
        if let (Some(vault), Some(folder)) = (&self.vault, self.daily_folder())
            && let Some(worktree) = self.project.read(cx).visible_worktrees(cx).next()
        {
            let snapshot = worktree.read(cx).snapshot();
            for entry in snapshot
                .traverse_from_path(true, false, false, &folder)
                .take_while(|entry| entry.path.starts_with(&folder))
            {
                let absolute = vault.root.join(entry.path.as_std_path());
                if let Some(date) = vault.daily_note_date(&absolute) {
                    notes.insert(date);
                }
            }
        }
        if notes != self.notes {
            self.notes = notes;
            cx.notify();
        }
    }

    fn refresh_active_path(&mut self, cx: &mut Context<Self>) {
        let active_path = self.workspace.upgrade().and_then(|workspace| {
            let workspace = workspace.read(cx);
            let project_path = workspace.active_item(cx)?.project_path(cx)?;
            self.project.read(cx).absolute_path(&project_path, cx)
        });
        if active_path != self.active_path {
            self.active_path = active_path;
            cx.notify();
        }
    }

    /// What the active note is, when it is dated.
    fn viewed(&self) -> Option<Viewed> {
        let vault = self.vault.as_ref()?;
        let path = self.active_path.as_ref()?;
        if let Some(date) = vault.daily_note_date(path) {
            return Some(Viewed::Day(date));
        }
        vault.weekly_note_week(path).map(Viewed::Week)
    }

    fn model(&self) -> MastheadModel {
        model(self.viewed(), self.today(), &self.notes)
    }

    /// The day navigation starts from: the viewed day, or today for a weekly
    /// or undated note (§6.3).
    fn navigation_anchor(&self) -> NaiveDate {
        match self.viewed() {
            Some(Viewed::Day(date)) => date,
            _ => self.today(),
        }
    }

    fn open_previous_day(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(date) = previous_note(self.navigation_anchor(), &self.notes) {
            self.open_day(date, window, cx);
        }
    }

    fn open_next_day(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let date = next_day(self.navigation_anchor(), self.today(), &self.notes);
        self.open_day(date, window, cx);
    }

    fn open_day(&mut self, date: NaiveDate, window: &mut Window, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.project.read(cx).fs().clone();
        open_vault_note(
            self.workspace.clone(),
            fs,
            vault,
            NoteKind::Daily,
            date,
            window,
            cx,
        )
        .detach_and_log_err(cx);
    }

    fn focus_strip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.vault.is_none() {
            return;
        }
        self.strip_cursor = Some(self.navigation_anchor());
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn enabled_dates(&self) -> Vec<NaiveDate> {
        self.model()
            .cells
            .iter()
            .filter(|cell| cell.enabled)
            .map(|cell| cell.date)
            .collect()
    }

    /// Moves the strip cursor by `days`, landing on the nearest enabled cell
    /// in that direction, or staying put when there is none.
    fn move_cursor(&mut self, days: i64, cx: &mut Context<Self>) {
        let Some(cursor) = self.strip_cursor else {
            return;
        };
        let enabled = self.enabled_dates();
        let target = if days < 0 {
            cursor.checked_sub_days(Days::new(days.unsigned_abs()))
        } else {
            cursor.checked_add_days(Days::new(days as u64))
        };
        let Some(target) = target else {
            return;
        };
        let landing = if days < 0 {
            enabled.iter().rev().find(|date| **date <= target)
        } else {
            enabled.iter().find(|date| **date >= target)
        };
        if let Some(landing) = landing {
            self.strip_cursor = Some(*landing);
            cx.notify();
        }
    }

    fn select_previous_day(
        &mut self,
        _: &SelectPreviousStripDay,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_cursor(-1, cx);
    }

    fn select_next_day(
        &mut self,
        _: &SelectNextStripDay,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_cursor(1, cx);
    }

    fn select_previous_week(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-7, cx);
    }

    fn select_next_week(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(7, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(first) = self.enabled_dates().first().copied() {
            self.strip_cursor = Some(first);
            cx.notify();
        }
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(last) = self.enabled_dates().last().copied() {
            self.strip_cursor = Some(last);
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(date) = self.strip_cursor {
            self.open_day(date, window, cx);
        }
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.strip_cursor = None;
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
        cx.notify();
    }

    fn reveal_active_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer_in(window, move |_, window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let pane = workspace.active_pane().clone();
                    pane.read(cx).focus_handle(cx).dispatch_action(
                        &RevealInProjectPanel::default(),
                        window,
                        cx,
                    );
                })
                .log_err();
        });
    }

    /// The active path relative to the vault, with ` / ` between folders.
    fn display_path(&self) -> Option<String> {
        let vault = self.vault.as_ref()?;
        let path = self.active_path.as_ref()?;
        let relative = path.strip_prefix(&vault.root).ok()?;
        let parts: Vec<String> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(parts.join(" / "))
    }

    fn render_nav_button(
        &self,
        id: &'static str,
        icon: IconName,
        target: Option<NaiveDate>,
        action: fn(&mut Self, &mut Window, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        div()
            .size(px(24.))
            .rounded(px(5.))
            .border_1()
            .border_color(colors.border)
            .flex()
            .items_center()
            .justify_center()
            .child(
                IconButton::new(id, icon)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .when_some(target, |this, target| {
                        this.tooltip(Tooltip::text(short_day(target)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| action(this, window, cx))),
            )
            .into_any_element()
    }

    fn render_date_block(&self, model: &MastheadModel, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let anchor = self.navigation_anchor();
        let previous = previous_note(anchor, &self.notes);
        let next = Some(next_day(anchor, self.today(), &self.notes));
        h_flex()
            .min_w(px(280.))
            .gap_4()
            .items_center()
            .child(self.render_nav_button(
                "thock-masthead-previous",
                IconName::ChevronLeft,
                previous,
                Self::open_previous_day,
                cx,
            ))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .font_family("Petrona")
                            .font_weight(FontWeight::MEDIUM)
                            .italic()
                            .text_size(rems(1.75))
                            .line_height(rems(1.75))
                            .text_color(colors.text)
                            .child(SharedString::from(model.title.clone())),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(rems(0.6875))
                            .line_height(rems(0.875))
                            .text_color(colors.text_muted)
                            .font_features(tabular_numerals())
                            .child(SharedString::from(model.date_line.clone())),
                    ),
            )
            .child(self.render_nav_button(
                "thock-masthead-next",
                IconName::ChevronRight,
                next,
                Self::open_next_day,
                cx,
            ))
            .into_any_element()
    }

    fn render_cell(&self, cell: Cell, focused: bool, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let on_accent = on_accent_color(cx);
        let date = cell.date;
        let numeral_color = if cell.today {
            on_accent
        } else if cell.has_note || cell.viewed {
            colors.text
        } else {
            colors.text_placeholder
        };
        let dot_color = if cell.today {
            on_accent
        } else {
            colors.text_placeholder
        };
        let tooltip: SharedString = if cell.enabled {
            short_day(date).into()
        } else {
            format!("No note on {}", short_day(date)).into()
        };
        let mut element = v_flex()
            .id(SharedString::from(format!("thock-strip-{date}")))
            .w(px(40.))
            .py(px(6.))
            .items_center()
            .gap_1()
            .rounded(px(6.))
            .when(cell.weekend() && !cell.today, |this| this.opacity(0.55))
            .when(cell.today, |this| this.bg(colors.text_accent))
            .when(cell.viewed && !cell.today, |this| {
                this.border_1().border_color(colors.text_accent)
            })
            .when(focused, |this| {
                this.border_2().border_color(colors.border_focused)
            })
            .tooltip(Tooltip::text(tooltip))
            .child(
                div()
                    .text_size(rems(0.625))
                    .line_height(rems(0.75))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if cell.today {
                        on_accent
                    } else {
                        colors.text_placeholder
                    })
                    .child(SharedString::from(weekday_name(date)[..1].to_string())),
            )
            .child(
                div()
                    .text_size(rems(0.9375))
                    .line_height(rems(1.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .font_features(tabular_numerals())
                    .text_color(numeral_color)
                    .child(SharedString::from(date.day().to_string())),
            )
            .child(
                div()
                    .size(px(4.))
                    .rounded_full()
                    .when(cell.has_note, |this| this.bg(dot_color)),
            );
        if cell.enabled {
            element = element
                .cursor_pointer()
                .hover(|this| this.bg(colors.ghost_element_hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_day(date, window, cx);
                }));
        }
        element.into_any_element()
    }

    fn render_strip(
        &self,
        model: &MastheadModel,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let width = window.viewport_size().width;
        if width < px(NO_STRIP_BELOW) {
            return div().flex_1().into_any_element();
        }
        let strip_focused = self.focus_handle.is_focused(window);
        let cells: &[Cell] = if width < px(ONE_WEEK_BELOW) {
            model.viewed_week()
        } else {
            &model.cells
        };
        h_flex()
            .flex_1()
            .justify_center()
            .items_center()
            .gap(px(20.))
            .children(cells.chunks(CELLS / 2).map(|week| {
                h_flex().gap(px(4.)).children(week.iter().map(|cell| {
                    let focused = strip_focused && self.strip_cursor == Some(cell.date);
                    self.render_cell(*cell, focused, cx)
                }))
            }))
            .into_any_element()
    }

    fn render_path(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let Some(path) = self.display_path() else {
            return div().min_w(px(120.)).into_any_element();
        };
        div()
            .id("thock-masthead-path")
            .min_w(px(120.))
            .max_w(px(360.))
            .flex()
            .justify_end()
            .font_family(".ZedMono")
            .text_size(rems(0.6875))
            .text_color(colors.text_placeholder)
            .cursor_pointer()
            .tooltip(Tooltip::text("Show in the file tree"))
            .on_click(cx.listener(|this, _, window, cx| this.reveal_active_path(window, cx)))
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(path)),
            )
            .into_any_element()
    }
}

/// Lining-up digits for the strip's numerals and the date line (§4.1).
fn tabular_numerals() -> gpui::FontFeatures {
    gpui::FontFeatures(Arc::new(vec![("tnum".to_string(), 1)]))
}

impl Focusable for Masthead {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Masthead {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.vault.is_none() {
            return div().into_any_element();
        }
        let colors = cx.theme().colors();
        let model = self.model();
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("ThockDayStrip");
        key_context.add("menu");
        h_flex()
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_previous_day))
            .on_action(cx.listener(Self::select_next_day))
            .on_action(cx.listener(Self::select_previous_week))
            .on_action(cx.listener(Self::select_next_week))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .w_full()
            .h(px(MASTHEAD_HEIGHT))
            .px_5()
            .gap_6()
            .items_center()
            .bg(colors.title_bar_background)
            .border_b_1()
            .border_color(colors.border)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.render_date_block(&model, cx))
            .child(self.render_strip(&model, window, cx))
            .child(self.render_path(cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor::Editor;
    use fs::FakeFs;
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use workspace::MultiWorkspace;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn notes(dates: &[NaiveDate]) -> BTreeSet<NaiveDate> {
        dates.iter().copied().collect()
    }

    const TODAY: (i32, u32, u32) = (2026, 10, 15);

    fn today() -> NaiveDate {
        date(TODAY.0, TODAY.1, TODAY.2)
    }

    /// Notes on Tue 13, Wed 14, Thu 15 and Sun 18 October 2026 (§6.3).
    fn example_notes() -> BTreeSet<NaiveDate> {
        notes(&[
            date(2026, 10, 13),
            date(2026, 10, 14),
            date(2026, 10, 15),
            date(2026, 10, 18),
        ])
    }

    #[test]
    fn a_daily_note_names_its_day_and_outlines_it() {
        let model = model(
            Some(Viewed::Day(date(2026, 10, 14))),
            today(),
            &example_notes(),
        );
        assert_eq!(model.title, "Wednesday");
        assert_eq!(model.date_line, "14 OCTOBER 2026 · WEEK 42");
        assert_eq!(model.cells.len(), 14);
        assert_eq!(
            model.cells[0].date,
            date(2026, 10, 5),
            "the strip opens on the Monday before"
        );
        assert_eq!(model.cells[13].date, date(2026, 10, 18));
        let viewed: Vec<_> = model
            .cells
            .iter()
            .filter(|cell| cell.viewed)
            .map(|cell| cell.date)
            .collect();
        assert_eq!(viewed, [date(2026, 10, 14)]);
        let today_cell = model.cells.iter().find(|cell| cell.today).unwrap();
        assert_eq!(today_cell.date, today());
        assert!(today_cell.enabled);
    }

    #[test]
    fn the_strip_keeps_today_in_view_when_looking_at_last_week() {
        let monday = date(2026, 10, 19);
        let sunday_before = model(Some(Viewed::Day(date(2026, 10, 18))), monday, &notes(&[]));
        assert_eq!(
            sunday_before.cells[0].date,
            date(2026, 10, 12),
            "the strip slides forward to the viewed week and today's"
        );
        assert_eq!(sunday_before.cells[13].date, date(2026, 10, 25));
        assert!(sunday_before.cells.iter().any(|cell| cell.today));
        assert!(sunday_before.cells.iter().any(|cell| cell.viewed));
        assert_eq!(
            sunday_before.viewed_week()[0].date,
            date(2026, 10, 12),
            "a narrow window shows the viewed week, not today's"
        );

        let same_week = model(Some(Viewed::Day(monday)), monday, &notes(&[]));
        assert_eq!(same_week.cells[0].date, date(2026, 10, 12));
        assert_eq!(same_week.viewed_week()[0].date, monday);

        let last_week = model(Some(Viewed::Week(date(2026, 10, 12))), monday, &notes(&[]));
        assert_eq!(last_week.cells[0].date, date(2026, 10, 12));
        assert!(last_week.cells.iter().any(|cell| cell.today));

        let two_weeks_back = model(Some(Viewed::Day(date(2026, 10, 11))), monday, &notes(&[]));
        assert_eq!(two_weeks_back.cells[0].date, date(2026, 9, 28));
        assert!(
            !two_weeks_back.cells.iter().any(|cell| cell.today),
            "today can't fit, so the week before the viewed one shows"
        );
    }

    #[test]
    fn cells_know_what_can_be_opened() {
        let model = model(Some(Viewed::Day(today())), today(), &example_notes());
        let by_day = |day: u32| {
            model
                .cells
                .iter()
                .find(|cell| cell.date.day() == day)
                .unwrap()
        };
        assert!(by_day(13).enabled && by_day(13).has_note);
        assert!(!by_day(12).enabled, "a past day without a note is inert");
        assert!(by_day(16).enabled, "tomorrow may be created");
        assert!(!by_day(17).enabled, "the day after tomorrow may not");
        assert!(by_day(18).enabled, "a future day with a note opens");
        assert!(by_day(18).weekend());
    }

    #[test]
    fn a_weekly_note_names_the_week_and_outlines_all_seven_days() {
        let model = model(
            Some(Viewed::Week(date(2026, 10, 12))),
            today(),
            &example_notes(),
        );
        assert_eq!(model.title, "Week 42");
        assert_eq!(model.date_line, "12 – 18 OCTOBER 2026");
        let outlined = model.cells.iter().filter(|cell| cell.viewed).count();
        assert_eq!(outlined, 7);
        assert!(model.cells[7].viewed && !model.cells[6].viewed);
    }

    #[test]
    fn an_undated_note_shows_today() {
        let model = model(None, today(), &example_notes());
        assert_eq!(model.title, "Thursday");
        assert_eq!(model.date_line, "15 OCTOBER 2026 · WEEK 42");
        let outlined: Vec<_> = model.cells.iter().filter(|cell| cell.viewed).collect();
        assert_eq!(outlined.len(), 1);
        assert!(outlined[0].today);
    }

    #[test]
    fn weeks_at_the_year_boundary() {
        let week_one = model(
            Some(Viewed::Week(date(2027, 1, 4))),
            today(),
            &BTreeSet::new(),
        );
        assert_eq!(week_one.title, "Week 1");
        assert_eq!(week_one.date_line, "4 – 10 JANUARY 2027");
        assert_eq!(week_one.cells[0].date, date(2026, 12, 28));

        let week_53 = model(
            Some(Viewed::Day(date(2026, 12, 31))),
            today(),
            &BTreeSet::new(),
        );
        assert_eq!(week_53.title, "Thursday");
        assert_eq!(week_53.date_line, "31 DECEMBER 2026 · WEEK 53");

        let across = model(
            Some(Viewed::Week(date(2026, 12, 28))),
            today(),
            &BTreeSet::new(),
        );
        assert_eq!(across.title, "Week 53");
        assert_eq!(across.date_line, "28 DECEMBER 2026 – 3 JANUARY 2027");

        let across_months = model(
            Some(Viewed::Week(date(2026, 9, 28))),
            today(),
            &BTreeSet::new(),
        );
        assert_eq!(across_months.date_line, "28 SEPTEMBER – 4 OCTOBER 2026");
    }

    #[test]
    fn previous_and_next_follow_the_examples() {
        let notes = example_notes();
        assert_eq!(
            previous_note(date(2026, 10, 15), &notes),
            Some(date(2026, 10, 14))
        );
        assert_eq!(
            previous_note(date(2026, 10, 13), &notes),
            None,
            "nothing before Tue 13 here"
        );
        let with_friday = notes.iter().copied().chain([date(2026, 10, 9)]).collect();
        assert_eq!(
            previous_note(date(2026, 10, 13), &with_friday),
            Some(date(2026, 10, 9)),
            "the empty Sat 10 to Mon 12 are skipped"
        );
        assert_eq!(
            next_day(date(2026, 10, 14), today(), &notes),
            date(2026, 10, 15)
        );
        assert_eq!(
            next_day(date(2026, 10, 15), today(), &notes),
            date(2026, 10, 18),
            "a later note wins over tomorrow"
        );
        assert_eq!(
            next_day(date(2026, 10, 15), today(), &example_notes_without(18)),
            date(2026, 10, 16),
            "from today with nothing ahead, tomorrow"
        );
        assert_eq!(
            next_day(date(2026, 10, 9), today(), &notes),
            date(2026, 10, 13),
            "the nearest later note, not today"
        );
        assert_eq!(
            next_day(date(2026, 10, 9), today(), &BTreeSet::new()),
            today(),
            "with nothing between, today"
        );
    }

    fn example_notes_without(day: u32) -> BTreeSet<NaiveDate> {
        example_notes()
            .into_iter()
            .filter(|date| date.day() != day)
            .collect()
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
                    KeyBinding::new("j", SelectNext, Some("ThockDayStrip")),
                    KeyBinding::new("k", SelectPrevious, Some("ThockDayStrip")),
                ]);
            }
            init(cx);
        });
    }

    struct Setup {
        workspace: Entity<Workspace>,
        masthead: Entity<Masthead>,
        cx: VisualTestContext,
        root: PathBuf,
        _vault_dir: tempfile::TempDir,
    }

    /// A vault with the §6.3 notes, today pinned to Thu 15 Oct 2026, and
    /// the note for `open` active in a real workspace with the masthead as
    /// its title bar item.
    async fn setup(cx: &mut TestAppContext, vim: bool, open: &str) -> Setup {
        init_test(cx, vim);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({
                ".thock": { "config.toml": "schema = 1\n" },
                "daily": {
                    "2026-10-13.md": "# Tue\n",
                    "2026-10-14.md": "# Wed\n",
                    "2026-10-15.md": "# Thu\n",
                    "2026-10-18.md": "# Sun\n",
                },
                "weekly": { "2026-W42.md": "# Week\n" },
                "backlog.md": "## Soon\n",
            }),
        )
        .await;
        let project = Project::test(fs, [root.as_path()], cx).await;
        // The workspace's key context and registered actions are rendered
        // by the multi-workspace, so the chords need the real root.
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        cx.run_until_parked();
        let masthead = workspace
            .read_with(&cx, |workspace, _| {
                workspace
                    .titlebar_item()
                    .and_then(|item| item.downcast::<Masthead>().ok())
            })
            .expect("the masthead stands in for the title bar");
        masthead.update(&mut cx, |masthead, cx| masthead.set_today(today(), cx));
        let project_path = project
            .read_with(&mut cx, |project, cx| {
                project.find_project_path(root.join(open), cx)
            })
            .unwrap();
        workspace
            .update_in(&mut cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();
        cx.run_until_parked();
        Setup {
            workspace,
            masthead,
            cx,
            root,
            _vault_dir: vault_dir,
        }
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

    fn strip_focused(setup: &mut Setup) -> bool {
        let masthead = setup.masthead.clone();
        setup
            .cx
            .update(|window, cx| masthead.read(cx).focus_handle.is_focused(window))
    }

    #[gpui::test]
    async fn the_masthead_mirrors_the_active_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false, "daily/2026-10-14.md").await;
        let model = setup
            .masthead
            .read_with(&setup.cx, |masthead, _| masthead.model());
        assert_eq!(model.title, "Wednesday");
        assert_eq!(
            model.cells.iter().filter(|cell| cell.has_note).count(),
            4,
            "the strip lists the daily folder"
        );
        let path = setup
            .masthead
            .read_with(&setup.cx, |masthead, _| masthead.display_path());
        assert_eq!(path.as_deref(), Some("daily / 2026-10-14.md"));

        let project_path = setup
            .workspace
            .read_with(&setup.cx, |workspace, cx| {
                workspace
                    .project()
                    .read(cx)
                    .find_project_path(setup.root.join("backlog.md"), cx)
            })
            .unwrap();
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap();
        setup.cx.run_until_parked();
        let model = setup
            .masthead
            .read_with(&setup.cx, |masthead, _| masthead.model());
        assert_eq!(model.title, "Thursday", "an undated note shows today");
    }

    #[gpui::test]
    async fn previous_day_opens_the_nearest_note_before(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false, "daily/2026-10-15.md").await;
        setup.cx.simulate_keystrokes("ctrl-alt-[");
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "daily/2026-10-14.md");
        setup.cx.simulate_keystrokes("ctrl-alt-[");
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "daily/2026-10-13.md");
        setup.cx.simulate_keystrokes("ctrl-alt-[");
        setup.cx.run_until_parked();
        assert_eq!(
            active_note(&mut setup),
            "daily/2026-10-13.md",
            "no earlier note, so nothing happens"
        );
    }

    #[gpui::test]
    async fn next_day_prefers_an_existing_note_over_tomorrow(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false, "daily/2026-10-14.md").await;
        setup.cx.simulate_keystrokes("ctrl-alt-]");
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "daily/2026-10-15.md");
        setup.cx.simulate_keystrokes("ctrl-alt-]");
        setup.cx.run_until_parked();
        assert_eq!(
            active_note(&mut setup),
            "daily/2026-10-18.md",
            "Sun 18 exists, so it comes before tomorrow"
        );
    }

    #[gpui::test]
    async fn navigation_starts_from_today_on_an_undated_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false, "backlog.md").await;
        setup.cx.simulate_keystrokes("ctrl-alt-[");
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "daily/2026-10-14.md");
    }

    #[gpui::test]
    async fn the_strip_walks_days_and_opens_on_enter(cx: &mut TestAppContext) {
        let mut setup = setup(cx, true, "daily/2026-10-15.md").await;
        setup.cx.simulate_keystrokes("ctrl-alt-w");
        setup.cx.run_until_parked();
        assert!(strip_focused(&mut setup));
        assert_eq!(
            setup
                .masthead
                .read_with(&setup.cx, |masthead, _| masthead.strip_cursor),
            Some(today())
        );
        setup.cx.simulate_keystrokes("h h");
        assert_eq!(
            setup
                .masthead
                .read_with(&setup.cx, |masthead, _| masthead.strip_cursor),
            Some(date(2026, 10, 13))
        );
        setup.cx.simulate_keystrokes("h");
        assert_eq!(
            setup
                .masthead
                .read_with(&setup.cx, |masthead, _| masthead.strip_cursor),
            Some(date(2026, 10, 13)),
            "disabled cells are skipped and the edge holds"
        );
        setup.cx.simulate_keystrokes("l l l l");
        assert_eq!(
            setup
                .masthead
                .read_with(&setup.cx, |masthead, _| masthead.strip_cursor),
            Some(date(2026, 10, 18)),
            "Wed, Thu, tomorrow, then the Sunday note past the inert Sat"
        );
        setup.cx.simulate_keystrokes("k");
        assert_eq!(
            setup
                .masthead
                .read_with(&setup.cx, |masthead, _| masthead.strip_cursor),
            Some(date(2026, 10, 18)),
            "no enabled cell a week up"
        );
        setup.cx.simulate_keystrokes("h h h enter");
        setup.cx.run_until_parked();
        assert_eq!(active_note(&mut setup), "daily/2026-10-14.md");
    }

    /// The deepest context binding a chord wins, so a `thock::` chord on the
    /// workspace that the editor or pane also binds (`editor::Fold` on
    /// `cmd-alt-[`) never fires while writing. The keymaps are read raw so
    /// bindings for actions this crate doesn't link still count.
    #[gpui::test]
    async fn workspace_chords_reach_past_a_focused_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false, "daily/2026-10-15.md").await;
        let context_stack = setup.cx.update(|window, _| window.context_stack());
        assert!(
            context_stack
                .iter()
                .any(|context| context.contains("Editor")),
            "the note's editor holds focus"
        );

        struct Entry {
            context: String,
            depth: Option<usize>,
            chord: String,
            keystrokes: Vec<(gpui::Modifiers, String)>,
            action: String,
        }

        let mut shadowed = Vec::new();
        for (platform, content) in [
            (
                "linux",
                include_str!("../../../assets/keymaps/default-linux.json"),
            ),
            (
                "macos",
                include_str!("../../../assets/keymaps/default-macos.json"),
            ),
        ] {
            let keymap = KeymapFile::parse(content).expect("keymap parses");
            let mut entries = Vec::new();
            for section in keymap.sections() {
                let depth = if section.context.is_empty() {
                    Some(0)
                } else {
                    gpui::KeyBindingContextPredicate::parse(&section.context)
                        .expect("context parses")
                        .depth_of(&context_stack)
                };
                for (chord, action) in section.bindings() {
                    let keystrokes = chord
                        .split_whitespace()
                        .map(|keystroke| {
                            let keystroke =
                                gpui::Keystroke::parse(keystroke).expect("chord parses");
                            (keystroke.modifiers, keystroke.key)
                        })
                        .collect();
                    entries.push(Entry {
                        context: section.context.clone(),
                        depth,
                        chord: chord.clone(),
                        keystrokes,
                        action: action.to_string(),
                    });
                }
            }
            for (index, entry) in entries.iter().enumerate() {
                if entry.context != "Workspace" || !entry.action.starts_with("thock::") {
                    continue;
                }
                // Equal depth goes to the binding defined last.
                let winner = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, other)| {
                        other.depth.is_some() && other.keystrokes == entry.keystrokes
                    })
                    .max_by_key(|(other_index, other)| (other.depth, *other_index));
                if let Some((winner_index, winner)) = winner
                    && winner_index != index
                {
                    shadowed.push(format!(
                        "{platform}: `{}` runs {} ({}) instead of {}",
                        entry.chord, winner.action, winner.context, entry.action
                    ));
                }
            }
        }
        assert!(
            shadowed.is_empty(),
            "chords shadowed inside a note:\n{}",
            shadowed.join("\n")
        );
    }

    #[gpui::test]
    async fn escape_returns_focus_to_the_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx, false, "daily/2026-10-15.md").await;
        setup.cx.simulate_keystrokes("ctrl-alt-w");
        setup.cx.run_until_parked();
        assert!(strip_focused(&mut setup));
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(!strip_focused(&mut setup));
        let focused_in_editor =
            setup
                .workspace
                .update_in(&mut setup.cx, |workspace, window, cx| {
                    workspace
                        .active_item(cx)
                        .map(|item| item.item_focus_handle(cx).contains_focused(window, cx))
                        .unwrap_or(false)
                });
        assert!(focused_in_editor);
    }
}
