//! The desk's reminder model (spec `thock/specs/v45-reminders.md` §8.4, §12):
//! `.thock/reminders.toml`, the chip a token draws as, and the one wall clock
//! every reminder reader schedules on. The grammar and the line edits live in
//! `thock_sync_core`, shared with the phone.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use fs::Fs;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use serde::Deserialize;
use thock_sync_core::{
    DEFAULT_DAY_HEADING, DailyNotes, Due, Group, LineReminder, ReminderConfig, When,
    is_reminder_path,
};

use crate::notes::format_date;
use crate::vault::{VAULT_MARKER_DIR, Vault};

/// The settings file, under `.thock/`. Its own file because `config.toml`
/// rejects unknown tables and an older build would refuse the vault.
pub const REMINDERS_CONFIG_FILE: &str = "reminders.toml";

pub fn init(cx: &mut App) {
    if !cx.has_global::<ReminderClock>() {
        cx.set_global(ReminderClock::system());
    }
    ReminderDay::global(cx);
}

// ---------------------------------------------------------------------------
// Settings (§12).

/// `.thock/reminders.toml`, resolved. A missing file means `Default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderSettings {
    /// The index, relative to the vault root.
    pub index: String,
    /// Heading names, matched loosely (V26). The first of each is written.
    pub open_headings: Vec<String>,
    pub done_headings: Vec<String>,
    pub day_headings: Vec<String>,
    /// When a reminder with no time rings.
    pub date_only_at: NaiveTime,
    /// How far *In 1 hour* pushes a reminder.
    pub snooze_minutes: u32,
    /// The time of *Tomorrow 09:00* and the tab's reschedule choices.
    pub tomorrow_at: NaiveTime,
    /// Whether the desk rings at all.
    pub ring_on_desk: bool,
}

