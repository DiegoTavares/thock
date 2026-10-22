//! The day's reminders (spec `thock/specs/v45-reminders.md` §7.1, §7.2): the
//! reconciler that keeps a daily note's `### Reminders` subsection, under the
//! Day Planner, in step with the reminders due that day. It follows calendar
//! sync's rules (`calendar.rs`): the same heading resolution and holds, insert
//! and replace only, and a line the person edited is theirs. `reconcile` is
//! pure; `ReminderDaySync`, one per vault and owned by the `ReminderService`,
//! runs it and writes the result through the service.

use anyhow::Context as _;
use chrono::{NaiveDate, NaiveTime};
use fs::Fs;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, SharedString, Subscription, Task, WeakEntity,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thock_sync_core::{
    Due, MirrorLine, MirrorMark, ReminderState, mirror_text, note_link, parse_line, reminder_text,
};
use util::ResultExt as _;

use crate::calendar::{LineEdit, LineEditKind, apply_line_edits};
use crate::day_plan;
use crate::notes::NoteKind;
use crate::reminder_index::IndexedState;
use crate::reminder_service::{ReminderEvent, ReminderService};
use crate::reminders::{ReminderClock, ReminderSettings};
use crate::vault::{VAULT_MARKER_DIR, Vault};

/// Where a reminder stands, as the index reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayStatus {
    Open,
    Closed,
    /// The home line kept its id and lost its token.
    Dropped,
    /// No line carries the id any more.
    Removed,
}

/// One reminder the index knows, as the day reconciler needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayReminder {
    pub id: String,
    /// The home note, relative to the vault root (`projects/house.md`).
    pub home: String,
    /// The home line's words (`reminder_text`); empty when the line is gone.
    pub text: String,
    /// `None` for a dropped or removed reminder, or a time-only token outside
    /// a daily note.
    pub due: Option<Due>,
    pub status: DayStatus,
    /// The words the home line had before its latest change, when the caller
    /// saw them. A mirror still showing these follows the change; one showing
    /// anything else was edited by the person and is frozen.
    pub previous_text: Option<String>,
    /// The home line was closed when last read and is open again, so its
    /// mirrors are unticked. Otherwise a ticked mirror stays ticked: it is a
    /// copy tick the service carries to the home line (§5.3).
    pub reopened: bool,
}

impl DayReminder {
    /// Reads a reminder from its home line in the note at `home`, whose day is
    /// `home_day` when it is a daily note. `None` when the line carries no
    /// reminder.
    pub fn from_home_line(
        id: &str,
        home: &str,
        line: &str,
        home_day: Option<NaiveDate>,
    ) -> Option<Self> {
        let reminder = parse_line(line, home_day)?;
        let status = match reminder.state() {
            ReminderState::Open => DayStatus::Open,
            ReminderState::Closed => DayStatus::Closed,
            ReminderState::Dropped => DayStatus::Dropped,
        };
        Some(Self {
            id: id.to_string(),
            home: home.to_string(),
            text: reminder_text(line),
            due: reminder.due,
            status,
            previous_text: None,
            reopened: false,
        })
    }

    /// A reminder whose line is gone from every note.
    pub fn removed(id: &str, home: &str) -> Self {
        Self {
            id: id.to_string(),
            home: home.to_string(),
            text: String::new(),
            due: None,
            status: DayStatus::Removed,
            previous_text: None,
            reopened: false,
        }
    }
}

/// The headings the reconciler looks for: the planner's, from
/// `[day_planner]`, and the day section's, from `.thock/reminders.toml`. The
/// first name of each is the one written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayConfig {
    pub planner_headings: Vec<String>,
    pub day_headings: Vec<String>,
}

impl DayConfig {
    pub fn new(planner: &day_plan::DayPlannerConfig, settings: &ReminderSettings) -> Self {
        Self {
            planner_headings: std::iter::once(planner.heading.clone())
                .chain(planner.heading_aliases.iter().cloned())
                .collect(),
            day_headings: settings.day_headings.clone(),
        }
    }
}

/// A mirror line the person edited, left as they wrote it. Reported so the
/// service can log it to `.thock/cache/reminders/log.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frozen {
    pub id: String,
    pub line: String,
    /// The words the home line has now.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DayReconciled {
    /// The planner heading is absent and there is a reminder to list; the
    /// reconciler holds rather than inventing one, and the Day Planner offers
    /// *Add heading*.
    NoPlannerSection,
    /// The planner heading is level 6, so the day section would be level 7.
    NoSectionRoom,
    Edits {
        edits: Vec<LineEdit>,
        frozen: Vec<Frozen>,
    },
}

/// Brings the `### Reminders` section of the daily note at `note_path`, whose
/// day is `day`, in step with `reminders` on `today` (§7.1, §7.2). It lists
/// every open reminder due that day whose home is another note and, in today's
/// note only, every open reminder from an earlier day. Inserts land in sort
/// order, mirrors follow their home line, and a moved, closed, dropped or
/// removed reminder is marked, never deleted. Past days are left as they were.
/// Apply the edits with `calendar::apply_line_edits`; applying them and
/// reconciling again yields no edits.
pub fn reconcile(
    note: &str,
    note_path: &str,
    day: NaiveDate,
    today: NaiveDate,
    reminders: &[DayReminder],
    config: &DayConfig,
) -> DayReconciled {
    let empty = DayReconciled::Edits {
        edits: Vec::new(),
        frozen: Vec::new(),
    };
    if day < today {
        return empty;
    }
    let lines: Vec<&str> = note.lines().collect();

    // Any line of the note carrying the id stops an insert: a mirror the
    // person moved out of the section, or a copy of the home line.
    let ids_in_note: HashSet<String> = lines
        .iter()
        .filter_map(|line| parse_line(line, Some(day))?.id)
        .collect();
    let mut seen = HashSet::new();
    let reminders: Vec<&DayReminder> = reminders
        .iter()
        .filter(|reminder| seen.insert(reminder.id.as_str()))
        .collect();
    let mut wanted: Vec<MirrorLine> = reminders
        .iter()
        .filter(|reminder| {
            reminder.status == DayStatus::Open
                && reminder.home != note_path
                && !ids_in_note.contains(&reminder.id)
        })
        .filter_map(|reminder| {
            let mirror = match placement(reminder.due?, day, today) {
                Placement::Day(time) => MirrorLine {
                    checked: false,
                    time,
                    text: mirror_text(&reminder.text),
                    mark: None,
                    carried: None,
                    note: note_link(&reminder.home).to_string(),
                    id: reminder.id.clone(),
                },
                Placement::Carried(due) => MirrorLine {
                    checked: false,
                    time: None,
                    text: mirror_text(&reminder.text),
                    mark: None,
                    carried: Some(due),
                    note: note_link(&reminder.home).to_string(),
                    id: reminder.id.clone(),
                },
                Placement::Moved(_) => return None,
            };
            Some(mirror)
        })
        .collect();
    wanted.sort_by(|a, b| {
        sort_key(a)
            .cmp(&sort_key(b))
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.id.cmp(&b.id))
    });

    let planner_names =
        day_plan::HeadingNames::new(config.planner_headings.iter().map(String::as_str));
    let Some((planner_range, planner_level)) = day_plan::planner_section(&lines, &planner_names)
    else {
        return if wanted.is_empty() {
            empty
        } else {
            DayReconciled::NoPlannerSection
        };
    };
    let Some(section) = find_day_section(&lines, &planner_range, planner_level, config) else {
        if wanted.is_empty() {
            return empty;
        }
        if planner_level >= 6 {
            return DayReconciled::NoSectionRoom;
        }
        return DayReconciled::Edits {
            edits: section_creation_edits(&lines, &planner_range, planner_level, config, &wanted),
            frozen: Vec::new(),
        };
    };

    let by_id: HashMap<&str, &DayReminder> = reminders
        .iter()
        .map(|reminder| (reminder.id.as_str(), *reminder))
        .collect();
    let mut edits = Vec::new();
    let mut frozen = Vec::new();
    let mut updated = HashSet::new();
    // Each mirror's row and sort key after this pass, for placing inserts.
    let mut placed: Vec<(usize, SortKey)> = Vec::new();
    for row in section.clone() {
        let Some(line) = lines.get(row) else {
            continue;
        };
        let Some(parsed) = parse_line(line, Some(day)) else {
            continue;
        };
        let Some(id) = parsed.id.filter(|_| parsed.token.is_none()) else {
            continue;
        };
        let mirror = MirrorLine::parse(line);
        let known = by_id
            .get(id.as_str())
            .filter(|_| updated.insert(id.clone()));
        let (Some(reminder), Some(mirror)) = (known, mirror.clone()) else {
            if let Some(reminder) = known {
                frozen.push(Frozen {
                    id,
                    line: line.to_string(),
                    text: reminder.text.clone(),
                });
            }
            if let Some(mirror) = mirror {
                placed.push((row, sort_key(&mirror)));
            }
            continue;
        };
        match updated_mirror(&mirror, reminder, day, today) {
            Some(next) => {
                placed.push((row, sort_key(&next)));
                let rendered = next.to_string();
                if rendered != line.trim_end() {
                    edits.push(LineEdit {
                        row,
                        kind: LineEditKind::Replace(rendered),
                    });
                }
            }
            None => {
                placed.push((row, sort_key(&mirror)));
                frozen.push(Frozen {
                    id,
                    line: line.to_string(),
                    text: reminder.text.clone(),
                });
            }
        }
    }

    let fallback_row = after_last_content_row(&lines, &section);
    for mirror in &wanted {
        let key = sort_key(mirror);
        let row = placed
            .iter()
            .find(|(_, placed_key)| *placed_key > key)
            .map_or(fallback_row, |(row, _)| *row);
        edits.push(LineEdit {
            row,
            kind: LineEditKind::Insert(mirror.to_string()),
        });
    }
    edits.sort_by_key(|edit| edit.row);
    DayReconciled::Edits { edits, frozen }
}

