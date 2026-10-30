//! Nudges (spec `v52-nudges.md`): canned, time-and-day-triggered offers the
//! Thock Agent makes to keep a ritual going. Each entry in `REGISTRY` says
//! when it is due over one cheap structural reading of the vault; the chat
//! shows the pick as a V44 opener, and the `Ledger` keeps the day's offers
//! and snoozes across restarts.

use anyhow::{Context as _, Result};
use chrono::{Datelike as _, Days, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike as _, Weekday};
use db::kvp::KeyValueStore;
use fs::Fs;
use gpui::SharedString;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use util::ResultExt as _;

use crate::notes::NoteKind;
use crate::vault::Vault;

/// A nudge in the registry: what it offers, when, and what accepting runs.
pub struct Nudge {
    /// The ledger key, and the id tests and logs name it by.
    pub id: &'static str,
    /// The Routine skill `thock::RunSkill` resolves when the offer is taken.
    pub skill: &'static str,
    /// The copy, when the nudge is due at this moment over this reading.
    pub due: fn(&Moment, &VaultSnapshot) -> Option<SharedString>,
}

/// The local time an evaluation runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub today: NaiveDate,
    pub time: NaiveTime,
}

impl Moment {
    pub fn now() -> Self {
        Self::at(Local::now().naive_local())
    }

    pub fn at(now: NaiveDateTime) -> Self {
        Self {
            today: now.date(),
            time: now.time(),
        }
    }

    /// Nothing is due in the small hours (§4.3): at one in the morning
    /// "yesterday" is still tonight.
    fn is_quiet(&self) -> bool {
        self.time < QUIET_UNTIL
    }
}

const QUIET_UNTIL: NaiveTime = NaiveTime::from_hms_opt(6, 0, 0).expect("a valid time");
const WEEK_REVIEW_FROM: NaiveTime = NaiveTime::from_hms_opt(15, 0, 0).expect("a valid time");
const WEEK_REVIEW_UNTIL_MONDAY: NaiveTime = NaiveTime::from_hms_opt(12, 0, 0).expect("a valid time");
/// How many days back the Wrap nudge looks for the last day (§5.1).
const WRAP_LOOKBACK_DAYS: u64 = 3;
/// How many nudges may be offered in a day (§4.2).
const DAILY_CAP: usize = 2;
const DAILY_CLOSURE: &str = "Daily Closure";
const AI_WEEK_REVIEW: &str = "AI Week Review";

/// What one note says about itself, structurally.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteFacts {
    /// Level-1 headings, without the `#` and trimmed.
    pub headings: HashSet<String>,
    /// The note is the daily template for its day and nothing else.
    pub template_only: bool,
}

impl NoteFacts {
    pub fn from_text(text: &str) -> Self {
        let headings = text
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .map(|heading| heading.trim().to_string())
            .collect();
        Self {
            headings,
            template_only: false,
        }
    }

    fn has_heading(&self, heading: &str) -> bool {
        self.headings.contains(heading)
    }
}

/// One cheap structural reading of the vault, taken once per evaluation
/// (§4.1). Fixed paths only, never a walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultSnapshot {
    /// The enabled Routines' skill ids.
    pub skills: HashSet<String>,
    /// The daily notes of the last three days that exist, by day.
    pub daily: BTreeMap<NaiveDate, NoteFacts>,
    /// The reviewed week's weekly note, if it exists.
    pub week: Option<NoteFacts>,
    /// The days of the reviewed week that have a daily note, by existence
    /// alone.
    pub week_days: BTreeSet<NaiveDate>,
}