impl Default for ReminderSettings {
    fn default() -> Self {
        let nine = NaiveTime::from_hms_opt(9, 0, 0).unwrap_or(NaiveTime::MIN);
        Self {
            index: "reminders.md".into(),
            open_headings: vec!["Open".into()],
            done_headings: vec!["Done".into()],
            day_headings: vec![DEFAULT_DAY_HEADING.into()],
            date_only_at: nine,
            snooze_minutes: 60,
            tomorrow_at: nine,
            ring_on_desk: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SettingsContent {
    index: Option<String>,
    headings: HeadingsContent,
    ring: RingContent,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HeadingsContent {
    open: Option<Vec<String>>,
    done: Option<Vec<String>>,
    day: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RingContent {
    date_only_at: Option<String>,
    snooze_minutes: Option<u32>,
    tomorrow_at: Option<String>,
    desk: Option<bool>,
}

/// Parses `.thock/reminders.toml`. Every key is optional and unknown keys are
/// ignored, so a newer file still loads. A value that can't be used (a time
/// that isn't `HH:MM`, an index outside the vault) is an error to surface.
pub fn parse_reminder_settings(text: &str) -> Result<ReminderSettings> {
    let content: SettingsContent = toml::from_str(text).context("parsing reminders.toml")?;
    let defaults = ReminderSettings::default();
    let index = match content.index.map(|index| index.trim().to_string()) {
        Some(index) if !index.is_empty() => {
            if !is_vault_markdown_path(&index) {
                bail!("reminders.toml: index {index:?} is not a Markdown file inside the vault");
            }
            index
        }
        _ => defaults.index,
    };
    let headings = |names: Option<Vec<String>>, fallback: Vec<String>| {
        let names: Vec<String> = names
            .unwrap_or_default()
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        if names.is_empty() { fallback } else { names }
    };
    let time = |value: Option<String>, key: &str, fallback: NaiveTime| match value {
        None => Ok(fallback),
        Some(text) => NaiveTime::parse_from_str(text.trim(), "%H:%M")
            .with_context(|| format!("reminders.toml: [ring] {key} = {text:?} is not HH:MM")),
    };
    Ok(ReminderSettings {
        index,
        open_headings: headings(content.headings.open, defaults.open_headings),
        done_headings: headings(content.headings.done, defaults.done_headings),
        day_headings: headings(content.headings.day, defaults.day_headings),
        date_only_at: time(
            content.ring.date_only_at,
            "date_only_at",
            defaults.date_only_at,
        )?,
        snooze_minutes: content
            .ring
            .snooze_minutes
            .unwrap_or(defaults.snooze_minutes)
            .clamp(1, 24 * 60),
        tomorrow_at: time(
            content.ring.tomorrow_at,
            "tomorrow_at",
            defaults.tomorrow_at,
        )?,
        ring_on_desk: content.ring.desk.unwrap_or(defaults.ring_on_desk),
    })
}

fn is_vault_markdown_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && path.to_ascii_lowercase().ends_with(".md")
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Reads the vault's `.thock/reminders.toml` through `fs`. No file means the
/// defaults; an unreadable or invalid one is an error.
pub async fn load_reminder_settings(
    fs: &Arc<dyn Fs>,
    vault_root: &Path,
) -> Result<ReminderSettings> {
    let path = vault_root
        .join(VAULT_MARKER_DIR)
        .join(REMINDERS_CONFIG_FILE);
    if !fs.is_file(&path).await {
        return Ok(ReminderSettings::default());
    }
    let text = fs
        .load(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    parse_reminder_settings(&text)
}

impl ReminderSettings {
    /// What the sync core needs to read reminders from this vault's notes.
    pub fn reminder_config(&self, vault: &Vault) -> ReminderConfig {
        ReminderConfig {
            daily: DailyNotes {
                dir: vault.config.daily.dir.clone(),
                filename: vault.config.daily.filename.clone(),
            },
            day_headings: self.day_headings.clone(),
        }
    }

    /// When *In 1 hour* moves a reminder answered at `now`: `snooze_minutes`
    /// later, rounded up to the minute.
    pub fn snoozed_due(&self, now: NaiveDateTime) -> Due {
        let later = now + chrono::Duration::minutes(i64::from(self.snooze_minutes));
        let rounded = if later.second() == 0 && later.nanosecond() == 0 {
            later
        } else {
            later
                .with_second(0)
                .unwrap_or(later)
                .with_nanosecond(0)
                .unwrap_or(later)
                + chrono::Duration::minutes(1)
        };
        Due::new(rounded.date(), Some(rounded.time()))
    }

    /// *Tomorrow 09:00*, from `today`.
    pub fn tomorrow_due(&self, today: NaiveDate) -> Due {
        let tomorrow = today.checked_add_days(Days::new(1)).unwrap_or(today);
        Due::new(tomorrow, Some(self.tomorrow_at))
    }

    /// *Monday 09:00*: the next Monday after `today`, never `today` itself.
    pub fn monday_due(&self, today: NaiveDate) -> Due {
        let ahead = 7 - u64::from(today.weekday().num_days_from_monday());
        let monday = today.checked_add_days(Days::new(ahead)).unwrap_or(today);
        Due::new(monday, Some(self.tomorrow_at))
    }
}

/// Whether the note at `relative` (from the vault root, `/`-separated) can
/// hold reminders: Markdown outside `.thock/`, `routines/` and the folder of
/// the vault's templates (§4.2).
pub fn is_reminder_note(vault: &Vault, relative: &str) -> bool {
    is_reminder_path(
        relative,
        &[
            vault.config.daily.template.as_str(),
            vault.config.weekly.template.as_str(),
        ],
    )
}

// ---------------------------------------------------------------------------
// The chip (§8.4).

/// How a token draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChipState {
    /// Accent outline.
    Upcoming,
    /// Accent fill.
    Today,
    /// Warning colour.
    Overdue,
    /// Muted and struck, with a check.
    Closed,
    /// Dashed: a time-only token outside a daily note.
    NoDay,
}

impl ChipState {
    /// The group a view lists the reminder in, or `None` with no day.
    pub fn group(self) -> Option<Group> {
        match self {
            Self::Upcoming => Some(Group::Upcoming),
            Self::Today => Some(Group::Today),
            Self::Overdue => Some(Group::Overdue),
            Self::Closed => Some(Group::Done),
            Self::NoDay => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReminderChip {
    pub state: ChipState,
    pub label: String,
}

/// The chip a line's token draws as on `today`, or `None` when the line has
/// no token. State follows `thock_sync_core::group`, by day: a reminder due
/// today is *Today* even after its time, and *Overdue* only from the next day. Labels follow §8.4: *Tomorrow 10:00*, *Today 14:30*, *Tue 20 Oct
/// 18:00 · overdue*, *Mon 19 Oct* when closed, *17:00 · which day?*.
pub fn reminder_chip(reminder: &LineReminder, today: NaiveDate) -> Option<ReminderChip> {
    let token = reminder.token.as_ref()?;
    let Some(due) = reminder.due else {
        let label = match token.when {
            When::Time(time) => format!("{} · which day?", time.format("%H:%M")),
            When::Date(_) | When::DateTime(..) => "which day?".to_string(),
        };
        return Some(ReminderChip {
            state: ChipState::NoDay,
            label,
        });
    };
    let (state, label) = match thock_sync_core::group(reminder.state(), Some(due), today) {
        Some(Group::Done) | None => (ChipState::Closed, day_label(due.date, today)),
        Some(Group::Overdue) => (
            ChipState::Overdue,
            format!("{} · overdue", due_label(due, today)),
        ),
        Some(Group::Today) => (ChipState::Today, due_label(due, today)),
        Some(Group::Upcoming) => (ChipState::Upcoming, due_label(due, today)),
    };
    Some(ReminderChip { state, label })
}

/// *Today*, *Tomorrow* or *Yesterday*, otherwise `ddd D MMM`, with the year
/// when it isn't `today`'s.
pub fn day_label(date: NaiveDate, today: NaiveDate) -> String {
    let offset = date.signed_duration_since(today).num_days();
    match offset {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        _ if date.year() == today.year() => format_date(date, "ddd D MMM"),
        _ => format_date(date, "ddd D MMM YYYY"),
    }
}

/// `day_label` and the time, when there is one: *Tomorrow 10:00*.
pub fn due_label(due: Due, today: NaiveDate) -> String {
    let day = day_label(due.date, today);
    match due.time {
        Some(time) => format!("{day} {}", time.format("%H:%M")),
        None => day,
    }
}

// ---------------------------------------------------------------------------
// The wall clock (plan §5.3).

/// Local wall-clock time. Reminders are due at wall-clock times, so every
/// reminder reader asks this instead of `chrono::Local`, and tests can pick
/// the moment.
pub trait WallClock: Send + Sync {
    fn now(&self) -> NaiveDateTime;
}

struct SystemClock;

impl WallClock for SystemClock {
    fn now(&self) -> NaiveDateTime {
        chrono::Local::now().naive_local()
    }
}

/// The app's one wall clock, a global set by `init`.
#[derive(Clone)]
pub struct ReminderClock(Arc<dyn WallClock>);

impl Global for ReminderClock {}

impl ReminderClock {
    fn system() -> Self {
        Self(Arc::new(SystemClock))
    }

    pub fn new(clock: Arc<dyn WallClock>) -> Self {
        Self(clock)
    }

    /// The global clock; the system clock when `init` hasn't run.
    pub fn global(cx: &App) -> Self {
        cx.try_global::<Self>()
            .cloned()
            .unwrap_or_else(Self::system)
    }

    /// Replaces the global clock.
    pub fn set_global(clock: Arc<dyn WallClock>, cx: &mut App) {
        cx.set_global(Self(clock));
    }

    pub fn now(&self) -> NaiveDateTime {
        self.0.now()
    }

    pub fn today(&self) -> NaiveDate {
        self.now().date()
    }

    /// How long until the wall clock reads `target`; zero once it has.
    pub fn duration_until(&self, target: NaiveDateTime) -> Duration {
        (target - self.now()).to_std().unwrap_or(Duration::ZERO)
    }
}

/// A clock that moves with the GPUI executor's fake time, starting at a
/// chosen moment: `cx.executor().advance_clock(...)` moves both, so a timer to
/// the next midnight fires when the test crosses it.
#[cfg(test)]
pub struct TestClock {
    executor: gpui::BackgroundExecutor,
    anchor: std::sync::Mutex<(NaiveDateTime, std::time::Instant)>,
}

#[cfg(test)]
impl TestClock {
    /// Installs a test clock reading `start` as the global clock.
    pub fn install(start: NaiveDateTime, cx: &mut App) -> Arc<Self> {
        let executor = cx.background_executor().clone();
        let clock = Arc::new(Self {
            anchor: std::sync::Mutex::new((start, executor.now())),
            executor,
        });
        ReminderClock::set_global(clock.clone(), cx);
        clock
    }

    /// Jumps the wall clock to `now`, as a clock change or a wake from sleep
    /// would, without moving the executor's time.
    pub fn set(&self, now: NaiveDateTime) {
        if let Ok(mut anchor) = self.anchor.lock() {
            *anchor = (now, self.executor.now());
        }
    }
}

#[cfg(test)]
impl WallClock for TestClock {
    fn now(&self) -> NaiveDateTime {
        let Ok(anchor) = self.anchor.lock() else {
            return NaiveDateTime::MIN;
        };
        let (start, at) = *anchor;
        let elapsed = self.executor.now().saturating_duration_since(at);
        start + chrono::Duration::from_std(elapsed).unwrap_or_default()
    }
}

/// The wall clock's date, kept current: it emits `DayChanged` (and notifies)
/// when local midnight passes. Chips relabel, the day reconciler runs and the
/// notifier reschedules on it.
pub struct ReminderDay {
    today: NaiveDate,
    _watch: Task<()>,
}

/// The local date moved: past midnight, or a clock change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayChanged {
    pub today: NaiveDate,
}

impl EventEmitter<DayChanged> for ReminderDay {}

struct GlobalReminderDay(Entity<ReminderDay>);

impl Global for GlobalReminderDay {}

/// The longest the watch sleeps, so a wake from sleep or a clock change is
/// noticed within a minute.
const DAY_CHECK: Duration = Duration::from_secs(60);

impl ReminderDay {
    /// The app's one day watch, created on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(day) = cx.try_global::<GlobalReminderDay>() {
            return day.0.clone();
        }
        let day = cx.new(Self::new);
        cx.set_global(GlobalReminderDay(day.clone()));
        day
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let watch = cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.update(cx, |_, cx| {
                    let clock = ReminderClock::global(cx);
                    let now = clock.now();
                    let midnight = now
                        .date()
                        .checked_add_days(Days::new(1))
                        .map(|tomorrow| tomorrow.and_time(NaiveTime::MIN));
                    midnight
                        .map_or(DAY_CHECK, |midnight| clock.duration_until(midnight))
                        .min(DAY_CHECK)
                }) else {
                    break;
                };
                cx.background_executor()
                    .timer(wait.max(Duration::from_millis(1)))
                    .await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            today: ReminderClock::global(cx).today(),
            _watch: watch,
        }
    }

    pub fn today(&self) -> NaiveDate {
        self.today
    }

    /// Re-reads the clock now, emitting `DayChanged` if the date moved.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let today = ReminderClock::global(cx).today();
        if today != self.today {
            self.today = today;
            cx.emit(DayChanged { today });
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::sync::Mutex;
    use thock_sync_core::parse_line;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    #[test]
    fn a_missing_or_empty_file_means_the_defaults() {
        let defaults = ReminderSettings::default();
        assert_eq!(parse_reminder_settings("").unwrap(), defaults);
        assert_eq!(defaults.index, "reminders.md");
        assert_eq!(defaults.day_headings, vec!["Reminders".to_string()]);
        assert_eq!(defaults.date_only_at, time(9, 0));
        assert_eq!(defaults.tomorrow_at, time(9, 0));
        assert_eq!(defaults.snooze_minutes, 60);
        assert!(defaults.ring_on_desk);
    }

    #[test]
    fn every_key_overrides() {
        let settings = parse_reminder_settings(
            r#"
            index = "life/lembretes.md"
            future_key = 1

            [headings]
            open = ["Abertos", "Open"]
            done = ["Feitos"]
            day = [" Lembretes "]

            [ring]
            date_only_at = "08:30"
            snooze_minutes = 15
            tomorrow_at = "7:45"
            desk = false
            "#,
        )
        .unwrap();
        assert_eq!(
            settings,
            ReminderSettings {
                index: "life/lembretes.md".into(),
                open_headings: vec!["Abertos".into(), "Open".into()],
                done_headings: vec!["Feitos".into()],
                day_headings: vec!["Lembretes".into()],
                date_only_at: time(8, 30),
                snooze_minutes: 15,
                tomorrow_at: time(7, 45),
                ring_on_desk: false,
            }
        );
    }

    #[test]
    fn partial_and_odd_values_fall_back() {
        let settings = parse_reminder_settings(
            "index = \"  \"\n[headings]\nopen = []\nday = [\"\"]\n[ring]\nsnooze_minutes = 0\n",
        )
        .unwrap();
        let defaults = ReminderSettings::default();
        assert_eq!(settings.index, defaults.index);
        assert_eq!(settings.open_headings, defaults.open_headings);
        assert_eq!(settings.day_headings, defaults.day_headings);
        assert_eq!(settings.snooze_minutes, 1);
    }

    #[test]
    fn unusable_values_are_errors() {
        for text in [
            "[ring]\ndate_only_at = \"9am\"",
            "[ring]\ntomorrow_at = \"25:00\"",
            "index = \"../outside.md\"",
            "index = \"/etc/reminders.md\"",
            "index = \"reminders.txt\"",
            "index = 3",
            "not toml",
        ] {
            assert!(parse_reminder_settings(text).is_err(), "{text:?}");
        }
    }

    #[gpui::test]
    async fn settings_load_through_fs(cx: &mut TestAppContext) {
        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree("/vault", serde_json::json!({ ".thock": {} }))
            .await;
        let fs: Arc<dyn Fs> = fs;
        let root = Path::new("/vault");
        assert_eq!(
            load_reminder_settings(&fs, root).await.unwrap(),
            ReminderSettings::default()
        );
        fs.write(
            &root.join(".thock/reminders.toml"),
            b"[ring]\nsnooze_minutes = 30\n",
        )
        .await
        .unwrap();
        assert_eq!(
            load_reminder_settings(&fs, root)
                .await
                .unwrap()
                .snooze_minutes,
            30
        );
        fs.write(&root.join(".thock/reminders.toml"), b"[ring]\ndesk = 3\n")
            .await
            .unwrap();
        assert!(load_reminder_settings(&fs, root).await.is_err());
    }

    #[test]
    fn answer_times() {
        let settings = ReminderSettings::default();
        let wednesday = date(2026, 10, 21);
        let at = |hour, minute, second| wednesday.and_hms_opt(hour, minute, second).unwrap();
        assert_eq!(
            settings.snoozed_due(at(14, 5, 0)),
            Due::new(wednesday, Some(time(15, 5)))
        );
        assert_eq!(
            settings.snoozed_due(at(14, 5, 1)),
            Due::new(wednesday, Some(time(15, 6)))
        );
        assert_eq!(
            settings.snoozed_due(at(23, 30, 0)),
            Due::new(date(2026, 10, 22), Some(time(0, 30)))
        );
        assert_eq!(
            settings.tomorrow_due(wednesday),
            Due::new(date(2026, 10, 22), Some(time(9, 0)))
        );
        assert_eq!(
            settings.monday_due(wednesday),
            Due::new(date(2026, 10, 26), Some(time(9, 0)))
        );
        assert_eq!(
            settings.monday_due(date(2026, 10, 26)),
            Due::new(date(2026, 11, 2), Some(time(9, 0))),
            "a Monday's Monday is the next one"
        );
    }

    fn chip(line: &str, day: Option<NaiveDate>, today: NaiveDate) -> Option<ReminderChip> {
        reminder_chip(&parse_line(line, day)?, today)
    }

    #[test]
    fn every_chip_state_and_label() {
        let today = date(2026, 10, 21);
        let cases = [
            (
                "- [ ] Bank (@2026-10-22 10:00)",
                None,
                ChipState::Upcoming,
                "Tomorrow 10:00",
            ),
            ("Ask (@2026-11-02)", None, ChipState::Upcoming, "Mon 2 Nov"),
            (
                "Renew (@2027-01-04 09:00)",
                None,
                ChipState::Upcoming,
                "Mon 4 Jan 2027 09:00",
            ),
            (
                "- [ ] Plumber (@2026-10-21 14:30)",
                None,
                ChipState::Today,
                "Today 14:30",
            ),
            ("Invoice (@2026-10-21)", None, ChipState::Today, "Today"),
            (
                "- [ ] Dentist (@15:30)",
                Some(today),
                ChipState::Today,
                "Today 15:30",
            ),
            (
                "- [ ] Call (@2026-10-20 18:00)",
                None,
                ChipState::Overdue,
                "Yesterday 18:00 · overdue",
            ),
            (
                "- [ ] Call (@2026-10-18 18:00)",
                None,
                ChipState::Overdue,
                "Sun 18 Oct 18:00 · overdue",
            ),
            (
                "- [ ] Call (@2025-12-30)",
                None,
                ChipState::Overdue,
                "Tue 30 Dec 2025 · overdue",
            ),
            (
                "- [x] Deposit (@2026-10-19 09:00)",
                None,
                ChipState::Closed,
                "Mon 19 Oct",
            ),
            (
                "Quote. ~~(@2026-10-22 10:00)~~",
                None,
                ChipState::Closed,
                "Tomorrow",
            ),
            (
                "Stretch (@17:00)",
                None,
                ChipState::NoDay,
                "17:00 · which day?",
            ),
        ];
        for (line, day, state, label) in cases {
            assert_eq!(
                chip(line, day, today),
                Some(ReminderChip {
                    state,
                    label: label.into()
                }),
                "{line}"
            );
        }
        assert_eq!(
            chip("- [ ] Dropped <!--rem:9e4a21f0b388-->", None, today),
            None
        );
    }

    #[test]
    fn chip_states_group_like_the_index_fixture() {
        #[derive(Deserialize)]
        struct Case {
            now: String,
            daily: DailyNotesFixture,
            files: std::collections::BTreeMap<String, String>,
            reminders: Vec<Expected>,
        }
        #[derive(Deserialize)]
        struct DailyNotesFixture {
            dir: String,
            filename: String,
        }
        #[derive(Deserialize)]
        struct Expected {
            home: String,
            line: Option<usize>,
            group: Option<String>,
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../thock-sync-core/fixtures/v1/reminder_index");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let case: Case =
                serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap())
                    .unwrap();
            let today = NaiveDateTime::parse_from_str(&case.now, "%Y-%m-%dT%H:%M")
                .unwrap()
                .date();
            let daily = DailyNotes {
                dir: case.daily.dir,
                filename: case.daily.filename,
            };
            for expected in case.reminders {
                let Some(line) = expected.line else {
                    continue;
                };
                let text = case.files[&expected.home].split('\n').nth(line).unwrap();
                let reminder = parse_line(text, daily.day_of(&expected.home)).unwrap();
                let group = match reminder_chip(&reminder, today) {
                    Some(chip) => chip.state.group(),
                    None => Some(Group::Done),
                };
                let group = group.map(|group| format!("{group:?}").to_lowercase());
                assert_eq!(group, expected.group, "{text}");
                checked += 1;
            }
        }
        assert!(checked >= 10);
    }

    #[gpui::test]
    async fn the_day_changes_at_midnight(cx: &mut TestAppContext) {
        let clock = cx.update(|cx| {
            TestClock::install(date(2026, 10, 21).and_hms_opt(23, 58, 0).unwrap(), cx)
        });
        let day = cx.update(ReminderDay::global);
        let changes = Arc::new(Mutex::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let changes = changes.clone();
            cx.subscribe(&day, move |_, event: &DayChanged, _| {
                changes.lock().unwrap().push(event.today);
            })
        });
        assert_eq!(day.read_with(cx, |day, _| day.today()), date(2026, 10, 21));

        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert!(changes.lock().unwrap().is_empty(), "23:59 is still today");

        cx.executor().advance_clock(Duration::from_secs(61));
        cx.run_until_parked();
        assert_eq!(*changes.lock().unwrap(), vec![date(2026, 10, 22)]);
        assert_eq!(day.read_with(cx, |day, _| day.today()), date(2026, 10, 22));

        clock.set(date(2026, 10, 25).and_hms_opt(8, 0, 0).unwrap());
        cx.executor().advance_clock(DAY_CHECK);
        cx.run_until_parked();
        assert_eq!(
            *changes.lock().unwrap(),
            vec![date(2026, 10, 22), date(2026, 10, 25)],
            "a clock change is noticed within a minute"
        );
    }
}