enum Placement {
    /// Due on the note's day, at its time or untimed.
    Day(Option<NaiveTime>),
    /// Still open from an earlier day, listed in today's note.
    Carried(Due),
    /// Due on another day.
    Moved(NaiveDate),
}

fn placement(due: Due, day: NaiveDate, today: NaiveDate) -> Placement {
    if due.date == day {
        Placement::Day(due.time)
    } else if day == today && due.date < today {
        Placement::Carried(due)
    } else {
        Placement::Moved(due.date)
    }
}

/// Timed mirrors first by time, then untimed ones, then carried ones by
/// their original due (§7.2).
type SortKey = (u8, Option<NaiveTime>, Option<Due>);

fn sort_key(mirror: &MirrorLine) -> SortKey {
    match (mirror.time, mirror.carried) {
        (Some(time), _) => (0, Some(time), None),
        (None, None) => (1, None, None),
        (None, Some(due)) => (2, None, Some(due)),
    }
}

/// The mirror as it should read now, or `None` when the person edited its
/// words and it is frozen.
fn updated_mirror(
    mirror: &MirrorLine,
    reminder: &DayReminder,
    day: NaiveDate,
    today: NaiveDate,
) -> Option<MirrorLine> {
    let text = followed_text(&mirror.text, reminder)?;
    let note = match reminder.status {
        DayStatus::Removed => mirror.note.clone(),
        _ => note_link(&reminder.home).to_string(),
    };
    let base = MirrorLine {
        checked: mirror.checked,
        time: None,
        text,
        mark: None,
        carried: None,
        note,
        id: mirror.id.clone(),
    };
    Some(match reminder.status {
        DayStatus::Dropped => MirrorLine {
            mark: Some(MirrorMark::Dropped),
            ..base
        },
        DayStatus::Removed => MirrorLine {
            mark: Some(MirrorMark::Removed),
            ..base
        },
        DayStatus::Open | DayStatus::Closed => {
            let Some(due) = reminder.due else {
                return Some(mirror.clone());
            };
            let checked = match reminder.status {
                DayStatus::Closed => true,
                _ => mirror.checked && !reminder.reopened,
            };
            match placement(due, day, today) {
                Placement::Day(time) => MirrorLine {
                    checked,
                    time,
                    ..base
                },
                Placement::Carried(due) => MirrorLine {
                    checked,
                    carried: Some(due),
                    ..base
                },
                Placement::Moved(date) => MirrorLine {
                    checked,
                    mark: Some(MirrorMark::Moved(date)),
                    ..base
                },
            }
        }
    })
}

/// The mirror's words after the home line's change: unchanged while they
/// still begin with the home's words (anything the person added after them
/// stays), the home's new words when they still show its previous ones, and
/// `None` when the person rewrote them.
fn followed_text(current: &str, reminder: &DayReminder) -> Option<String> {
    if reminder.text.is_empty() {
        return Some(current.to_string());
    }
    let expected = mirror_text(&reminder.text);
    if words_trailing(current, &expected).is_some() {
        return Some(current.to_string());
    }
    let previous = mirror_text(reminder.previous_text.as_deref()?);
    let trailing = words_trailing(current, &previous)?;
    Some(format!("{expected}{trailing}"))
}

/// What `text` has after `words`, when it begins with them as whole words.
fn words_trailing<'a>(text: &'a str, words: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(words)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(rest)
}

/// The day section: a heading one level below the planner's, inside the
/// planner section, named by `day_headings` (V26 matching). The range is the
/// content below the heading.
fn find_day_section(
    lines: &[&str],
    planner_range: &Range<usize>,
    planner_level: usize,
    config: &DayConfig,
) -> Option<Range<usize>> {
    let child_level = planner_level + 1;
    let names = day_plan::HeadingNames::new(config.day_headings.iter().map(String::as_str));
    let (start, _) = day_plan::best_heading(
        planner_range
            .clone()
            .filter_map(|row| lines.get(row).map(|line| (row, *line))),
        &names,
        Some(child_level),
    )?;
    let end = (start + 1..planner_range.end)
        .find(|&row| {
            lines.get(row).is_some_and(|line| {
                day_plan::heading_level_and_text(line)
                    .is_some_and(|(level, _)| level <= child_level)
            })
        })
        .unwrap_or(planner_range.end);
    Some(start + 1..end)
}

fn after_last_content_row(lines: &[&str], range: &Range<usize>) -> usize {
    range
        .clone()
        .rev()
        .find(|&row| lines.get(row).is_some_and(|line| !line.trim().is_empty()))
        .map_or(range.start, |row| row + 1)
}

