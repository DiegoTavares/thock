//! The reminders index, pure (spec `thock/specs/v45-reminders.md` §5, §6.1):
//! reading `reminders.md` and the notes it links to into reminders, the batch
//! step that keeps the index in step with the vault, and applying the line
//! changes that step plans. No I/O: `reminder_service.rs` feeds it texts and
//! writes what it returns.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use thock_sync_core::{
    Appearance, Due, Group, ReminderConfig, ReminderState, Task, add_id, close, drop_token,
    heading_matches, id_marker, is_id, note_headings, note_link, parse_note, reminder_text, reopen,
    rewrite, trailing_comments_start,
};

use crate::reminders::ReminderSettings;

/// How long an id gone from every note waits before its entry is marked
/// removed, so a cut in one note and a paste in another lands as a move.
pub const REMOVAL_GRACE: chrono::Duration = chrono::Duration::seconds(10);

const INDEX_COMMENT: &str = "<!-- Thock keeps this index. A task here is a reminder; a link points at one that lives in another note. -->";

/// The two index sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IndexSection {
    Open,
    Done,
}

/// Bookkeeping the desk writes on an entry in `## Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StampKind {
    /// `✅ <date>`: closed.
    Done,
    /// `dropped <date>`: the token was removed.
    Dropped,
    /// `removed <date>`: the line is gone from every note.
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Stamp {
    pub kind: StampKind,
    pub date: NaiveDate,
}

impl Stamp {
    fn text(self) -> String {
        let word = match self.kind {
            StampKind::Done => "✅",
            StampKind::Dropped => "dropped",
            StampKind::Removed => "removed",
        };
        format!("{word} {}", self.date.format("%Y-%m-%d"))
    }
}

/// What an index entry is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// `- [[note]] …`: the reminder lives in `home`, a vault-relative path.
    Link { home: String },
    /// A full line: the reminder lives in the index itself.
    Loose,
}

/// One list item under `## Open` or `## Done` that carries a reminder id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub id: String,
    /// The entry's line index in the index file.
    pub row: usize,
    pub section: IndexSection,
    pub kind: EntryKind,
    pub stamp: Option<Stamp>,
}

/// The vault-relative path a wikilink target names: `projects/house` is
/// `projects/house.md`.
pub fn link_path(target: &str) -> String {
    let target = target.trim().trim_start_matches('/');
    if target.to_ascii_lowercase().ends_with(".md") {
        target.to_string()
    } else {
        format!("{target}.md")
    }
}

/// The section ranges of the index: (section, rows below the heading).
fn section_ranges(
    text: &str,
    settings: &ReminderSettings,
) -> Vec<(IndexSection, std::ops::Range<usize>)> {
    let headings = note_headings(text);
    let line_count = text.split('\n').count();
    let mut ranges = Vec::new();
    for heading in &headings {
        let section = if settings
            .open_headings
            .iter()
            .any(|name| heading_matches(&heading.text, name))
        {
            IndexSection::Open
        } else if settings
            .done_headings
            .iter()
            .any(|name| heading_matches(&heading.text, name))
        {
            IndexSection::Done
        } else {
            continue;
        };
        let end = headings
            .iter()
            .find(|other| other.row > heading.row && other.level <= heading.level)
            .map_or(line_count, |other| other.row);
        ranges.push((section, heading.row + 1..end));
    }
    ranges
}

fn list_item_body(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|marker| trimmed.strip_prefix(marker))
}

fn parse_stamp(line: &str) -> Option<Stamp> {
    let head = line[..trailing_comments_start(line)].trim_end();
    let date_text = head.get(head.len().checked_sub(10)?..)?;
    let date = NaiveDate::parse_from_str(date_text, "%Y-%m-%d").ok()?;
    let before = head[..head.len() - 10].trim_end();
    let kind = if before.ends_with('✅') {
        StampKind::Done
    } else if before.ends_with(" dropped") {
        StampKind::Dropped
    } else if before.ends_with(" removed") {
        StampKind::Removed
    } else {
        return None;
    };
    Some(Stamp { kind, date })
}

/// Every entry of the index, top to bottom: list items with a reminder id
/// under the Open and Done headings (matched loosely), outside fences, front
/// matter and comments.
pub fn parse_index(
    text: &str,
    index_path: &str,
    settings: &ReminderSettings,
    config: &ReminderConfig,
) -> Vec<IndexEntry> {
    let ranges = section_ranges(text, settings);
    let lines: Vec<&str> = text.split('\n').collect();
    parse_note(text, index_path, config)
        .into_iter()
        .filter_map(|appearance| {
            let id = appearance.reminder.id.clone()?;
            let section = ranges
                .iter()
                .rev()
                .find(|(_, range)| range.contains(&appearance.line))?
                .0;
            let line = lines.get(appearance.line)?.trim_end_matches('\r');
            let body = list_item_body(line)?;
            let kind = match body
                .strip_prefix("[[")
                .and_then(|rest| rest.split_once("]]"))
            {
                Some((target, _)) => EntryKind::Link {
                    home: link_path(target.split('|').next().unwrap_or(target)),
                },
                None => EntryKind::Loose,
            };
            Some(IndexEntry {
                id,
                row: appearance.line,
                section,
                kind,
                stamp: parse_stamp(line),
            })
        })
        .collect()
}

/// The home line of `id` in a note: the first line that carries the id and
/// a token, else the first that carries the id. Mirror lines never count.
pub fn find_home(text: &str, path: &str, id: &str, config: &ReminderConfig) -> Option<Appearance> {
    let candidates: Vec<Appearance> = parse_note(text, path, config)
        .into_iter()
        .filter(|appearance| !appearance.mirror && appearance.reminder.id.as_deref() == Some(id))
        .collect();
    let with_token = candidates
        .iter()
        .position(|appearance| appearance.reminder.token.is_some());
    let index = with_token.unwrap_or(0);
    candidates.into_iter().nth(index)
}

// ---------------------------------------------------------------------------
// Reading (§6.2).