impl VaultSnapshot {
    /// Reads the snapshot for `today` through the project `Fs`: the enabled
    /// manifests, up to three daily notes and the template they came from,
    /// the reviewed week's note, and a stat of each of that week's days.
    pub async fn read(fs: Arc<dyn Fs>, vault: Vault, today: NaiveDate) -> Self {
        let skills = crate::routines::enabled_routine_manifests(&vault)
            .into_iter()
            .flat_map(|manifest| manifest.skills)
            .map(|skill| skill.id)
            .collect();
        let template = fs
            .load(&vault.template_path(NoteKind::Daily))
            .await
            .unwrap_or_default();
        let mut daily = BTreeMap::new();
        for offset in 1..=WRAP_LOOKBACK_DAYS {
            let Some(day) = today.checked_sub_days(Days::new(offset)) else {
                continue;
            };
            let path = vault.note_path(NoteKind::Daily, day);
            let Some(text) = fs.load(&path).await.ok() else {
                continue;
            };
            let mut facts = NoteFacts::from_text(&text);
            facts.template_only = is_template_only(&text, &template, day, &note_title(&path));
            daily.insert(day, facts);
        }
        let monday = reviewed_week(today);
        let week = fs
            .load(&vault.note_path(NoteKind::Weekly, monday))
            .await
            .ok()
            .map(|text| NoteFacts::from_text(&text));
        let mut week_days = BTreeSet::new();
        for offset in 0..7 {
            let Some(day) = monday.checked_add_days(Days::new(offset)) else {
                continue;
            };
            if day > today {
                break;
            }
            if fs.is_file(&vault.note_path(NoteKind::Daily, day)).await {
                week_days.insert(day);
            }
        }
        Self {
            skills,
            daily,
            week,
            week_days,
        }
    }
}

fn note_title(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// Stands in for V51's `notes::is_template_only` until that lands on `main`:
// line-by-line equality with the template expanded for the day, `{{time}}`
// lines skipped, trailing whitespace ignored.
fn is_template_only(note: &str, template: &str, date: NaiveDate, title: &str) -> bool {
    let expanded = crate::notes::expand_template(template, date, NaiveTime::MIN, title);
    let note_lines: Vec<&str> = note.trim_end().lines().map(str::trim_end).collect();
    let template_lines: Vec<&str> = template.trim_end().lines().collect();
    let expanded_lines: Vec<&str> = expanded.trim_end().lines().map(str::trim_end).collect();
    if note_lines.len() != template_lines.len() || template_lines.len() != expanded_lines.len() {
        return false;
    }
    note_lines
        .iter()
        .zip(template_lines.iter().zip(expanded_lines.iter()))
        .all(|(note_line, (template_line, expanded_line))| {
            template_line.contains("{{time}}") || note_line == expanded_line
        })
}

/// An offer the chat shows (§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NudgeOffer {
    pub id: &'static str,
    pub skill: &'static str,
    pub text: SharedString,
}

/// The nudges, in priority order (§5).
pub const REGISTRY: &[Nudge] = &[WRAP_YESTERDAY, WEEK_REVIEW];

const WRAP_YESTERDAY: Nudge = Nudge {
    id: "wrap-yesterday",
    skill: "wrap-yesterday",
    due: wrap_due,
};

const WEEK_REVIEW: Nudge = Nudge {
    id: "week-review",
    skill: "week-review",
    due: week_review_due,
};

/// The most recent day before today, within the lookback, whose note is
/// more than the template. The Wrap nudge's day, and the ritual's (§5.1).
fn last_day(moment: &Moment, snapshot: &VaultSnapshot) -> Option<(NaiveDate, NoteFacts)> {
    (1..=WRAP_LOOKBACK_DAYS)
        .filter_map(|offset| moment.today.checked_sub_days(Days::new(offset)))
        .find_map(|day| {
            let facts = snapshot.daily.get(&day)?;
            (!facts.template_only).then(|| (day, facts.clone()))
        })
}

fn wrap_due(moment: &Moment, snapshot: &VaultSnapshot) -> Option<SharedString> {
    let (day, facts) = last_day(moment, snapshot)?;
    if facts.has_heading(DAILY_CLOSURE) {
        return None;
    }
    let name = if moment.today.checked_sub_days(Days::new(1)) == Some(day) {
        "yesterday".to_string()
    } else {
        weekday_name(day.weekday()).to_string()
    };
    Some(format!("It looks like {name} wasn't wrapped up. Should I go for it?").into())
}

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// The Monday of the week a review run today covers (§5.2): this ISO week
/// from Friday to Sunday, the previous one otherwise.
pub fn reviewed_week(today: NaiveDate) -> NaiveDate {
    let this_monday = today - Days::new(today.weekday().num_days_from_monday() as u64);
    match today.weekday() {
        Weekday::Fri | Weekday::Sat | Weekday::Sun => this_monday,
        _ => this_monday - Days::new(7),
    }
}

/// Friday from 15:00 through Sunday, and Monday before noon (§5.2).
fn in_week_review_window(moment: &Moment) -> bool {
    match moment.today.weekday() {
        Weekday::Fri => moment.time >= WEEK_REVIEW_FROM,
        Weekday::Sat | Weekday::Sun => true,
        Weekday::Mon => moment.time < WEEK_REVIEW_UNTIL_MONDAY,
        _ => false,
    }
}