/// The section, made the planner section's last child the first time a
/// reminder needs it.
fn section_creation_edits(
    lines: &[&str],
    planner_range: &Range<usize>,
    planner_level: usize,
    config: &DayConfig,
    wanted: &[MirrorLine],
) -> Vec<LineEdit> {
    let row = planner_range.end;
    let heading = config
        .day_headings
        .first()
        .map_or(thock_sync_core::DEFAULT_DAY_HEADING, String::as_str);
    let mut block = Vec::new();
    if row > 0
        && lines
            .get(row - 1)
            .is_some_and(|line| !line.trim().is_empty())
    {
        block.push(String::new());
    }
    block.push(format!("{} {heading}", "#".repeat(planner_level + 1)));
    block.extend(wanted.iter().map(MirrorLine::to_string));
    if lines.get(row).is_some_and(|next| !next.trim().is_empty()) {
        block.push(String::new());
    }
    block
        .into_iter()
        .map(|text| LineEdit {
            row,
            kind: LineEditKind::Insert(text),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The wiring: one per vault, owned by its `ReminderService`, so two windows
// never write a day section twice.

/// What the reconciler remembers of each reminder between runs, so a home
/// line edited while Thock was closed still carries its mirrors along.
const MEMORY_FILE: &str = "cache/reminders/day.json";
/// Where frozen mirrors are logged, one JSON object per line.
const LOG_FILE: &str = "cache/reminders/log.jsonl";
/// The most lines the log keeps; older ones are dropped as new ones arrive.
const LOG_LINES: usize = 500;

/// Why today's day section can't be written; the Day Planner's status row
/// offers the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DayHold {
    NoPlannerHeading { heading: SharedString },
    PlannerHeadingTooDeep { heading: SharedString },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Remembered {
    text: String,
    #[serde(default)]
    previous: Option<String>,
    #[serde(default)]
    closed: bool,
    /// Every day from today on that the reminder was due, so a note it moved
    /// away from is revisited to strike its mirror.
    #[serde(default)]
    days: BTreeSet<NaiveDate>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Memory {
    version: u32,
    reminders: BTreeMap<String, Remembered>,
    /// Frozen mirrors already logged, as the note's day, the id and the
    /// line. Past days are never reconciled again, so their entries go.
    #[serde(default)]
    frozen_logged: BTreeSet<(NaiveDate, String, String)>,
}

#[derive(Serialize)]
struct FrozenLogEntry<'a> {
    at: String,
    path: &'a str,
    id: &'a str,
    line: &'a str,
    home_text: &'a str,
    reason: &'static str,
}

/// Runs `reconcile` on the daily notes the index touches, whenever the index
/// changes, a note is created or changed, or the day turns over (§7.2). It
/// never creates a note and writes only through the service.
pub struct ReminderDaySync {
    service: WeakEntity<ReminderService>,
    memory: Memory,
    memory_loaded: bool,
    /// Vault-relative notes to reconcile next.
    pending: BTreeSet<String>,
    /// Read the index into memory and visit every day it names.
    index_dirty: bool,
    running: bool,
    hold: Option<DayHold>,
    /// The last failed write of a day section, until a retry or a later
    /// write of that note goes through.
    failure: Option<SharedString>,
    /// Notes whose last write failed, reconciled again on `retry` or the
    /// next run.
    retry: BTreeSet<String>,
    load_task: Option<Task<()>>,
    run_task: Option<Task<()>>,
    /// The last write of `day.json` and `log.jsonl`; each write waits for the
    /// one before, so two never interleave.
    cache_write: Option<Task<()>>,
    /// Makes every day-section write fail, for the retry tests.
    #[cfg(test)]
    fail_writes: bool,
    _subscription: Subscription,
}

struct NoteOutcome {
    path: String,
    day: NaiveDate,
    /// `None` when the note doesn't exist.
    reconciled: Option<DayReconciled>,
    error: Option<String>,
}

struct DayJob {
    service: WeakEntity<ReminderService>,
    fs: Arc<dyn Fs>,
    root: PathBuf,
    notes: Vec<(String, NaiveDate)>,
    today: NaiveDate,
    today_path: String,
    reminders: Arc<Vec<DayReminder>>,
    config: Arc<DayConfig>,
    planner_heading: SharedString,
    memory: Option<String>,
    #[cfg(test)]
    fail_writes: bool,
}

impl ReminderDaySync {
    pub fn new(service: &Entity<ReminderService>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let subscription = cx.subscribe(service, |this: &mut Self, _, event, cx| {
                match event {
                    ReminderEvent::IndexChanged
                    | ReminderEvent::DayChanged { .. }
                    | ReminderEvent::SettingsChanged => {
                        this.index_dirty = true;
                    }
                    ReminderEvent::NoteCreated { path } | ReminderEvent::NoteChanged { path } => {
                        this.pending.insert(path.clone());
                    }
                    ReminderEvent::ShowReminders => return,
                }
                this.schedule(cx);
            });
            let mut this = Self {
                service: service.downgrade(),
                memory: Memory::default(),
                memory_loaded: false,
                pending: BTreeSet::new(),
                index_dirty: false,
                running: false,
                hold: None,
                failure: None,
                retry: BTreeSet::new(),
                load_task: None,
                run_task: None,
                cache_write: None,
                #[cfg(test)]
                fail_writes: false,
                _subscription: subscription,
            };
            this.load_memory(cx);
            this
        })
    }

    /// Why today's section is on hold, if it is.
    pub fn hold(&self) -> Option<&DayHold> {
        self.hold.as_ref()
    }

    /// The error of the last day-section write that failed, while it stands.
    pub fn failure(&self) -> Option<&SharedString> {
        self.failure.as_ref()
    }

    /// Writes the day sections that failed again (`thock::RetryReminders`).
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        let mut failed = std::mem::take(&mut self.retry);
        self.pending.append(&mut failed);
        self.schedule(cx);
    }

    fn load_memory(&mut self, cx: &mut Context<Self>) {
        let Some(service) = self.service.upgrade() else {
            return;
        };
        let (fs, path) = {
            let service = service.read(cx);
            (
                service.fs(cx),
                service
                    .vault()
                    .root
                    .join(VAULT_MARKER_DIR)
                    .join(MEMORY_FILE),
            )
        };
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let memory = match fs {
                Some(fs) if fs.is_file(&path).await => fs
                    .load(&path)
                    .await
                    .log_err()
                    .and_then(|text| serde_json::from_str::<Memory>(&text).log_err()),
                _ => None,
            };
            this.update(cx, |this, cx| {
                if let Some(memory) = memory {
                    this.memory = memory;
                }
                this.memory_loaded = true;
                this.schedule(cx);
            })
            .log_err();
        }));
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if !self.memory_loaded || self.running {
            return;
        }
        if self.pending.is_empty() && !self.index_dirty {
            return;
        }
        self.running = true;
        self.run_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(job)) = this.update(cx, |this, cx| this.take_job(cx)) else {
                    break;
                };
                let outcomes = run_job(&job, cx).await;
                if this
                    .update(cx, |this, cx| this.finish_job(job, outcomes, cx))
                    .is_err()
                {
                    break;
                }
            }
            this.update(cx, |this, _| this.running = false).log_err();
        }));
    }

    fn take_job(&mut self, cx: &mut Context<Self>) -> Option<DayJob> {
        let service = self.service.upgrade()?;
        if self.pending.is_empty() && !self.index_dirty {
            return None;
        }
        let today = ReminderClock::global(cx).today();
        let service_state = service.read(cx);
        if !service_state.is_loaded() {
            return None;
        }
        let fs = service_state.fs(cx)?;
        let vault = service_state.vault().clone();
        let reminder_config = service_state.config();
        let config = DayConfig::new(&vault.config.day_planner, service_state.settings());
        let planner_heading: SharedString =
            vault.config.day_planner.heading.trim().to_string().into();
        let indexed = service_state.reminders().to_vec();

        let mut reopened = HashSet::new();
        let mut memory_changed = false;
        if std::mem::take(&mut self.index_dirty) {
            let known: HashSet<&str> = indexed
                .iter()
                .map(|reminder| reminder.id.as_str())
                .collect();
            let remembered = self.memory.reminders.len();
            self.memory
                .reminders
                .retain(|id, _| known.contains(id.as_str()));
            memory_changed |= self.memory.reminders.len() != remembered;
            for reminder in &indexed {
                if reminder.state == IndexedState::Missing {
                    continue;
                }
                let text = reminder.text.clone().unwrap_or_default();
                let closed = reminder.state == IndexedState::Closed;
                let entry = self
                    .memory
                    .reminders
                    .entry(reminder.id.clone())
                    .or_default();
                let before = entry.clone();
                if entry.text != text && !text.is_empty() {
                    if !entry.text.is_empty() {
                        entry.previous = Some(std::mem::take(&mut entry.text));
                    }
                    entry.text = text;
                }
                if entry.closed && reminder.state == IndexedState::Open {
                    reopened.insert(reminder.id.clone());
                }
                entry.closed = closed;
                entry.days.retain(|day| *day >= today);
                if let Some(due) = reminder.due.filter(|due| due.date >= today) {
                    entry.days.insert(due.date);
                }
                memory_changed |= *entry != before;
            }
            let days: BTreeSet<NaiveDate> = std::iter::once(today)
                .chain(
                    self.memory
                        .reminders
                        .values()
                        .flat_map(|entry| entry.days.iter().copied()),
                )
                .collect();
            for day in days {
                self.pending.insert(relative_path(
                    &vault,
                    &vault.note_path(NoteKind::Daily, day),
                ));
            }
        }

        let mut paths = std::mem::take(&mut self.pending);
        paths.append(&mut self.retry);
        let notes: Vec<(String, NaiveDate)> = paths
            .into_iter()
            .filter_map(|path| {
                let day = reminder_config.daily.day_of(&path)?;
                (day >= today).then_some((path, day))
            })
            .collect();
        let reminders = indexed
            .iter()
            .filter_map(|reminder| {
                let status = match reminder.state {
                    IndexedState::Open => DayStatus::Open,
                    IndexedState::Closed => DayStatus::Closed,
                    IndexedState::Dropped => DayStatus::Dropped,
                    IndexedState::Removed => DayStatus::Removed,
                    IndexedState::Missing => return None,
                };
                let remembered = self.memory.reminders.get(&reminder.id);
                Some(DayReminder {
                    id: reminder.id.clone(),
                    home: reminder.home.clone(),
                    text: reminder.text.clone().unwrap_or_default(),
                    due: reminder.due,
                    status,
                    previous_text: remembered.and_then(|entry| entry.previous.clone()),
                    reopened: reopened.contains(&reminder.id),
                })
            })
            .collect();
        let memory = memory_changed
            .then(|| serde_json::to_string(&self.memory).log_err())
            .flatten();
        if notes.is_empty() && memory.is_none() {
            return None;
        }
        Some(DayJob {
            service: service.downgrade(),
            fs,
            today_path: relative_path(&vault, &vault.note_path(NoteKind::Daily, today)),
            root: vault.root.clone(),
            notes,
            today,
            reminders: Arc::new(reminders),
            config: Arc::new(config),
            planner_heading,
            memory,
            #[cfg(test)]
            fail_writes: self.fail_writes,
        })
    }

    fn finish_job(&mut self, job: DayJob, outcomes: Vec<NoteOutcome>, cx: &mut Context<Self>) {
        let mut log = String::new();
        let now = ReminderClock::global(cx)
            .now()
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        let failure_before = self.failure.clone();
        for NoteOutcome {
            path,
            day,
            reconciled: outcome,
            error,
        } in &outcomes
        {
            match error {
                Some(error) => {
                    // Tried again on `retry` or the next trigger, never in a
                    // loop.
                    self.retry.insert(path.clone());
                    self.failure = Some(error.clone().into());
                }
                None => {
                    self.retry.remove(path);
                }
            }
            if *path == job.today_path {
                let hold = match outcome {
                    Some(DayReconciled::NoPlannerSection) => Some(DayHold::NoPlannerHeading {
                        heading: job.planner_heading.clone(),
                    }),
                    Some(DayReconciled::NoSectionRoom) => Some(DayHold::PlannerHeadingTooDeep {
                        heading: job.planner_heading.clone(),
                    }),
                    _ => None,
                };
                if hold != self.hold {
                    self.hold = hold;
                    cx.notify();
                    // The status rows observe the service, not this entity.
                    job.service.update(cx, |_, cx| cx.notify()).log_err();
                }
            }
            let Some(DayReconciled::Edits { frozen, .. }) = outcome else {
                continue;
            };
            for frozen in frozen {
                if !self
                    .memory
                    .frozen_logged
                    .insert((*day, frozen.id.clone(), frozen.line.clone()))
                {
                    continue;
                }
                let entry = FrozenLogEntry {
                    at: now.clone(),
                    path,
                    id: &frozen.id,
                    line: &frozen.line,
                    home_text: &frozen.text,
                    reason: "the mirror was edited by hand",
                };
                if let Some(line) = serde_json::to_string(&entry).log_err() {
                    log.push_str(&line);
                    log.push('\n');
                }
            }
        }
        if self.retry.is_empty() {
            self.failure = None;
        }
        if self.failure != failure_before {
            cx.notify();
            // The status rows observe the service, not this entity.
            job.service.update(cx, |_, cx| cx.notify()).log_err();
        }
        let logged_before = self.memory.frozen_logged.len();
        self.memory
            .frozen_logged
            .retain(|(day, _, _)| *day >= job.today);
        let pruned = self.memory.frozen_logged.len() != logged_before;
        let memory = if log.is_empty() && !pruned {
            job.memory
        } else {
            serde_json::to_string(&self.memory).log_err()
        };
        if memory.is_none() && log.is_empty() {
            return;
        }
        let fs = job.fs.clone();
        let cache = job.root.join(VAULT_MARKER_DIR);
        let previous = self.cache_write.take();
        self.cache_write = Some(cx.background_spawn(async move {
            if let Some(previous) = previous {
                previous.await;
            }
            let written: anyhow::Result<()> = async {
                if let Some(memory) = memory {
                    let path = cache.join(MEMORY_FILE);
                    if let Some(parent) = path.parent() {
                        fs.create_dir(parent).await?;
                    }
                    fs.atomic_write(path, memory).await?;
                }
                if !log.is_empty() {
                    let path = cache.join(LOG_FILE);
                    let existing = if fs.is_file(&path).await {
                        fs.load(&path).await?
                    } else {
                        String::new()
                    };
                    fs.atomic_write(path, appended_log(&existing, &log)).await?;
                }
                anyhow::Ok(())
            }
            .await;
            written
                .context("saving the day reconciler's cache")
                .log_err();
        }));
    }
}

