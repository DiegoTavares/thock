//! The day's reminders (spec `thock/specs/v45-reminders.md` §7.1, §7.2): the
//! reconciler that keeps a daily note's `### Reminders` subsection, under the
//! Day Planner, in step with the reminders due that day. It follows calendar
//! sync's rules (`calendar.rs`): the same heading resolution and holds, insert
//! and replace only, and a line the person edited is theirs. Pure
//! string-in/edits-out; `reminder_service.rs` runs it and writes the result.

use chrono::{NaiveDate, NaiveTime};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use thock_sync_core::{
    Due, MirrorLine, MirrorMark, ReminderState, cut_text, note_link, parse_line, reminder_text,
};

use crate::calendar::{LineEdit, LineEditKind};
use crate::day_plan;
use crate::reminders::ReminderSettings;

/// How long a mirror line's text may be, in characters (§7.1).
pub const MIRROR_TEXT_CHARS: usize = 80;

/// What a mirror says when its home line has no words of its own.
const EMPTY_TEXT: &str = "Reminder";

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
/// words and it is frozen. A mirror is never unticked: a tick the person
/// made is a copy tick the service carries to the home line (§5.3).
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
            let checked = mirror.checked || reminder.status == DayStatus::Closed;
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

/// A home line's words as a mirror writes them: cut on a word, with nothing
/// that would read as a marker or a strike (§7.1).
fn mirror_text(text: &str) -> String {
    let clean = text
        .replace("<!--", "")
        .replace("~~", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() {
        return EMPTY_TEXT.to_string();
    }
    cut_text(&clean, MIRROR_TEXT_CHARS)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::apply_line_edits;

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
    fn a_closed_reminder_is_never_inserted_and_a_ticked_mirror_never_unticked() {
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
        let (applied, _) = run(note, &[closed, still_open]);
        assert_eq!(applied, note);
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
        ];
        let (applied, _) = run("## Day planner\n", &reminders);
        let mirrors: Vec<MirrorLine> = applied.lines().filter_map(MirrorLine::parse).collect();
        assert_eq!(mirrors.len(), 3, "{applied}");
        assert!(
            mirrors
                .iter()
                .all(|mirror| mirror.text.chars().count() <= MIRROR_TEXT_CHARS + 1)
        );
        assert!(mirrors.iter().any(|mirror| mirror.text == EMPTY_TEXT));
        let mut ids: Vec<&str> = mirrors.iter().map(|mirror| mirror.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["111111111111", "222222222222", "333333333333"]);
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