fn week_review_due(moment: &Moment, snapshot: &VaultSnapshot) -> Option<SharedString> {
    if !in_week_review_window(moment) || snapshot.week_days.is_empty() {
        return None;
    }
    if snapshot
        .week
        .as_ref()
        .is_some_and(|week| week.has_heading(AI_WEEK_REVIEW))
    {
        return None;
    }
    let text = if moment.today.weekday() == Weekday::Mon {
        "Last week never got its review. Should I run it now?"
    } else {
        "The week's winding down. Should I run the Week Review?"
    };
    Some(text.into())
}

/// Every nudge due at `moment`, in registry order, among those whose skill
/// the vault has enabled (§4.5). Nothing during the quiet hours.
pub fn evaluate(registry: &[Nudge], moment: &Moment, snapshot: &VaultSnapshot) -> Vec<NudgeOffer> {
    if moment.is_quiet() {
        return Vec::new();
    }
    registry
        .iter()
        .filter(|nudge| snapshot.skills.contains(nudge.skill))
        .filter_map(|nudge| {
            (nudge.due)(moment, snapshot).map(|text| NudgeOffer {
                id: nudge.id,
                skill: nudge.skill,
                text,
            })
        })
        .collect()
}

/// How long until the next top of the hour, plus a few seconds so the tick
/// lands on the new hour, not just short of it (§4.3).
pub fn next_tick(now: NaiveDateTime) -> Duration {
    let hour_start = now
        .with_minute(0)
        .and_then(|now| now.with_second(0))
        .and_then(|now| now.with_nanosecond(0))
        .unwrap_or(now);
    let next = hour_start + chrono::Duration::hours(1) + chrono::Duration::seconds(5);
    (next - now)
        .to_std()
        .unwrap_or(Duration::from_secs(60 * 60))
}

/// The day's offers and snoozes (§4.2), persisted in the key-value store
/// per vault (§4.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// The day each nudge was answered on; it stays away for that day.
    #[serde(default)]
    snoozed: BTreeMap<String, NaiveDate>,
    /// The nudges offered on each day.
    #[serde(default)]
    offered: BTreeMap<NaiveDate, Vec<String>>,
}

impl Ledger {
    pub fn store_key(vault_root: &Path) -> String {
        format!("thock-nudges:{}", vault_root.display())
    }