/// `existing` with `new` lines after it, keeping only the last `LOG_LINES`.
/// `Fs` has no append, so the log is written whole; the cap keeps that cheap.
fn appended_log(existing: &str, new: &str) -> String {
    let lines: Vec<&str> = existing.lines().chain(new.lines()).collect();
    let start = lines.len().saturating_sub(LOG_LINES);
    let mut log = lines[start..].join("\n");
    log.push('\n');
    log
}

fn relative_path(vault: &Vault, path: &Path) -> String {
    path.strip_prefix(&vault.root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Reconciles each note of `job` through the service, one after another.
/// A note that is missing is left missing (`None`). A failed write surfaces
/// in this entity's `failure`, and the note is tried again on `retry` or the
/// next run.
async fn run_job(job: &DayJob, cx: &mut AsyncApp) -> Vec<NoteOutcome> {
    let mut outcomes = Vec::new();
    for (path, day) in &job.notes {
        // A missing note stays missing, and asking the service to write it
        // would only make it read the path again.
        if !job.fs.is_file(&job.root.join(path)).await {
            outcomes.push(NoteOutcome {
                path: path.clone(),
                day: *day,
                reconciled: None,
                error: None,
            });
            continue;
        }
        #[cfg(test)]
        if job.fail_writes {
            outcomes.push(NoteOutcome {
                path: path.clone(),
                day: *day,
                reconciled: None,
                error: Some(format!("{path}: the disk is full")),
            });
            continue;
        }
        let outcome = Arc::new(Mutex::new(None));
        let Ok(write) = job.service.update(cx, |service, cx| {
            let outcome = outcome.clone();
            let reminders = job.reminders.clone();
            let config = job.config.clone();
            let note_path = path.clone();
            let (day, today) = (*day, job.today);
            service.write_day_section(
                path,
                move |text| {
                    let text = text?;
                    let reconciled = reconcile(text, &note_path, day, today, &reminders, &config);
                    let edited = match &reconciled {
                        DayReconciled::Edits { edits, .. } if !edits.is_empty() => {
                            Some(apply_line_edits(text, edits))
                        }
                        _ => None,
                    };
                    if let Ok(mut outcome) = outcome.lock() {
                        *outcome = Some(reconciled);
                    }
                    edited
                },
                cx,
            )
        }) else {
            break;
        };
        let error = match write.await {
            Ok(_) => None,
            Err(error) => {
                log::warn!("Thock reminders: writing the day section of {path} failed: {error:#}");
                Some(format!("{path}: {error:#}"))
            }
        };
        let reconciled = outcome.lock().ok().and_then(|mut outcome| outcome.take());
        outcomes.push(NoteOutcome {
            path: path.clone(),
            day: *day,
            reconciled,
            error,
        });
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "daily/2026-10-21.md";

    fn date(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap()
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    fn today() -> NaiveDate {
        date(10, 21)
    }

    fn config() -> DayConfig {
        DayConfig::new(
            &day_plan::DayPlannerConfig::default(),
            &ReminderSettings::default(),
        )
    }

    fn open(id: &str, home: &str, text: &str, due: Due) -> DayReminder {
        DayReminder {
            id: id.to_string(),
            home: home.to_string(),
            text: text.to_string(),
            due: Some(due),
            status: DayStatus::Open,
            previous_text: None,
            reopened: false,
        }
    }

    fn at(month: u32, day: u32, hour: u32, minute: u32) -> Due {
        Due::new(date(month, day), Some(time(hour, minute)))
    }

    fn on(month: u32, day: u32) -> Due {
        Due::new(date(month, day), None)
    }

    fn run_on(
        note: &str,
        path: &str,
        day: NaiveDate,
        reminders: &[DayReminder],
    ) -> (String, Vec<Frozen>) {
        match reconcile(note, path, day, today(), reminders, &config()) {
            DayReconciled::Edits { edits, frozen } => {
                let applied = apply_line_edits(note, &edits);
                match reconcile(&applied, path, day, today(), reminders, &config()) {
                    DayReconciled::Edits { edits, .. } => {
                        assert!(edits.is_empty(), "not idempotent:\n{applied}\n{edits:?}")
                    }
                    other => panic!("second pass: {other:?}"),
                }
                (applied, frozen)
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    fn run(note: &str, reminders: &[DayReminder]) -> (String, Vec<Frozen>) {
        run_on(note, NOTE, today(), reminders)
    }

    /// The spec's §7.1 example: two of the day's reminders and one carried.
    fn example() -> Vec<DayReminder> {
        vec![
            open(
                "2f6b08e19a5c",
                "backlog.md",
                "Renew the car insurance",
                at(10, 20, 9, 0),
            ),
            open(
                "a90c1377d02e",
                "reminders.md",
                "Send the October invoice",
                on(10, 21),
            ),
            open(
                "7d3e55a1c0f2",
                "projects/house.md",
                "Call the plumber",
                at(10, 21, 14, 30),
            ),
        ]
    }

    #[test]
    fn the_section_is_created_as_the_planners_last_child_in_order() {
        let note = "# Wednesday\n\n## Day planner\n- [x] 09:30 - 11:00 Deep work on the budget\n\
                    - [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->\n\n\
                    ### Calendar\n- [ ] 12:00 - 12:30 Team sync <!--gcal:5d0c2a91b7e4-->\n\n\
                    ## Notes\nA day.\n";
        let mut reminders = example();
        reminders.push(open("5b71c08e3f6d", NOTE, "Dentist", at(10, 21, 15, 30)));
        let (applied, frozen) = run(note, &reminders);
        assert!(frozen.is_empty());
        assert_eq!(
            applied,
            "# Wednesday\n\n## Day planner\n- [x] 09:30 - 11:00 Deep work on the budget\n\
             - [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->\n\n\
             ### Calendar\n- [ ] 12:00 - 12:30 Team sync <!--gcal:5d0c2a91b7e4-->\n\n\
             ### Reminders\n\
             - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
             - [ ] Send the October invoice · [[reminders]] <!--rem:a90c1377d02e-->\n\
             - [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]] <!--rem:2f6b08e19a5c-->\n\n\
             ## Notes\nA day.\n"
        );
    }

    #[test]
    fn inserts_sort_timed_then_untimed_then_carried_without_reordering() {
        let note = "## Day planner\n\n### Reminders\n\
                    - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
                    my own words\n\
                    - [ ] Send the October invoice · [[reminders]] <!--rem:a90c1377d02e-->\n";
        let mut reminders = example();
        reminders.push(open("111111111111", "a.md", "Early", at(10, 21, 8, 0)));
        reminders.push(open("222222222222", "b.md", "Lunch", at(10, 21, 12, 0)));
        reminders.push(open("333333333333", "c.md", "Late", at(10, 21, 20, 0)));
        reminders.push(open("444444444444", "d.md", "Older", on(10, 19)));
        let (applied, _) = run(note, &reminders);
        assert_eq!(
            applied,
            "## Day planner\n\n### Reminders\n\
             - [ ] 08:00 Early · [[a]] <!--rem:111111111111-->\n\
             - [ ] 12:00 Lunch · [[b]] <!--rem:222222222222-->\n\
             - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
             my own words\n\
             - [ ] 20:00 Late · [[c]] <!--rem:333333333333-->\n\
             - [ ] Send the October invoice · [[reminders]] <!--rem:a90c1377d02e-->\n\
             - [ ] Older · 2026-10-19 · [[d]] <!--rem:444444444444-->\n\
             - [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]] <!--rem:2f6b08e19a5c-->\n"
        );
    }

    #[test]
    fn a_home_change_replaces_the_time_and_the_words() {
        let note = "## Day planner\n### Reminders\n\
                    - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n";
        let mut moved = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 21, 16, 0),
        );
        let (applied, _) = run(note, std::slice::from_ref(&moved));
        assert!(
            applied.contains("- [ ] 16:00 Call the plumber · [[projects/house]]"),
            "{applied}"
        );

        moved.text = "Call the plumber about the boiler".to_string();
        moved.previous_text = Some("Call the plumber".to_string());
        let (applied, frozen) = run(note, &[moved]);
        assert!(frozen.is_empty());
        assert!(
            applied.contains("- [ ] 16:00 Call the plumber about the boiler · [[projects/house]]"),
            "{applied}"
        );
    }

    #[test]
    fn words_the_person_added_after_the_home_words_stay() {
        let note = "## Day planner\n### Reminders\n\
                    - [ ] 14:30 Call the plumber - he said Tuesday · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n";
        let reminder = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 21, 15, 0),
        );
        let (applied, frozen) = run(note, &[reminder]);
        assert!(frozen.is_empty());
        assert!(
            applied.contains("- [ ] 15:00 Call the plumber - he said Tuesday ·"),
            "{applied}"
        );
    }

    #[test]
    fn a_mirror_the_person_rewrote_is_frozen_and_reported() {
        let note = "## Day planner\n### Reminders\n\
                    - [ ] 14:30 Ring Bob about the leak · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
                    - [ ] 10:00 Pay rent, edited past the shape <!--rem:a90c1377d02e--> extra\n";
        let reminders = [
            open(
                "7d3e55a1c0f2",
                "projects/house.md",
                "Call the plumber",
                at(10, 21, 16, 0),
            ),
            open(
                "a90c1377d02e",
                "reminders.md",
                "Pay rent",
                at(10, 21, 10, 0),
            ),
        ];
        let (applied, frozen) = run(note, &reminders);
        assert_eq!(
            applied, note,
            "frozen lines are never touched, nor re-inserted"
        );
        assert_eq!(frozen.len(), 2);
        assert_eq!(frozen[0].id, "7d3e55a1c0f2");
        assert_eq!(frozen[0].text, "Call the plumber");
        assert_eq!(frozen[1].id, "a90c1377d02e");
    }

    #[test]
    fn a_move_strikes_the_line_and_a_move_back_restores_it() {
        let note = "## Day planner\n### Reminders\n\
                    - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n";
        let away = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 23, 9, 0),
        );
        let (moved, _) = run(note, &[away]);
        assert_eq!(
            moved,
            "## Day planner\n### Reminders\n\
             - [ ] ~~Call the plumber~~ → 2026-10-23 · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n"
        );
        let back = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 21, 14, 30),
        );
        let (restored, _) = run(&moved, &[back]);
        assert_eq!(restored, note);
    }

    #[test]
    fn a_close_ticks_and_a_drop_or_removal_strikes() {
        let note = "## Day planner\n### Reminders\n\
                    - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
                    - [ ] Send the October invoice · [[reminders]] <!--rem:a90c1377d02e-->\n\
                    - [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]] <!--rem:2f6b08e19a5c-->\n";
        let mut closed = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 21, 14, 30),
        );
        closed.status = DayStatus::Closed;
        let dropped = DayReminder {
            due: None,
            status: DayStatus::Dropped,
            ..open(
                "a90c1377d02e",
                "reminders.md",
                "Send the October invoice",
                on(10, 21),
            )
        };
        let removed = DayReminder::removed("2f6b08e19a5c", "backlog.md");
        let (applied, _) = run(note, &[closed, dropped, removed]);
        assert_eq!(
            applied,
            "## Day planner\n### Reminders\n\
             - [x] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
             - [ ] ~~Send the October invoice~~ (dropped) · [[reminders]] <!--rem:a90c1377d02e-->\n\
             - [ ] ~~Renew the car insurance~~ (removed) · [[backlog]] <!--rem:2f6b08e19a5c-->\n"
        );
    }

    #[test]
    fn a_closed_reminder_is_never_inserted_and_a_mirror_unticks_only_on_reopen() {
        let note = "## Day planner\n### Reminders\n\
                    - [x] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n";
        let mut closed = open(
            "a90c1377d02e",
            "reminders.md",
            "Send the invoice",
            on(10, 21),
        );
        closed.status = DayStatus::Closed;
        let still_open = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 21, 14, 30),
        );
        let (applied, _) = run(note, &[closed, still_open.clone()]);
        assert_eq!(applied, note, "a person's tick waits for the service");

        let reopened = DayReminder {
            reopened: true,
            ..still_open
        };
        let (applied, _) = run(note, &[reopened]);
        assert!(
            applied.contains("- [ ] 14:30 Call the plumber · [[projects/house]]"),
            "a reopened home unticks its mirror: {applied}"
        );
    }

    #[test]
    fn carried_reminders_go_only_in_todays_note() {
        let earlier = [open(
            "2f6b08e19a5c",
            "backlog.md",
            "Renew the car insurance",
            at(10, 20, 9, 0),
        )];
        let tomorrow = "## Day planner\n- [ ] Walk\n";
        let (applied, _) = run_on(tomorrow, "daily/2026-10-22.md", date(10, 22), &earlier);
        assert_eq!(applied, tomorrow, "a future day gets no overdue reminders");

        let (applied, _) = run("## Day planner\n- [ ] Walk\n", &earlier);
        assert!(applied.contains("- [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]]"));
    }

    #[test]
    fn a_future_day_lists_its_own_reminders() {
        let reminders = [
            open(
                "7d3e55a1c0f2",
                "projects/house.md",
                "Call the plumber",
                at(10, 22, 10, 0),
            ),
            open("a90c1377d02e", "reminders.md", "Today's thing", on(10, 21)),
        ];
        let (applied, _) = run_on(
            "## Day planner\n",
            "daily/2026-10-22.md",
            date(10, 22),
            &reminders,
        );
        assert_eq!(
            applied,
            "## Day planner\n\n### Reminders\n\
             - [ ] 10:00 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n"
        );
    }

    #[test]
    fn past_days_are_left_as_they_were() {
        let note = "## Day planner\n### Reminders\n\
                    - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n";
        let mut closed = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 20, 14, 30),
        );
        closed.status = DayStatus::Closed;
        let new = open(
            "a90c1377d02e",
            "reminders.md",
            "Send the invoice",
            on(10, 20),
        );
        let (applied, _) = run_on(note, "daily/2026-10-20.md", date(10, 20), &[closed, new]);
        assert_eq!(applied, note);
    }

    #[test]
    fn reminders_whose_home_is_this_note_are_not_repeated() {
        let note = "## Day planner\n- [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->\n";
        let reminders = [
            open("5b71c08e3f6d", NOTE, "Dentist", at(10, 21, 15, 30)),
            // The index still names the old home, but a copy is already here.
            open(
                "5b71c08e3f6d",
                "elsewhere.md",
                "Dentist",
                at(10, 21, 15, 30),
            ),
        ];
        let (applied, _) = run(note, &reminders);
        assert_eq!(applied, note);
    }

    #[test]
    fn a_mirror_moved_out_of_the_section_is_not_inserted_again() {
        let note = "## Day planner\n\
                    - [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\
                    ### Reminders\n";
        let reminder = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 21, 14, 30),
        );
        let (applied, _) = run(note, &[reminder]);
        assert_eq!(applied, note);
    }

    #[test]
    fn an_unknown_id_is_left_alone() {
        let note = "## Day planner\n### Reminders\n\
                    - [ ] 14:30 Something gone · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n";
        let (applied, frozen) = run(note, &[]);
        assert_eq!(applied, note);
        assert!(frozen.is_empty());
    }

    #[test]
    fn no_planner_heading_holds_only_when_there_is_something_to_list() {
        let note = "# Wednesday\n- [ ] Walk\n";
        assert_eq!(
            reconcile(note, NOTE, today(), today(), &example(), &config()),
            DayReconciled::NoPlannerSection
        );
        assert_eq!(
            reconcile(note, NOTE, today(), today(), &[], &config()),
            DayReconciled::Edits {
                edits: Vec::new(),
                frozen: Vec::new()
            }
        );
        assert_eq!(
            reconcile(
                "###### Day planner\n",
                NOTE,
                today(),
                today(),
                &example(),
                &config()
            ),
            DayReconciled::NoSectionRoom
        );
    }

    #[test]
    fn headings_match_loosely_and_aliases_count() {
        let mut config = config();
        config.day_headings = vec!["Lembretes".to_string(), "Reminders".to_string()];
        config.planner_headings = vec!["Hoje".to_string(), "Day planner".to_string()];
        let note = "## 📅 Day-planner\n### 🔔 **Reminders**\n";
        let reminder = open(
            "7d3e55a1c0f2",
            "projects/house.md",
            "Call the plumber",
            at(10, 21, 14, 30),
        );
        let DayReconciled::Edits { edits, .. } = reconcile(
            note,
            NOTE,
            today(),
            today(),
            std::slice::from_ref(&reminder),
            &config,
        ) else {
            panic!("expected edits");
        };
        let applied = apply_line_edits(note, &edits);
        assert_eq!(applied.matches("Reminders").count(), 1, "{applied}");

        let DayReconciled::Edits { edits, .. } =
            reconcile("## Hoje\n", NOTE, today(), today(), &[reminder], &config)
        else {
            panic!("expected edits");
        };
        assert!(apply_line_edits("## Hoje\n", &edits).contains("### Lembretes\n"));
    }

    #[test]
    fn mirror_text_is_cut_and_cannot_forge_a_marker() {
        let long = "word ".repeat(30);
        let reminders = [
            open("111111111111", "a.md", &long, on(10, 21)),
            open(
                "222222222222",
                "b.md",
                "~~sneaky~~ <!--rem:ffffffffffff-->",
                on(10, 21),
            ),
            open("333333333333", "c.md", "", on(10, 21)),
            open(
                "444444444444",
                "d.md",
                "Moved from (@2026-10-20 09:00) to here",
                on(10, 21),
            ),
        ];
        let (applied, _) = run("## Day planner\n", &reminders);
        let mirrors: Vec<MirrorLine> = applied.lines().filter_map(MirrorLine::parse).collect();
        assert_eq!(mirrors.len(), 4, "{applied}");
        assert!(
            mirrors
                .iter()
                .all(|mirror| mirror.text.chars().count() <= thock_sync_core::MIRROR_TEXT_CHARS + 1)
        );
        assert!(mirrors.iter().any(|mirror| mirror.text == "Reminder"));
        let mut ids: Vec<&str> = mirrors.iter().map(|mirror| mirror.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            [
                "111111111111",
                "222222222222",
                "333333333333",
                "444444444444"
            ]
        );
        for line in applied.lines().filter(|line| line.contains("<!--rem:")) {
            assert!(
                parse_line(line, None).is_some_and(|reminder| reminder.token.is_none()),
                "no token on a mirror line: {line}"
            );
        }
        for line in applied.lines().filter(|line| line.contains("<!--rem:")) {
            let id = parse_line(line, None).and_then(|reminder| reminder.id);
            assert!(id.is_some_and(|id| ids.contains(&id.as_str())), "{line}");
        }
    }

    #[test]
    fn from_home_line_reads_state_text_and_due() {
        let reminder = DayReminder::from_home_line(
            "5b71c08e3f6d",
            NOTE,
            "- [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->",
            Some(today()),
        )
        .unwrap();
        assert_eq!(reminder.text, "Dentist");
        assert_eq!(reminder.due, Some(at(10, 21, 15, 30)));
        assert_eq!(reminder.status, DayStatus::Open);
        let dropped = DayReminder::from_home_line(
            "9e4a21f0b388",
            "projects/house.md",
            "- [ ] Send the leak photos <!--rem:9e4a21f0b388-->",
            None,
        )
        .unwrap();
        assert_eq!(dropped.status, DayStatus::Dropped);
        assert_eq!(dropped.due, None);
    }
}