/// A reminder's state as a reader sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexedState {
    Open,
    Closed,
    Dropped,
    /// No home line, and the entry is stamped removed.
    Removed,
    /// No home line yet: a move that hasn't settled, or a deleted line inside
    /// its grace period. Drawn as *line not found*.
    Missing,
}

/// One reminder of the index, read through its home line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedReminder {
    pub id: String,
    pub section: IndexSection,
    pub kind: EntryKind,
    pub stamp: Option<Stamp>,
    /// The home note, vault-relative.
    pub home: String,
    /// The home line's index, when found.
    pub line: Option<usize>,
    /// The home line as written.
    pub line_text: Option<String>,
    /// The label: `reminder_text` of the home line.
    pub text: Option<String>,
    pub due: Option<Due>,
    /// The home line's checkbox, for rows that draw one.
    pub task: Option<Task>,
    pub state: IndexedState,
}

impl IndexedReminder {
    /// Where a view lists it on `today`; `None` when it has no day or no line.
    pub fn group(&self, today: NaiveDate) -> Option<Group> {
        let state = match self.state {
            IndexedState::Open => ReminderState::Open,
            IndexedState::Closed => ReminderState::Closed,
            IndexedState::Dropped => ReminderState::Dropped,
            IndexedState::Removed => return Some(Group::Done),
            IndexedState::Missing => return None,
        };
        thock_sync_core::group(state, self.due, today)
    }

    /// The local instant it rings, `date_only_at` on its day without a time.
    pub fn rings_at(&self, date_only_at: NaiveTime) -> Option<NaiveDateTime> {
        self.due.map(|due| due.at(date_only_at))
    }

    pub fn is_open(&self) -> bool {
        self.state == IndexedState::Open
    }
}