    /// Loads the vault's ledger, or an empty one when nothing was saved or
    /// what was saved doesn't parse.
    pub fn load(store: &KeyValueStore, key: &str) -> Self {
        store
            .read_kvp(key)
            .context("loading the nudge ledger")
            .log_err()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).log_err())
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).context("serializing the nudge ledger")
    }

    pub fn is_snoozed(&self, id: &str, today: NaiveDate) -> bool {
        self.snoozed.get(id) == Some(&today)
    }

    /// Keeps `id` away for the rest of today.
    pub fn snooze(&mut self, id: &str, today: NaiveDate) {
        self.snoozed.insert(id.to_string(), today);
        self.prune(today);
    }

    /// The offer to show, if any (§4.2): not snoozed today, not the one
    /// already pending, within the day's cap, first in registry order. The
    /// pick is recorded under today.
    pub fn pick(
        &mut self,
        offers: Vec<NudgeOffer>,
        today: NaiveDate,
        pending: Option<&str>,
    ) -> Option<NudgeOffer> {
        self.prune(today);
        let offers: Vec<NudgeOffer> = offers
            .into_iter()
            .filter(|offer| !self.is_snoozed(offer.id, today))
            .collect();
        if offers
            .iter()
            .any(|offer| pending.is_some_and(|pending| pending == offer.id))
        {
            return None;
        }
        let offered_today = self.offered.entry(today).or_default();
        let pick = offers.into_iter().find(|offer| {
            offered_today.iter().any(|id| id == offer.id) || offered_today.len() < DAILY_CAP
        })?;
        if !offered_today.iter().any(|id| id == pick.id) {
            offered_today.push(pick.id.to_string());
        }
        Some(pick)
    }

    fn prune(&mut self, today: NaiveDate) {
        self.snoozed.retain(|_, day| *day == today);
        self.offered.retain(|day, _| *day == today);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::VaultStatus;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("a valid date")
    }

    fn moment(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Moment {
        Moment {
            today: date(year, month, day),
            time: NaiveTime::from_hms_opt(hour, minute, 0).expect("a valid time"),
        }
    }

    fn timeline_skills() -> HashSet<String> {
        ["wrap-yesterday", "week-review", "wrap-today"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn note(headings: &[&str]) -> NoteFacts {
        NoteFacts {
            headings: headings.iter().map(|heading| heading.to_string()).collect(),
            template_only: false,
        }
    }

    fn template_only() -> NoteFacts {
        NoteFacts {
            headings: HashSet::new(),
            template_only: true,
        }
    }

    fn snapshot(daily: &[(NaiveDate, NoteFacts)]) -> VaultSnapshot {
        VaultSnapshot {
            skills: timeline_skills(),
            daily: daily.iter().cloned().collect(),
            week: None,
            week_days: BTreeSet::new(),
        }
    }

    fn ids(offers: &[NudgeOffer]) -> Vec<&'static str> {
        offers.iter().map(|offer| offer.id).collect()
    }

    // 2026-10-30 is a Friday; 2026-11-02 a Monday.
    const FRI: NaiveDate = match NaiveDate::from_ymd_opt(2026, 10, 30) {
        Some(date) => date,
        None => panic!("a valid date"),
    };

    #[test]
    fn the_week_review_is_due_from_friday_afternoon_until_monday_noon() {
        let mut snapshot = snapshot(&[]);
        snapshot.week_days.insert(date(2026, 10, 26));
        let due = |moment: Moment| evaluate(REGISTRY, &moment, &snapshot);
        assert!(ids(&due(moment(2026, 10, 30, 14, 0))).is_empty());
        let friday = due(moment(2026, 10, 30, 16, 0));
        assert_eq!(ids(&friday), ["week-review"]);
        assert_eq!(
            friday[0].text.as_ref(),
            "The week's winding down. Should I run the Week Review?"
        );
        assert_eq!(ids(&due(moment(2026, 10, 31, 9, 0))), ["week-review"]);
        assert_eq!(ids(&due(moment(2026, 11, 1, 21, 0))), ["week-review"]);
        let monday = due(moment(2026, 11, 2, 9, 0));
        assert_eq!(ids(&monday), ["week-review"]);
        assert_eq!(
            monday[0].text.as_ref(),
            "Last week never got its review. Should I run it now?"
        );
        assert!(ids(&due(moment(2026, 11, 2, 13, 0))).is_empty());
        assert!(ids(&due(moment(2026, 11, 4, 16, 0))).is_empty());
    }

    #[test]
    fn the_reviewed_week_is_this_one_from_friday_and_last_week_on_monday() {
        assert_eq!(reviewed_week(FRI), date(2026, 10, 26));
        assert_eq!(reviewed_week(date(2026, 11, 1)), date(2026, 10, 26));
        assert_eq!(reviewed_week(date(2026, 11, 2)), date(2026, 10, 26));
        assert_eq!(reviewed_week(date(2026, 11, 4)), date(2026, 10, 26));
    }

    #[test]
    fn a_reviewed_week_or_one_without_notes_gets_no_review_nudge() {
        let friday = moment(2026, 10, 30, 16, 0);
        let mut snapshot = snapshot(&[]);
        assert!(evaluate(REGISTRY, &friday, &snapshot).is_empty(), "no notes");
        snapshot.week_days.insert(date(2026, 10, 27));
        snapshot.week = Some(note(&["Week Goals", "AI Week Review"]));
        assert!(evaluate(REGISTRY, &friday, &snapshot).is_empty(), "reviewed");
        snapshot.week = Some(note(&["Week Goals"]));
        assert_eq!(ids(&evaluate(REGISTRY, &friday, &snapshot)), ["week-review"]);
    }

    #[test]
    fn the_wrap_nudge_names_the_last_day_that_was_written() {
        let monday = moment(2026, 11, 2, 9, 0);
        let friday_unwrapped = (FRI, note(&["Day Planner"]));
        let offers = evaluate(REGISTRY, &monday, &snapshot(&[friday_unwrapped.clone()]));
        assert_eq!(ids(&offers), ["wrap-yesterday"]);
        assert_eq!(
            offers[0].text.as_ref(),
            "It looks like Friday wasn't wrapped up. Should I go for it?"
        );
        assert_eq!(offers[0].skill, "wrap-yesterday");

        let with_empty_sunday = snapshot(&[friday_unwrapped, (date(2026, 11, 1), template_only())]);
        let offers = evaluate(REGISTRY, &monday, &with_empty_sunday);
        assert!(offers[0].text.contains("Friday"), "a template-only day isn't a day");

        let tuesday = moment(2026, 11, 3, 9, 0);
        let offers = evaluate(
            REGISTRY,
            &tuesday,
            &snapshot(&[(date(2026, 11, 2), note(&["Day Planner"]))]),
        );
        assert_eq!(
            offers[0].text.as_ref(),
            "It looks like yesterday wasn't wrapped up. Should I go for it?"
        );
    }

    #[test]
    fn the_wrap_nudge_stays_quiet_when_there_is_nothing_to_wrap() {
        let monday = moment(2026, 11, 2, 9, 0);
        let wrapped = (FRI, note(&["Day Planner", "Daily Closure"]));
        let thursday = (date(2026, 10, 29), note(&["Day Planner"]));
        assert!(
            evaluate(REGISTRY, &monday, &snapshot(&[wrapped, thursday])).is_empty(),
            "the last day is wrapped; older gaps don't count"
        );
        assert!(evaluate(REGISTRY, &monday, &snapshot(&[])).is_empty(), "no notes");
        let only_template = snapshot(&[(date(2026, 11, 1), template_only())]);
        assert!(evaluate(REGISTRY, &monday, &only_template).is_empty());
        let too_old = snapshot(&[(date(2026, 10, 29), note(&["Day Planner"]))]);
        assert!(evaluate(REGISTRY, &monday, &too_old).is_empty(), "four days back");
    }

    #[test]
    fn nothing_is_due_in_the_small_hours_or_without_the_routine() {
        let friday_unwrapped = (FRI, note(&["Day Planner"]));
        let mut snapshot = snapshot(&[friday_unwrapped]);
        snapshot.week_days.insert(FRI);
        let early = moment(2026, 10, 31, 5, 30);
        assert!(evaluate(REGISTRY, &early, &snapshot).is_empty());
        let saturday = moment(2026, 10, 31, 9, 0);
        assert_eq!(
            ids(&evaluate(REGISTRY, &saturday, &snapshot)),
            ["wrap-yesterday", "week-review"]
        );
        snapshot.skills.clear();
        assert!(evaluate(REGISTRY, &saturday, &snapshot).is_empty());
    }

    #[test]
    fn the_ledger_snoozes_caps_and_keeps_the_pending_one() {
        let today = FRI;
        let offer = |id: &'static str| NudgeOffer {
            id,
            skill: id,
            text: "?".into(),
        };
        let mut ledger = Ledger::default();
        let pick = ledger.pick(vec![offer("a"), offer("b")], today, None);
        assert_eq!(pick.map(|offer| offer.id), Some("a"));
        // Still pending: nothing new.
        assert!(ledger.pick(vec![offer("a"), offer("b")], today, Some("a")).is_none());
        ledger.snooze("a", today);
        assert_eq!(
            ledger.pick(vec![offer("a"), offer("b")], today, None).map(|offer| offer.id),
            Some("b")
        );
        // Two offered today: a third waits for tomorrow, but one of the two
        // comes back without counting.
        assert!(ledger.pick(vec![offer("c")], today, None).is_none());
        assert_eq!(
            ledger.pick(vec![offer("b"), offer("c")], today, None).map(|offer| offer.id),
            Some("b")
        );
        let tomorrow = date(2026, 10, 31);
        assert_eq!(
            ledger.pick(vec![offer("a"), offer("c")], tomorrow, None).map(|offer| offer.id),
            Some("a")
        );
        assert!(!ledger.is_snoozed("a", tomorrow));
        assert!(ledger.offered.keys().all(|day| *day == tomorrow), "old days are pruned");
    }

    #[test]
    fn the_tick_lands_just_past_the_next_hour() {
        let now = date(2026, 10, 30).and_hms_opt(9, 12, 30).expect("a valid time");
        assert_eq!(next_tick(now), Duration::from_secs(47 * 60 + 35));
        let on_the_hour = date(2026, 10, 30).and_hms_opt(23, 0, 0).expect("a valid time");
        assert_eq!(next_tick(on_the_hour), Duration::from_secs(60 * 60 + 5));
    }

    #[test]
    fn a_template_only_note_is_recognized() {
        let template = "# {{date}}\n\nStarted {{time}}\n\n## Day Planner\n";
        let day = FRI;
        let untouched = "# 2026-10-30\n\nStarted 09:12\n\n## Day Planner\n";
        assert!(is_template_only(untouched, template, day, "2026-10-30"));
        let written = "# 2026-10-30\n\nStarted 09:12\n\n## Day Planner\n- [ ] Call the plumber\n";
        assert!(!is_template_only(written, template, day, "2026-10-30"));
        assert!(!is_template_only("", template, day, "2026-10-30"));
    }

    #[test]
    fn the_skills_agree_with_the_nudges_on_the_day_and_the_week() {
        let wrap = include_str!("../assets/routines/timeline/skills/wrap-yesterday.md");
        assert!(wrap.contains("looking back at most three days"));
        assert!(wrap.contains("anything beyond the template"));
        assert!(!wrap.contains("**The target day is yesterday**"));
        let review = include_str!("../assets/routines/timeline/skills/week-review.md");
        assert!(review.contains("the ISO week that contains today when today is Friday"));
        assert!(!review.contains("Monday–Sunday of the week before today"));
    }

    #[gpui::test]
    async fn the_ledger_survives_a_restart(cx: &mut TestAppContext) {
        let store = cx.update(|cx| KeyValueStore::global(cx));
        let key = Ledger::store_key(Path::new("/vault"));
        let mut ledger = Ledger::default();
        ledger.snooze("wrap-yesterday", FRI);
        let json = ledger.to_json().expect("serializes");
        store
            .write_kvp(key.clone(), json)
            .await
            .expect("the store writes");
        let reloaded = Ledger::load(&store, &key);
        assert_eq!(reloaded, ledger);
        assert!(reloaded.is_snoozed("wrap-yesterday", FRI));
    }

    fn vault(root: &Path) -> Vault {
        let config = "schema = 1\n[daily]\ndir = \"days\"\nfilename = \"YYYYMMDD\"\n\
                      [weekly]\ndir = \"weeks\"\nfilename = \"GGGG-[W]WW\"\n";
        std::fs::create_dir_all(root.join(".thock")).expect("the vault folder");
        std::fs::write(root.join(".thock/config.toml"), config).expect("the config");
        match Vault::detect(root) {
            VaultStatus::Valid(vault) => vault,
            other => panic!("not a vault: {other:?}"),
        }
    }

    #[gpui::test]
    async fn the_snapshot_reads_fixed_paths_only(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path().to_path_buf();
        let vault = vault(&root);
        assert_eq!(vault.config.daily.dir, "days");
        assert_eq!(vault.config.weekly.filename, "GGGG-[W]WW");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({
                "templates": { "daily.md": "# {{date}}\n\n## Day Planner\n" },
                "days": {
                    "20261030.md": "# 2026-10-30\n\n## Day Planner\n- [ ] Call\n## Not a closure\n",
                    "20261101.md": "# 2026-11-01\n\n## Day Planner\n",
                    "20261027.md": "# Tuesday\n",
                },
                "weeks": { "2026-W44.md": "# Week Goals\n\n# AI Week Review\n" },
            }),
        )
        .await;
        let monday = date(2026, 11, 2);
        let snapshot = VaultSnapshot::read(fs.clone(), vault.clone(), monday).await;
        assert!(snapshot.skills.is_empty(), "no routine installed");
        let friday = &snapshot.daily[&FRI];
        assert_eq!(friday.headings, HashSet::from(["2026-10-30".to_string()]));
        assert!(!friday.template_only);
        assert!(snapshot.daily[&date(2026, 11, 1)].template_only);
        assert!(!snapshot.daily.contains_key(&date(2026, 10, 31)), "no note");
        assert!(!snapshot.daily.contains_key(&date(2026, 10, 27)), "outside the window");
        let week = snapshot.week.as_ref().expect("last week's note");
        assert!(week.headings.contains(AI_WEEK_REVIEW));
        assert_eq!(
            snapshot.week_days,
            BTreeSet::from([date(2026, 10, 27), FRI, date(2026, 11, 1)])
        );

        // Without a template nothing is template-only, and a missing week
        // note is simply absent.
        fs.remove_file(&root.join("templates/daily.md"), Default::default())
            .await
            .expect("removes");
        let friday_next = date(2026, 11, 6);
        let snapshot = VaultSnapshot::read(fs, vault, friday_next).await;
        assert!(snapshot.week.is_none());
        assert_eq!(snapshot.week_days, BTreeSet::new());
        assert_eq!(snapshot.daily, BTreeMap::new());
    }
}