#[cfg(test)]
mod wiring_tests {
    use super::*;
    use crate::reminders::TestClock;
    use crate::vault::VaultConfig;
    use chrono::NaiveDateTime;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use project::Project;
    use serde_json::json;
    use settings::SettingsStore;
    use std::time::Duration;

    const HOUSE: &str = "projects/house.md";
    const TODAY: &str = "daily/2026-10-21.md";
    const TOMORROW: &str = "daily/2026-10-22.md";

    struct Setup {
        fs: Arc<FakeFs>,
        _project: Entity<Project>,
        service: Entity<ReminderService>,
    }

    async fn setup(cx: &mut TestAppContext, tree: serde_json::Value) -> Setup {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            let start =
                NaiveDateTime::parse_from_str("2026-10-21 14:05", "%Y-%m-%d %H:%M").unwrap();
            TestClock::install(start, cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", tree).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let vault = Vault {
            root: PathBuf::from("/vault"),
            config: VaultConfig::default(),
        };
        let service = cx.update(|cx| ReminderService::attach(vault, &project, None, cx));
        settle(cx);
        Setup {
            fs,
            _project: project,
            service,
        }
    }

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..16 {
            cx.executor().advance_clock(Duration::from_millis(500));
            cx.run_until_parked();
        }
    }

    async fn read(setup: &Setup, path: &str) -> String {
        setup
            .fs
            .load(&Path::new("/vault").join(path))
            .await
            .unwrap_or_default()
    }

    async fn write(setup: &Setup, cx: &mut TestAppContext, path: &str, text: &str) {
        setup
            .fs
            .atomic_write(Path::new("/vault").join(path), text.to_string())
            .await
            .unwrap();
        settle(cx);
    }

    /// The house note's reminder line, with the id the service gave it.
    async fn house_line(setup: &Setup) -> String {
        read(setup, HOUSE)
            .await
            .lines()
            .find(|line| line.contains("<!--rem:"))
            .unwrap_or_else(|| panic!("no id yet"))
            .to_string()
    }

    #[gpui::test]
    async fn a_new_daily_note_gets_its_section_and_follows_the_index(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": {},
            }),
        )
        .await;
        let line = house_line(&setup).await;

        write(&setup, cx, TODAY, "## Day planner\n- [ ] Walk\n").await;
        let note = read(&setup, TODAY).await;
        assert!(note.contains("\n### Reminders\n"), "{note}");
        assert!(
            note.contains("- [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:"),
            "{note}"
        );

        let moved = line.replace("14:30", "15:00");
        write(&setup, cx, HOUSE, &format!("{moved}\n")).await;
        let note = read(&setup, TODAY).await;
        assert!(note.contains("- [ ] 15:00 Call the plumber ·"), "{note}");

        let closed = thock_sync_core::close(&moved).unwrap();
        write(&setup, cx, HOUSE, &format!("{closed}\n")).await;
        let note = read(&setup, TODAY).await;
        assert!(note.contains("- [x] 15:00 Call the plumber ·"), "{note}");

        write(&setup, cx, HOUSE, &format!("{moved}\n")).await;
        let note = read(&setup, TODAY).await;
        assert!(
            note.contains("- [ ] 15:00 Call the plumber ·"),
            "reopened: {note}"
        );
        assert_eq!(note.matches("<!--rem:").count(), 1, "{note}");
    }

    #[gpui::test]
    async fn a_note_without_a_planner_heading_holds(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": {},
            }),
        )
        .await;
        write(&setup, cx, TODAY, "# Wednesday\n").await;
        assert_eq!(read(&setup, TODAY).await, "# Wednesday\n");
        let hold = setup.service.read_with(cx, |service, cx| {
            service
                .day_sync()
                .and_then(|day| day.read(cx).hold().cloned())
        });
        assert_eq!(
            hold,
            Some(DayHold::NoPlannerHeading {
                heading: "Day planner".into()
            })
        );

        write(&setup, cx, TODAY, "# Wednesday\n\n## Day planner\n").await;
        let note = read(&setup, TODAY).await;
        assert!(note.contains("14:30 Call the plumber"), "{note}");
        let hold = setup.service.read_with(cx, |service, cx| {
            service
                .day_sync()
                .and_then(|day| day.read(cx).hold().cloned())
        });
        assert_eq!(hold, None);
    }

    #[gpui::test]
    async fn an_edited_mirror_is_frozen_and_logged_once(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": { "2026-10-21.md": "## Day planner\n" },
            }),
        )
        .await;
        let note = read(&setup, TODAY).await;
        let mirror = note
            .lines()
            .find(|line| line.contains("<!--rem:"))
            .unwrap()
            .to_string();
        let edited = mirror.replace("Call the plumber", "Ring Bob");
        write(&setup, cx, TODAY, &note.replace(&mirror, &edited)).await;
        let line = house_line(&setup).await;
        write(
            &setup,
            cx,
            HOUSE,
            &format!("{}\n", line.replace("14:30", "16:00")),
        )
        .await;
        write(
            &setup,
            cx,
            HOUSE,
            &format!("{}\n", line.replace("14:30", "17:00")),
        )
        .await;

        assert!(read(&setup, TODAY).await.contains(&edited));
        let log = read(&setup, ".thock/cache/reminders/log.jsonl").await;
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(log.contains("Ring Bob"), "{log}");
    }

    fn day_sync(setup: &Setup, cx: &mut TestAppContext) -> Entity<ReminderDaySync> {
        setup
            .service
            .read_with(cx, |service, _| service.day_sync().cloned())
            .unwrap()
    }

    fn reminders_row(setup: &Setup, cx: &mut TestAppContext) -> Option<String> {
        setup.service.read_with(cx, |service, cx| {
            crate::sync_status::reminder_day_status(service, cx)
                .map(|status| format!("{} | {:?}", status.summary, status.detail))
        })
    }

    #[gpui::test]
    async fn a_failed_day_write_stands_until_retry_writes_it(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": { "2026-10-21.md": "## Day planner\n" },
                "other.md": "Plain\n",
            }),
        )
        .await;
        let day = day_sync(&setup, cx);
        let line = house_line(&setup).await;
        day.update(cx, |day, _| day.fail_writes = true);
        write(
            &setup,
            cx,
            HOUSE,
            &format!("{}\n", line.replace("14:30", "16:00")),
        )
        .await;
        assert!(read(&setup, TODAY).await.contains("14:30 Call the plumber"));
        let row = reminders_row(&setup, cx).expect("a failure row");
        assert!(row.contains("the disk is full"), "{row}");

        // A batch for another note leaves the failure standing.
        write(&setup, cx, "other.md", "Plain, edited\n").await;
        assert!(reminders_row(&setup, cx).is_some());

        day.update(cx, |day, cx| {
            day.fail_writes = false;
            day.retry(cx);
        });
        settle(cx);
        assert!(read(&setup, TODAY).await.contains("16:00 Call the plumber"));
        assert_eq!(reminders_row(&setup, cx), None);
        assert_eq!(day.read_with(cx, |day, _| day.failure().cloned()), None);
    }

    #[test]
    fn the_log_keeps_only_its_last_lines() {
        let existing: String = (0..LOG_LINES).map(|line| format!("{line}\n")).collect();
        let log = appended_log(&existing, "new\n");
        assert_eq!(log.lines().count(), LOG_LINES);
        assert!(log.starts_with("1\n"));
        assert!(log.ends_with("new\n"));
        assert_eq!(appended_log("", "a\n"), "a\n");
    }

    #[gpui::test]
    async fn logged_frozen_lines_of_past_days_are_forgotten(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": { "2026-10-21.md": "## Day planner\n" },
            }),
        )
        .await;
        let note = read(&setup, TODAY).await;
        let mirror = note
            .lines()
            .find(|line| line.contains("<!--rem:"))
            .unwrap()
            .to_string();
        let edited = mirror.replace("Call the plumber", "Ring Bob");
        write(&setup, cx, TODAY, &note.replace(&mirror, &edited)).await;
        let day = day_sync(&setup, cx);
        assert_eq!(
            day.read_with(cx, |day, _| day.memory.frozen_logged.len()),
            1
        );

        cx.executor()
            .advance_clock(Duration::from_secs(10 * 60 * 60));
        settle(cx);
        assert_eq!(
            day.read_with(cx, |day, _| day.memory.frozen_logged.len()),
            0
        );
        let memory = read(&setup, ".thock/cache/reminders/day.json").await;
        assert!(!memory.contains("Ring Bob"), "{memory}");
    }

    #[gpui::test]
    async fn a_quick_undo_on_the_home_is_never_taken_back_by_the_mirror(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": { "2026-10-21.md": "## Day planner\n" },
            }),
        )
        .await;
        let line = house_line(&setup).await;
        assert!(
            read(&setup, TODAY)
                .await
                .contains("- [ ] 14:30 Call the plumber")
        );

        let closed = thock_sync_core::close(&line).unwrap();
        setup
            .fs
            .atomic_write(Path::new("/vault").join(HOUSE), format!("{closed}\n"))
            .await
            .unwrap();
        // One batch: the index follows the close and the reconciler ticks the
        // mirror. The batch that reads the ticked mirror hasn't run yet.
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        assert!(
            read(&setup, TODAY)
                .await
                .contains("- [x] 14:30 Call the plumber")
        );

        // The person undoes the close in the same moment.
        setup
            .fs
            .atomic_write(Path::new("/vault").join(HOUSE), format!("{line}\n"))
            .await
            .unwrap();
        settle(cx);
        assert_eq!(house_line(&setup).await, line, "the undo stands");
        let note = read(&setup, TODAY).await;
        assert!(note.contains("- [ ] 14:30 Call the plumber"), "{note}");
    }

    #[gpui::test]
    async fn reminders_gone_from_the_index_are_forgotten(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": { "2026-10-21.md": "## Day planner\n" },
            }),
        )
        .await;
        let day = day_sync(&setup, cx);
        assert_eq!(day.read_with(cx, |day, _| day.memory.reminders.len()), 1);

        write(&setup, cx, HOUSE, "").await;
        write(&setup, cx, "reminders.md", "## Open\n\n## Done\n").await;
        assert_eq!(day.read_with(cx, |day, _| day.memory.reminders.len()), 0);
        let memory = read(&setup, ".thock/cache/reminders/day.json").await;
        assert!(!memory.contains("Call the plumber"), "{memory}");
    }

    #[gpui::test]
    async fn midnight_carries_an_overdue_reminder_into_the_new_today(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": { "2026-10-22.md": "## Day planner\n- [ ] Walk\n" },
            }),
        )
        .await;
        assert!(!read(&setup, TOMORROW).await.contains("Reminders"));

        cx.executor()
            .advance_clock(Duration::from_secs(10 * 60 * 60));
        settle(cx);
        let note = read(&setup, TOMORROW).await;
        assert!(
            note.contains(
                "- [ ] Call the plumber · 2026-10-21 14:30 · [[projects/house]] <!--rem:"
            ),
            "{note}"
        );
    }

    #[gpui::test]
    async fn previous_words_survive_a_restart(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30)\n" },
                "daily": { "2026-10-21.md": "## Day planner\n" },
            }),
        )
        .await;
        let memory = read(&setup, ".thock/cache/reminders/day.json").await;
        assert!(memory.contains("Call the plumber"), "{memory}");
        let line = house_line(&setup).await;

        // Forget everything in memory, as a restart would, and change the
        // home line before the next run.
        let day = setup
            .service
            .read_with(cx, |service, _| service.day_sync().cloned())
            .unwrap();
        day.update(cx, |day, cx| {
            day.memory = Memory::default();
            day.memory_loaded = false;
            day.load_memory(cx);
        });
        let renamed = line.replace("Call the plumber", "Call the plumber about the boiler");
        write(&setup, cx, HOUSE, &format!("{renamed}\n")).await;
        let note = read(&setup, TODAY).await;
        assert!(
            note.contains("14:30 Call the plumber about the boiler ·"),
            "{note}"
        );
    }
}