/// Reads every reminder of the index, in file order. `files` holds the text
/// of each note the index links to (a missing key is a missing file) and
/// need not include the index itself.
pub fn read_index(
    index_path: &str,
    index_text: &str,
    files: &HashMap<String, String>,
    settings: &ReminderSettings,
    config: &ReminderConfig,
) -> Vec<IndexedReminder> {
    parse_index(index_text, index_path, settings, config)
        .into_iter()
        .map(|entry| {
            let home = match &entry.kind {
                EntryKind::Link { home } => home.clone(),
                EntryKind::Loose => index_path.to_string(),
            };
            let text = if home == index_path {
                Some(index_text)
            } else {
                files.get(&home).map(String::as_str)
            };
            let found = text.and_then(|text| {
                let appearance = find_home(text, &home, &entry.id, config)?;
                let line = text
                    .split('\n')
                    .nth(appearance.line)?
                    .trim_end_matches('\r')
                    .to_string();
                Some((appearance, line))
            });
            match found {
                Some((appearance, line)) => IndexedReminder {
                    state: match appearance.reminder.state() {
                        ReminderState::Open => IndexedState::Open,
                        ReminderState::Closed => IndexedState::Closed,
                        ReminderState::Dropped => IndexedState::Dropped,
                    },
                    id: entry.id,
                    section: entry.section,
                    kind: entry.kind,
                    stamp: entry.stamp,
                    home,
                    line: Some(appearance.line),
                    text: Some(reminder_text(&line)),
                    due: appearance.reminder.due,
                    task: appearance.reminder.task,
                    line_text: Some(line),
                },
                None => IndexedReminder {
                    state: if entry
                        .stamp
                        .is_some_and(|stamp| stamp.kind == StampKind::Removed)
                    {
                        IndexedState::Removed
                    } else {
                        IndexedState::Missing
                    },
                    id: entry.id,
                    section: entry.section,
                    kind: entry.kind,
                    stamp: entry.stamp,
                    home,
                    line: None,
                    line_text: None,
                    text: None,
                    due: None,
                    task: None,
                },
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The scan: what each note says about reminders, kept between batches.

/// One reminder line of a note, as the batch step needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanLine {
    pub row: usize,
    pub id: Option<String>,
    /// The line carries a token.
    pub token: bool,
    /// Ticked, or its token struck.
    pub closed: bool,
    /// A mirror line in a daily note's day section.
    pub mirror: bool,
}

impl ScanLine {
    fn state(&self) -> ReminderState {
        match (self.token, self.closed) {
            (false, _) => ReminderState::Dropped,
            (true, true) => ReminderState::Closed,
            (true, false) => ReminderState::Open,
        }
    }
}

/// Every reminder line of a note.
pub fn scan_note(text: &str, path: &str, config: &ReminderConfig) -> Vec<ScanLine> {
    parse_note(text, path, config)
        .into_iter()
        .map(|appearance| {
            let reminder = &appearance.reminder;
            ScanLine {
                row: appearance.line,
                id: reminder.id.clone(),
                token: reminder.token.is_some(),
                closed: reminder.task == Some(Task::Done)
                    || reminder.token.as_ref().is_some_and(|token| token.struck),
                mirror: appearance.mirror,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Changes: what a batch writes, addressed by id so a stale row never edits
// the wrong line.

/// An edit to a reminder's home line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeEdit {
    Close,
    Reopen,
    /// Rewrite the token; the full date unless the home is the new day's
    /// daily note.
    Rewrite(Due),
    /// Remove the token, keep the words and the id.
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Give the line at `row`, if it still reads `expected`, the id.
    AddId {
        row: usize,
        expected: String,
        id: String,
    },
    /// Edit the home line of `id` in this file.
    Home { id: String, edit: HomeEdit },
    /// Append `line` at the end of `section` in the index.
    AppendEntry { section: IndexSection, line: String },
    /// Replace the index entry of `id` with `line`.
    ReplaceEntry { id: String, line: String },
    /// Move the index entry of `id` to the end of `to`, as `line`.
    MoveEntry {
        id: String,
        to: IndexSection,
        line: String,
    },
}

/// The changes one file needs, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChanges {
    pub path: String,
    pub changes: Vec<Change>,
}

/// The index a desk writes the first time it needs one.
pub fn index_skeleton(settings: &ReminderSettings) -> String {
    let open = settings
        .open_headings
        .first()
        .map_or("Open", String::as_str);
    let done = settings
        .done_headings
        .first()
        .map_or("Done", String::as_str);
    format!("# Reminders\n\n{INDEX_COMMENT}\n\n## {open}\n\n## {done}\n")
}

struct Lines {
    lines: Vec<String>,
    endings: Vec<&'static str>,
    ending: &'static str,
}

impl Lines {
    fn split(text: &str) -> Self {
        let mut lines = Vec::new();
        let mut endings = Vec::new();
        let mut crlf = 0;
        let mut lf = 0;
        for piece in text.split_inclusive('\n') {
            if let Some(line) = piece.strip_suffix("\r\n") {
                crlf += 1;
                lines.push(line.to_string());
                endings.push("\r\n");
            } else if let Some(line) = piece.strip_suffix('\n') {
                lf += 1;
                lines.push(line.to_string());
                endings.push("\n");
            } else {
                lines.push(piece.to_string());
                endings.push("");
            }
        }
        Self {
            lines,
            endings,
            ending: if crlf > lf { "\r\n" } else { "\n" },
        }
    }

    fn join(&self) -> String {
        let mut text = String::new();
        for (line, ending) in self.lines.iter().zip(&self.endings) {
            text.push_str(line);
            text.push_str(ending);
        }
        text
    }

    fn insert(&mut self, at: usize, line: String) {
        let at = at.min(self.lines.len());
        if at == self.lines.len()
            && let Some(last) = self.endings.last_mut()
            && last.is_empty()
        {
            *last = self.ending;
            self.lines.push(line);
            self.endings.push("");
            return;
        }
        self.lines.insert(at, line);
        self.endings.insert(at, self.ending);
    }

    fn remove(&mut self, at: usize) {
        if at < self.lines.len() {
            self.lines.remove(at);
            let ending = self.endings.remove(at);
            if at == self.lines.len()
                && ending.is_empty()
                && let Some(last) = self.endings.last_mut()
            {
                *last = "";
            }
        }
    }
}

/// Applies `changes` to a file's text (`None` for a missing file) and returns
/// the new text. Changes that no longer apply (the line moved, the edit has
/// nothing to do) are skipped; line endings are kept.
pub fn apply_changes(
    text: Option<&str>,
    path: &str,
    changes: &[Change],
    settings: &ReminderSettings,
    config: &ReminderConfig,
) -> String {
    let needs_index = changes.iter().any(|change| {
        matches!(
            change,
            Change::AppendEntry { .. } | Change::MoveEntry { .. }
        )
    });
    let initial = match text {
        Some(text) => text.to_string(),
        None if needs_index => index_skeleton(settings),
        None => String::new(),
    };
    let mut lines = Lines::split(&initial);
    for change in changes {
        match change {
            Change::AddId { row, expected, id } => {
                let row = if lines.lines.get(*row) == Some(expected) {
                    Some(*row)
                } else {
                    let matching: Vec<usize> = (0..lines.lines.len())
                        .filter(|index| &lines.lines[*index] == expected)
                        .collect();
                    (matching.len() == 1).then(|| matching[0])
                };
                if let Some(row) = row
                    && let Some(edited) = add_id(&lines.lines[row], id)
                {
                    lines.lines[row] = edited;
                }
            }
            Change::Home { id, edit } => {
                let text = lines.join();
                let Some(home) = find_home(&text, path, id, config) else {
                    continue;
                };
                let day = config.daily.day_of(path);
                let line = &lines.lines[home.line];
                let edited = match edit {
                    HomeEdit::Close => close(line),
                    HomeEdit::Reopen => reopen(line),
                    HomeEdit::Rewrite(due) => rewrite(line, *due, day),
                    HomeEdit::Drop => drop_token(line),
                };
                if let Some(edited) = edited {
                    lines.lines[home.line] = edited;
                }
            }
            Change::AppendEntry { section, line } => {
                append_entry(&mut lines, *section, line.clone(), settings);
            }
            Change::ReplaceEntry { id, line } => {
                if let Some(entry) = entry_row(&lines, path, id, settings, config) {
                    lines.lines[entry] = line.clone();
                }
            }
            Change::MoveEntry { id, to, line } => {
                if let Some(entry) = entry_row(&lines, path, id, settings, config) {
                    lines.remove(entry);
                    append_entry(&mut lines, *to, line.clone(), settings);
                }
            }
        }
    }
    lines.join()
}

fn entry_row(
    lines: &Lines,
    path: &str,
    id: &str,
    settings: &ReminderSettings,
    config: &ReminderConfig,
) -> Option<usize> {
    parse_index(&lines.join(), path, settings, config)
        .into_iter()
        .find(|entry| entry.id == id)
        .map(|entry| entry.row)
}

/// Inserts `line` after the last non-blank line of `section`, creating the
/// heading when the index has none.
fn append_entry(
    lines: &mut Lines,
    section: IndexSection,
    line: String,
    settings: &ReminderSettings,
) {
    let row = section_end(lines, section, settings).unwrap_or_else(|| {
        add_heading(lines, section, settings);
        section_end(lines, section, settings).unwrap_or(lines.lines.len())
    });
    lines.insert(row, line);
}

/// The row a new entry of `section` goes at: after its last non-blank line.
fn section_end(lines: &Lines, section: IndexSection, settings: &ReminderSettings) -> Option<usize> {
    let (_, range) = section_ranges(&lines.join(), settings)
        .into_iter()
        .find(|(found, _)| *found == section)?;
    let mut end = range.start;
    for row in range.clone() {
        if lines
            .lines
            .get(row)
            .is_some_and(|line| !line.trim().is_empty())
        {
            end = row + 1;
        }
    }
    Some(end)
}

fn add_heading(lines: &mut Lines, section: IndexSection, settings: &ReminderSettings) {
    let (names, fallback) = match section {
        IndexSection::Open => (&settings.open_headings, "Open"),
        IndexSection::Done => (&settings.done_headings, "Done"),
    };
    let heading = format!("## {}", names.first().map_or(fallback, String::as_str));
    let done_heading = (section == IndexSection::Open)
        .then(|| {
            section_ranges(&lines.join(), settings)
                .into_iter()
                .find(|(found, _)| *found == IndexSection::Done)
                .map(|(_, range)| range.start - 1)
        })
        .flatten();
    match done_heading {
        Some(row) => {
            lines.insert(row, heading);
            lines.insert(row + 1, String::new());
        }
        None => {
            if lines
                .lines
                .last()
                .is_some_and(|last| !last.trim().is_empty())
            {
                lines.insert(lines.lines.len(), String::new());
            }
            lines.insert(lines.lines.len(), heading);
        }
    }
}

/// Inserts a new empty task, `- [ ] `, at the end of the Open section (the
/// tab's `o`). Returns the new text and the new line's row.
pub fn insert_loose_line(text: Option<&str>, settings: &ReminderSettings) -> (String, usize) {
    let initial = text.map_or_else(|| index_skeleton(settings), str::to_string);
    let mut lines = Lines::split(&initial);
    let row = section_end(&lines, IndexSection::Open, settings).unwrap_or_else(|| {
        add_heading(&mut lines, IndexSection::Open, settings);
        section_end(&lines, IndexSection::Open, settings).unwrap_or(lines.lines.len())
    });
    lines.insert(row, "- [ ] ".to_string());
    (lines.join(), row)
}

// ---------------------------------------------------------------------------
// The batch step (§6.1 steps 2–7).

/// Everything one batch reads.
pub struct BatchInput<'a> {
    pub index_path: &'a str,
    /// The index's text, `None` when the file doesn't exist.
    pub index_text: Option<&'a str>,
    /// Every in-scope note's reminder lines, already updated for this batch.
    /// It includes the index.
    pub scan: &'a BTreeMap<String, Vec<ScanLine>>,
    /// The lines of each changed note before this batch, for copy ticks.
    pub previous: &'a BTreeMap<String, Vec<ScanLine>>,
    /// The text of each note that changed in this batch.
    pub changed: &'a BTreeMap<String, String>,
    /// Rows that must not gain an id yet (a cursor sits on them).
    pub skip_rows: &'a HashMap<String, HashSet<usize>>,
    pub now: NaiveDateTime,
}

/// What a batch carries to the next one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchState {
    /// Ids whose entry has no home line anywhere, since when.
    pub missing_since: HashMap<String, NaiveDateTime>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    /// One entry per file to write, in path order.
    pub changes: Vec<FileChanges>,
    /// When to run again without a file event: the end of a removal grace.
    pub recheck_at: Option<NaiveDateTime>,
}

impl BatchPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

fn link_line(home: &str, id: &str, stamp: Option<Stamp>) -> String {
    match stamp {
        Some(stamp) => format!(
            "- [[{}]] {} {}",
            note_link(home),
            stamp.text(),
            id_marker(id)
        ),
        None => format!("- [[{}]] {}", note_link(home), id_marker(id)),
    }
}

/// A loose line with its stamp set to `stamp` (or removed).
fn restamp(line: &str, stamp: Option<Stamp>) -> String {
    let split = trailing_comments_start(line);
    let mut head = line[..split].trim_end().to_string();
    if parse_stamp(line).is_some() && head.len() >= 10 {
        head.truncate(head.len() - 10);
        let trimmed = head.trim_end();
        let trimmed = trimmed
            .strip_suffix('✅')
            .or_else(|| trimmed.strip_suffix("dropped"))
            .or_else(|| trimmed.strip_suffix("removed"))
            .unwrap_or(trimmed);
        head = trimmed.trim_end().to_string();
    }
    if let Some(stamp) = stamp {
        head.push(' ');
        head.push_str(&stamp.text());
    }
    let comments = line[split..].trim();
    if comments.is_empty() {
        head
    } else {
        format!("{head} {comments}")
    }
}

/// Plans the writes that bring the index in step with the notes: ids for new
/// tokens, a link per new reminder, moves followed, state followed, copy
/// ticks applied to home lines, and hand-deleted links restored. `new_id`
/// draws a candidate id; collisions are drawn again.
pub fn plan_batch(
    input: &BatchInput<'_>,
    state: &mut BatchState,
    settings: &ReminderSettings,
    config: &ReminderConfig,
    new_id: &mut dyn FnMut() -> String,
) -> BatchPlan {
    let today = input.now.date();
    let entries = input
        .index_text
        .map(|text| parse_index(text, input.index_path, settings, config))
        .unwrap_or_default();
    let mut scan = input.scan.clone();
    let mut known: HashSet<String> = scan
        .values()
        .flatten()
        .filter_map(|line| line.id.clone())
        .chain(entries.iter().map(|entry| entry.id.clone()))
        .collect();
    let mut changes: BTreeMap<String, Vec<Change>> = BTreeMap::new();

    // Step 2: ids for tokens that have none.
    for (path, text) in input.changed {
        let rows: Vec<&str> = text.split('\n').collect();
        let skip = input.skip_rows.get(path);
        let Some(lines) = scan.get_mut(path) else {
            continue;
        };
        for line in lines.iter_mut() {
            if !line.token || line.id.is_some() || skip.is_some_and(|skip| skip.contains(&line.row))
            {
                continue;
            }
            let Some(expected) = rows.get(line.row) else {
                continue;
            };
            let id = loop {
                let candidate = new_id();
                if is_id(&candidate) && known.insert(candidate.clone()) {
                    break candidate;
                }
            };
            changes
                .entry(path.clone())
                .or_default()
                .push(Change::AddId {
                    row: line.row,
                    expected: expected.trim_end_matches('\r').to_string(),
                    id: id.clone(),
                });
            line.id = Some(id);
        }
    }

    // Where each id appears as a possible home, path order.
    let mut homes: BTreeMap<&str, Vec<(&str, &ScanLine)>> = BTreeMap::new();
    for (path, lines) in &scan {
        for line in lines {
            if line.mirror {
                continue;
            }
            if let Some(id) = &line.id {
                homes.entry(id).or_default().push((path, line));
            }
        }
    }
    let home_in = |id: &str, path: &str| -> Option<&ScanLine> {
        let candidates: Vec<&ScanLine> = homes
            .get(id)?
            .iter()
            .filter(|(found, _)| *found == path)
            .map(|(_, line)| *line)
            .collect();
        candidates
            .iter()
            .find(|line| line.token)
            .or_else(|| candidates.first())
            .copied()
    };
    let moved_to = |id: &str, from: &str| -> Option<String> {
        homes.get(id)?.iter().find_map(|(path, line)| {
            (line.token && *path != from && *path != input.index_path).then(|| path.to_string())
        })
    };

    let mut index_changes = Vec::new();
    let mut home_changes: BTreeMap<String, Vec<Change>> = BTreeMap::new();
    let mut recheck_at: Option<NaiveDateTime> = None;

    // Step 6 first, so step 5 sees the state the copies set: a tick or an
    // untick on a copy is written to the home line.
    let mut copy_state: HashMap<String, bool> = HashMap::new();
    for (path, lines) in input
        .changed
        .keys()
        .filter_map(|path| Some((path, scan.get(path)?)))
    {
        let previous = input.previous.get(path);
        for line in lines {
            let Some(id) = &line.id else { continue };
            let Some(entry) = entries.iter().find(|entry| &entry.id == id) else {
                continue;
            };
            let home_path = match &entry.kind {
                EntryKind::Link { home } => home.as_str(),
                EntryKind::Loose => input.index_path,
            };
            let Some(home) = home_in(id, home_path) else {
                continue;
            };
            if home_path == path && home.row == line.row {
                continue;
            }
            let before = previous.and_then(|previous| {
                previous
                    .iter()
                    .find(|old| old.id.as_ref() == Some(id) && old.mirror == line.mirror)
            });
            let Some(before) = before else { continue };
            if before.closed == line.closed || home.state() == ReminderState::Dropped {
                continue;
            }
            if home.closed != line.closed {
                home_changes
                    .entry(home_path.to_string())
                    .or_default()
                    .push(Change::Home {
                        id: id.clone(),
                        edit: if line.closed {
                            HomeEdit::Close
                        } else {
                            HomeEdit::Reopen
                        },
                    });
                copy_state.insert(id.clone(), line.closed);
            }
        }
    }

    // Steps 4 and 5: follow moves and state for every entry.
    for entry in &entries {
        let (home_path, loose) = match &entry.kind {
            EntryKind::Link { home } => (home.clone(), false),
            EntryKind::Loose => (input.index_path.to_string(), true),
        };
        let mut target = home_path.clone();
        let mut home = home_in(&entry.id, &home_path);
        if home.is_none() && !loose {
            if let Some(new_home) = moved_to(&entry.id, &home_path) {
                home = home_in(&entry.id, &new_home);
                target = new_home;
            }
        }
        let Some(home) = home else {
            if entry
                .stamp
                .is_some_and(|stamp| stamp.kind == StampKind::Removed)
            {
                state.missing_since.remove(&entry.id);
                continue;
            }
            let since = *state
                .missing_since
                .entry(entry.id.clone())
                .or_insert(input.now);
            if input.now - since >= REMOVAL_GRACE {
                state.missing_since.remove(&entry.id);
                let stamp = Some(Stamp {
                    kind: StampKind::Removed,
                    date: today,
                });
                index_changes.push(Change::MoveEntry {
                    id: entry.id.clone(),
                    to: IndexSection::Done,
                    line: link_line(&home_path, &entry.id, stamp),
                });
            } else {
                let at = since + REMOVAL_GRACE;
                recheck_at = Some(recheck_at.map_or(at, |current| current.min(at)));
            }
            continue;
        };
        state.missing_since.remove(&entry.id);

        let home_state = match copy_state.get(&entry.id) {
            Some(true) => ReminderState::Closed,
            Some(false) => ReminderState::Open,
            None => home.state(),
        };
        let wanted = match home_state {
            ReminderState::Open => (IndexSection::Open, None),
            ReminderState::Closed => (
                IndexSection::Done,
                Some(Stamp {
                    kind: StampKind::Done,
                    date: today,
                }),
            ),
            ReminderState::Dropped => (
                IndexSection::Done,
                Some(Stamp {
                    kind: StampKind::Dropped,
                    date: today,
                }),
            ),
        };
        let retargeted = target != home_path;
        let entry_line = |stamp: Option<Stamp>| -> String {
            if loose {
                let current = input
                    .index_text
                    .and_then(|text| text.split('\n').nth(entry.row))
                    .unwrap_or_default()
                    .trim_end_matches('\r');
                restamp(current, stamp)
            } else {
                link_line(&target, &entry.id, stamp)
            }
        };
        if wanted.0 != entry.section {
            index_changes.push(Change::MoveEntry {
                id: entry.id.clone(),
                to: wanted.0,
                line: entry_line(wanted.1),
            });
        } else if retargeted {
            index_changes.push(Change::ReplaceEntry {
                id: entry.id.clone(),
                line: entry_line(entry.stamp),
            });
        }
    }

    // Steps 3 and 7: a link for every reminder the index doesn't list.
    let listed: HashSet<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    for (id, appearances) in &homes {
        if listed.contains(id) {
            continue;
        }
        let Some((path, line)) = appearances
            .iter()
            .find(|(path, line)| line.token && *path != input.index_path)
        else {
            continue;
        };
        let (section, stamp) = match line.state() {
            ReminderState::Closed => (
                IndexSection::Done,
                Some(Stamp {
                    kind: StampKind::Done,
                    date: today,
                }),
            ),
            _ => (IndexSection::Open, None),
        };
        index_changes.push(Change::AppendEntry {
            section,
            line: link_line(path, id, stamp),
        });
    }

    for (path, home) in home_changes {
        changes.entry(path).or_default().extend(home);
    }
    if !index_changes.is_empty() {
        changes
            .entry(input.index_path.to_string())
            .or_default()
            .extend(index_changes);
    }
    BatchPlan {
        changes: changes
            .into_iter()
            .map(|(path, changes)| FileChanges { path, changes })
            .collect(),
        recheck_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use thock_sync_core::DailyNotes;

    fn config() -> ReminderConfig {
        ReminderConfig::default()
    }

    fn settings() -> ReminderSettings {
        ReminderSettings::default()
    }

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    /// A tiny vault: files by path, a scan of them, and the batch state.
    struct Vault {
        files: BTreeMap<String, String>,
        scan: BTreeMap<String, Vec<ScanLine>>,
        state: BatchState,
        next_id: u64,
    }

    impl Vault {
        fn new(files: &[(&str, &str)]) -> Self {
            let mut vault = Self {
                files: files
                    .iter()
                    .map(|(path, text)| (path.to_string(), text.to_string()))
                    .collect(),
                scan: BTreeMap::new(),
                state: BatchState::default(),
                next_id: 0,
            };
            vault.scan = vault
                .files
                .iter()
                .map(|(path, text)| (path.clone(), scan_note(text, path, &config())))
                .collect();
            vault
        }

        /// Runs batches until nothing is left to write, as the service does
        /// with its own writes, and returns the last plan.
        fn settle(&mut self, changed: &[&str], now: NaiveDateTime) -> BatchPlan {
            let mut changed: Vec<String> = changed.iter().map(|path| path.to_string()).collect();
            for _ in 0..5 {
                let previous: BTreeMap<String, Vec<ScanLine>> = changed
                    .iter()
                    .filter_map(|path| Some((path.clone(), self.scan.get(path)?.clone())))
                    .collect();
                let texts: BTreeMap<String, String> = changed
                    .iter()
                    .filter_map(|path| Some((path.clone(), self.files.get(path)?.clone())))
                    .collect();
                for path in &changed {
                    match self.files.get(path) {
                        Some(text) => {
                            self.scan
                                .insert(path.clone(), scan_note(text, path, &config()));
                        }
                        None => {
                            self.scan.remove(path);
                        }
                    }
                }
                let index_text = self.files.get("reminders.md").cloned();
                let next_id = &mut self.next_id;
                let plan = plan_batch(
                    &BatchInput {
                        index_path: "reminders.md",
                        index_text: index_text.as_deref(),
                        scan: &self.scan,
                        previous: &previous,
                        changed: &texts,
                        skip_rows: &HashMap::new(),
                        now,
                    },
                    &mut self.state,
                    &settings(),
                    &config(),
                    &mut || {
                        *next_id += 1;
                        format!("{:012x}", 0xa00000000000u64 + *next_id)
                    },
                );
                if plan.is_empty() {
                    return plan;
                }
                changed.clear();
                for file in &plan.changes {
                    let text = apply_changes(
                        self.files.get(&file.path).map(String::as_str),
                        &file.path,
                        &file.changes,
                        &settings(),
                        &config(),
                    );
                    self.files.insert(file.path.clone(), text);
                    changed.push(file.path.clone());
                }
            }
            panic!("batches never settled: {:#?}", self.files);
        }

        fn edit(&mut self, path: &str, text: &str) {
            self.files.insert(path.to_string(), text.to_string());
        }

        fn index(&self) -> &str {
            self.files.get("reminders.md").map_or("", String::as_str)
        }
    }

    const NOW: &str = "2026-10-21 14:05:00";

    #[test]
    fn a_new_token_gets_an_id_and_one_link() {
        let mut vault = Vault::new(&[(
            "projects/house.md",
            "# House\n- [ ] Call the plumber (@2026-10-21 14:30)\n",
        )]);
        vault.settle(&["projects/house.md"], at(NOW));
        assert_eq!(
            vault.files["projects/house.md"],
            "# House\n- [ ] Call the plumber (@2026-10-21 14:30) <!--rem:a00000000001-->\n"
        );
        assert_eq!(
            vault.index(),
            format!(
                "# Reminders\n\n{INDEX_COMMENT}\n\n## Open\n- [[projects/house]] <!--rem:a00000000001-->\n\n## Done\n"
            )
        );
        // Nothing more to do on a second pass: one link per reminder.
        assert!(
            vault
                .settle(&["projects/house.md", "reminders.md"], at(NOW))
                .is_empty()
        );
    }

    #[test]
    fn ids_are_drawn_again_on_a_collision() {
        let mut vault = Vault::new(&[(
            "a.md",
            "x (@2026-10-22) <!--rem:a00000000001-->\ny (@2026-10-22)\n",
        )]);
        vault.settle(&["a.md"], at(NOW));
        assert!(vault.files["a.md"].contains("y (@2026-10-22) <!--rem:a00000000002-->"));
    }

    #[test]
    fn the_cursor_line_waits_for_its_id() {
        let vault = Vault::new(&[("a.md", "x (@2026-10-22)\n")]);
        let changed: BTreeMap<String, String> =
            [("a.md".to_string(), vault.files["a.md"].clone())].into();
        let skip: HashMap<String, HashSet<usize>> = [("a.md".to_string(), [0].into())].into();
        let plan = plan_batch(
            &BatchInput {
                index_path: "reminders.md",
                index_text: None,
                scan: &vault.scan,
                previous: &BTreeMap::new(),
                changed: &changed,
                skip_rows: &skip,
                now: at(NOW),
            },
            &mut BatchState::default(),
            &settings(),
            &config(),
            &mut || "a00000000001".into(),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn a_closed_new_reminder_lands_in_done() {
        let mut vault =
            Vault::new(&[("a.md", "- [x] Paid (@2026-10-19) <!--rem:111111111111-->\n")]);
        vault.settle(&["a.md"], at(NOW));
        assert!(
            vault
                .index()
                .ends_with("## Done\n- [[a]] ✅ 2026-10-21 <!--rem:111111111111-->\n")
        );
    }

    #[test]
    fn a_cut_and_paste_retargets_the_link() {
        let mut vault = Vault::new(&[
            ("a.md", "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n"),
            (
                "reminders.md",
                "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            ),
            ("b.md", "# B\n"),
        ]);
        vault.edit("a.md", "");
        vault.edit(
            "b.md",
            "# B\n- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n",
        );
        vault.settle(&["a.md", "b.md"], at(NOW));
        assert_eq!(
            vault.index(),
            "## Open\n- [[b]] <!--rem:111111111111-->\n\n## Done\n"
        );
    }

    #[test]
    fn a_deleted_line_is_removed_after_the_grace() {
        let mut vault = Vault::new(&[
            ("a.md", "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n"),
            (
                "reminders.md",
                "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            ),
        ]);
        vault.edit("a.md", "# nothing left\n");
        let plan = vault.settle(&["a.md"], at(NOW));
        assert_eq!(plan.recheck_at, Some(at("2026-10-21 14:05:10")));
        assert!(vault.index().starts_with("## Open\n- [[a]]"));

        vault.settle(&[], at("2026-10-21 14:05:09"));
        assert!(
            vault.index().starts_with("## Open\n- [[a]]"),
            "still waiting"
        );

        vault.settle(&[], at("2026-10-21 14:05:10"));
        assert_eq!(
            vault.index(),
            "## Open\n\n## Done\n- [[a]] removed 2026-10-21 <!--rem:111111111111-->\n"
        );
        assert!(vault.settle(&[], at("2026-10-21 14:10:00")).is_empty());
    }

    #[test]
    fn a_line_back_within_the_grace_is_never_removed() {
        let mut vault = Vault::new(&[
            ("a.md", "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n"),
            (
                "reminders.md",
                "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            ),
        ]);
        vault.edit("a.md", "");
        vault.settle(&["a.md"], at(NOW));
        vault.edit("a.md", "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n");
        vault.settle(&["a.md"], at("2026-10-21 14:05:05"));
        assert!(vault.settle(&[], at("2026-10-21 14:06:00")).is_empty());
        assert!(vault.index().starts_with("## Open\n- [[a]]"));
    }

    #[test]
    fn closing_dropping_and_reopening_move_the_entry() {
        let mut vault = Vault::new(&[
            (
                "a.md",
                "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\nAsk (@2026-10-23) <!--rem:222222222222-->\n",
            ),
            (
                "reminders.md",
                "## Open\n- [[a]] <!--rem:111111111111-->\n- [[a]] <!--rem:222222222222-->\n\n## Done\n",
            ),
        ]);
        vault.edit(
            "a.md",
            "- [x] Call (@2026-10-22) <!--rem:111111111111-->\nAsk <!--rem:222222222222-->\n",
        );
        vault.settle(&["a.md"], at(NOW));
        assert_eq!(
            vault.index(),
            "## Open\n\n## Done\n- [[a]] ✅ 2026-10-21 <!--rem:111111111111-->\n- [[a]] dropped 2026-10-21 <!--rem:222222222222-->\n"
        );
        vault.edit(
            "a.md",
            "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\nAsk <!--rem:222222222222-->\n",
        );
        vault.settle(&["a.md"], at(NOW));
        assert_eq!(
            vault.index(),
            "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n- [[a]] dropped 2026-10-21 <!--rem:222222222222-->\n"
        );
    }

    #[test]
    fn a_tick_on_a_copy_ticks_the_home_line() {
        let mut vault = Vault::new(&[
            (
                "projects/house.md",
                "- [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->\n",
            ),
            (
                "daily/2026-10-21.md",
                "## Day planner\n\n### Reminders\n- [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n",
            ),
            (
                "reminders.md",
                "## Open\n- [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\n## Done\n",
            ),
        ]);
        vault.edit(
            "daily/2026-10-21.md",
            "## Day planner\n\n### Reminders\n- [x] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n",
        );
        vault.settle(&["daily/2026-10-21.md"], at(NOW));
        assert_eq!(
            vault.files["projects/house.md"],
            "- [x] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->\n"
        );
        assert!(
            vault
                .index()
                .ends_with("## Done\n- [[projects/house]] ✅ 2026-10-21 <!--rem:7d3e55a1c0f2-->\n")
        );
    }

    #[test]
    fn a_hand_deleted_link_comes_back() {
        let mut vault = Vault::new(&[
            ("a.md", "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n"),
            (
                "reminders.md",
                "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            ),
        ]);
        vault.edit("reminders.md", "## Open\n\n## Done\n");
        vault.settle(&["reminders.md"], at(NOW));
        assert_eq!(
            vault.index(),
            "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n"
        );
    }

    #[test]
    fn a_phone_line_is_taken_as_it_is() {
        let phone = "## Open\n- [ ] Call the bank (@2026-10-22 10:00) <!--rem:3f9a1c0d4e21-->\n";
        let mut vault = Vault::new(&[("reminders.md", phone)]);
        assert!(vault.settle(&["reminders.md"], at(NOW)).is_empty());
        assert_eq!(vault.index(), phone);

        // Ticked on the phone, it moves to Done with its stamp; the desk adds
        // the Done heading the phone never wrote.
        vault.edit(
            "reminders.md",
            "## Open\n- [x] Call the bank (@2026-10-22 10:00) <!--rem:3f9a1c0d4e21-->\n",
        );
        vault.settle(&["reminders.md"], at(NOW));
        assert_eq!(
            vault.index(),
            "## Open\n\n## Done\n- [x] Call the bank (@2026-10-22 10:00) ✅ 2026-10-21 <!--rem:3f9a1c0d4e21-->\n"
        );
    }

    #[test]
    fn a_loose_line_without_an_id_gets_one_in_place() {
        let mut vault = Vault::new(&[("reminders.md", "## Open\n- [ ] Invoice (@2026-10-21)\n")]);
        vault.settle(&["reminders.md"], at(NOW));
        assert_eq!(
            vault.index(),
            "## Open\n- [ ] Invoice (@2026-10-21) <!--rem:a00000000001-->\n"
        );
    }

    #[test]
    fn changes_keep_crlf_and_skip_stale_rows() {
        let text = "a\r\nx (@2026-10-22)\r\n";
        let changed = apply_changes(
            Some(text),
            "a.md",
            &[
                Change::AddId {
                    row: 0,
                    expected: "x (@2026-10-22)".into(),
                    id: "111111111111".into(),
                },
                Change::AddId {
                    row: 5,
                    expected: "gone".into(),
                    id: "222222222222".into(),
                },
            ],
            &settings(),
            &config(),
        );
        assert_eq!(changed, "a\r\nx (@2026-10-22) <!--rem:111111111111-->\r\n");
    }

    #[test]
    fn home_edits_find_the_line_by_id() {
        let text = "# Day\n### Reminders\n- [ ] Copy · [[a]] <!--rem:111111111111-->\n- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n";
        let due = Due::new(
            NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
            NaiveTime::from_hms_opt(16, 0, 0),
        );
        let path = "daily/2026-10-21.md";
        let rewritten = apply_changes(
            Some(text),
            path,
            &[Change::Home {
                id: "111111111111".into(),
                edit: HomeEdit::Rewrite(due),
            }],
            &settings(),
            &config(),
        );
        assert!(
            rewritten.ends_with("- [ ] Call (@16:00) <!--rem:111111111111-->\n"),
            "{rewritten}"
        );
        let dropped = apply_changes(
            Some(&rewritten),
            path,
            &[Change::Home {
                id: "111111111111".into(),
                edit: HomeEdit::Drop,
            }],
            &settings(),
            &config(),
        );
        assert!(dropped.ends_with("- [ ] Call <!--rem:111111111111-->\n"));
    }

    #[test]
    fn loose_lines_go_under_open() {
        let (text, row) = insert_loose_line(None, &settings());
        assert_eq!(row, 5);
        assert_eq!(text.split('\n').nth(row), Some("- [ ] "));
        let (text, row) = insert_loose_line(
            Some("## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n"),
            &settings(),
        );
        assert_eq!(
            (text.as_str(), row),
            (
                "## Open\n- [[a]] <!--rem:111111111111-->\n- [ ] \n\n## Done\n",
                2
            )
        );
        let (text, row) = insert_loose_line(Some("# Mine\n"), &settings());
        assert_eq!((text.as_str(), row), ("# Mine\n\n## Open\n- [ ] \n", 3));
    }

    #[derive(Deserialize)]
    struct FixtureCase {
        name: String,
        now: String,
        index: String,
        headings: FixtureHeadings,
        daily: FixtureDaily,
        files: HashMap<String, String>,
        reminders: Vec<FixtureReminder>,
    }

    #[derive(Deserialize)]
    struct FixtureHeadings {
        open: Vec<String>,
        done: Vec<String>,
        day: Vec<String>,
    }

    #[derive(Deserialize)]
    struct FixtureDaily {
        dir: String,
        filename: String,
    }

    #[derive(Deserialize)]
    struct FixtureReminder {
        id: String,
        section: String,
        entry: String,
        home: String,
        line: Option<usize>,
        text: Option<String>,
        due: Option<String>,
        rings_at: Option<String>,
        state: String,
        group: Option<String>,
    }

    #[test]
    fn the_reader_passes_every_index_fixture() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../thock-sync-core/fixtures/v1/reminder_index");
        let mut cases = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let case: FixtureCase =
                serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap())
                    .unwrap();
            let settings = ReminderSettings {
                index: case.index.clone(),
                open_headings: case.headings.open,
                done_headings: case.headings.done,
                day_headings: case.headings.day.clone(),
                ..ReminderSettings::default()
            };
            let config = ReminderConfig {
                daily: DailyNotes {
                    dir: case.daily.dir,
                    filename: case.daily.filename,
                },
                day_headings: case.headings.day,
            };
            let now = NaiveDateTime::parse_from_str(&case.now, "%Y-%m-%dT%H:%M").unwrap();
            let read = read_index(
                &case.index,
                &case.files[&case.index],
                &case.files,
                &settings,
                &config,
            );
            assert_eq!(read.len(), case.reminders.len(), "{}", case.name);
            for (reminder, expected) in read.iter().zip(&case.reminders) {
                let label = format!("{} {}", case.name, expected.id);
                assert_eq!(reminder.id, expected.id, "{label}");
                let section = match reminder.section {
                    IndexSection::Open => "open",
                    IndexSection::Done => "done",
                };
                assert_eq!(section, expected.section, "{label}");
                let entry = match reminder.kind {
                    EntryKind::Link { .. } => "link",
                    EntryKind::Loose => "loose",
                };
                assert_eq!(entry, expected.entry, "{label}");
                assert_eq!(reminder.home, expected.home, "{label}");
                assert_eq!(reminder.line, expected.line, "{label}");
                assert_eq!(reminder.text, expected.text, "{label}");
                assert_eq!(
                    reminder.due.map(|due| due.to_string()),
                    expected.due,
                    "{label}"
                );
                assert_eq!(
                    reminder
                        .rings_at(settings.date_only_at)
                        .map(|at| at.format("%Y-%m-%dT%H:%M").to_string()),
                    expected.rings_at,
                    "{label}"
                );
                assert_eq!(
                    format!("{:?}", reminder.state).to_lowercase(),
                    expected.state,
                    "{label}"
                );
                assert_eq!(
                    reminder
                        .group(now.date())
                        .map(|group| format!("{group:?}").to_lowercase()),
                    expected.group,
                    "{label}"
                );
            }
            cases += 1;
        }
        assert!(cases >= 2);
    }
}
