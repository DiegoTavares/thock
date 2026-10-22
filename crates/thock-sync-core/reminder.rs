//! Reminders (spec `thock/specs/v45-reminders.md` §4, §5.2, §7.1, §11): the
//! `(@…)` token and `<!--rem:…-->` id grammar, the daily-path day rule, where
//! a token counts, mirror lines, state, and the home-line edits every writer
//! makes. The desk and the phone both read and write reminders through these,
//! so a line edited on either device comes out byte for byte the same.
//!
//! Byte ranges are UTF-8 offsets into the line they were read from.

use std::fmt;
use std::ops::Range;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use crate::dates::{names_a_day, parse_date};
use crate::markdown::{
    Document, heading_key, strip_checkbox, strip_list_marker, strip_time_prefix,
};

/// The number of lowercase hex digits in a reminder id.
pub const ID_LENGTH: usize = 12;
/// The day section a daily note gets under its planner, unless
/// `.thock/reminders.toml` names another.
pub const DEFAULT_DAY_HEADING: &str = "Reminders";

const ID_OPEN: &str = "<!--rem:";
const COMMENT_OPEN: &str = "<!--";
const COMMENT_CLOSE: &str = "-->";
const CALENDAR_MARKER: &str = "<!--gcal:";
const TOKEN_OPEN: &str = "(@";
const STRIKE: &str = "~~";
const MIRROR_SEPARATOR: &str = " · ";

/// A vault's `[daily]` notes: the folder and the moment-style filename format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyNotes {
    pub dir: String,
    pub filename: String,
}

impl Default for DailyNotes {
    fn default() -> Self {
        Self {
            dir: "daily".into(),
            filename: "YYYY-MM-DD".into(),
        }
    }
}

impl DailyNotes {
    /// The day of the daily note at the vault-relative `path`, or `None` when
    /// it is not one (spec §4.2). The path below `dir`, without `.md`, is read
    /// back through `filename`; a format with no year, month or day token never
    /// resolves.
    pub fn day_of(&self, path: &str) -> Option<NaiveDate> {
        if !names_a_day(&self.filename) {
            return None;
        }
        let dir = self.dir.trim_matches('/');
        let below = if dir.is_empty() {
            path
        } else {
            path.strip_prefix(dir)?.strip_prefix('/')?
        };
        parse_date(below.strip_suffix(".md")?, &self.filename)
    }
}

/// What a reader needs to know about the vault to read reminders from a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderConfig {
    pub daily: DailyNotes,
    /// The day section's heading names, matched loosely (V26). The first is
    /// the one written.
    pub day_headings: Vec<String>,
}

impl Default for ReminderConfig {
    fn default() -> Self {
        Self {
            daily: DailyNotes::default(),
            day_headings: vec![DEFAULT_DAY_HEADING.into()],
        }
    }
}

/// Whether a token in the vault-relative `path` can be a reminder (spec
/// §4.2): a Markdown file outside `.thock/`, `routines/` and the folder of
/// each template in `template_paths` (`templates/daily.md` excludes
/// `templates/`; a template at the vault root excludes only itself).
pub fn is_reminder_path(path: &str, template_paths: &[&str]) -> bool {
    let is_markdown = path
        .rsplit_once('.')
        .is_some_and(|(stem, extension)| !stem.is_empty() && extension.eq_ignore_ascii_case("md"));
    if !is_markdown || path.starts_with(".thock/") || path.starts_with("routines/") {
        return false;
    }
    !template_paths.iter().any(
        |template| match template.trim_start_matches('/').rsplit_once('/') {
            Some((folder, _)) => path
                .strip_prefix(folder)
                .is_some_and(|rest| rest.starts_with('/')),
            None => path == template.trim_start_matches('/'),
        },
    )
}

/// When a reminder is due: a day, and a time unless it is date-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Due {
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
}

impl Due {
    pub fn new(date: NaiveDate, time: Option<NaiveTime>) -> Self {
        Self {
            date,
            time: time.and_then(|time| time.with_second(0)?.with_nanosecond(0)),
        }
    }

    /// The local wall-clock instant it rings: its time, or `date_only_at`
    /// on its day when it has none.
    pub fn at(&self, date_only_at: NaiveTime) -> NaiveDateTime {
        self.date.and_time(self.time.unwrap_or(date_only_at))
    }

    /// Reads `YYYY-MM-DD` or `YYYY-MM-DD HH:MM` (`H:MM` too), the form
    /// `Display` writes.
    pub fn parse(text: &str) -> Option<Self> {
        match text.split_once(' ') {
            Some((date, time)) => Some(Self::new(parse_iso_date(date)?, Some(parse_clock(time)?))),
            None => Some(Self::new(parse_iso_date(text)?, None)),
        }
    }
}

impl fmt::Display for Due {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.date.format("%Y-%m-%d"))?;
        if let Some(time) = self.time {
            write!(formatter, " {}", time.format("%H:%M"))?;
        }
        Ok(())
    }
}

/// What a token says, as written (spec §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Date(NaiveDate),
    DateTime(NaiveDate, NaiveTime),
    /// `(@HH:MM)`: a day only inside a daily note.
    Time(NaiveTime),
}

impl When {
    /// The due this token names in a note whose day is `day` (a daily note's,
    /// or `None` elsewhere). A time-only token outside a daily note has none.
    pub fn due(&self, day: Option<NaiveDate>) -> Option<Due> {
        match *self {
            Self::Date(date) => Some(Due::new(date, None)),
            Self::DateTime(date, time) => Some(Due::new(date, Some(time))),
            Self::Time(time) => day.map(|day| Due::new(day, Some(time))),
        }
    }
}

/// The token a line carries: where it is and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The token's bytes in the line, including the `~~` around a struck one.
    pub range: Range<usize>,
    pub when: When,
    /// `~~(@…)~~`: closed in prose.
    pub struck: bool,
}

/// A task line's checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Open,
    Done,
}

/// A reminder's state, read from its home line alone (spec §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReminderState {
    /// An unticked task or prose with an unstruck token.
    Open,
    /// A ticked task, or a struck token.
    Closed,
    /// An id with no token left.
    Dropped,
}

/// Where a view lists a reminder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Overdue,
    Today,
    Upcoming,
    Done,
}

/// The group a reminder belongs in on `today`: closed and dropped ones are
/// Done; an open one is Overdue before its day, Today on it and Upcoming
/// after. Groups go by day, not by instant: a reminder whose time (or, when
/// date-only, `date_only_at`) has passed stays in Today until the day ends,
/// and is Overdue from the next day. An open reminder with no day has none.
pub fn group(state: ReminderState, due: Option<Due>, today: NaiveDate) -> Option<Group> {
    if state != ReminderState::Open {
        return Some(Group::Done);
    }
    let due = due?;
    Some(match due.date.cmp(&today) {
        std::cmp::Ordering::Less => Group::Overdue,
        std::cmp::Ordering::Equal => Group::Today,
        std::cmp::Ordering::Greater => Group::Upcoming,
    })
}

/// What one line says about a reminder: its id, its counting token, and
/// whether it is a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineReminder {
    pub id: Option<String>,
    pub token: Option<Token>,
    /// `None` for prose.
    pub task: Option<Task>,
    /// The token's due in this note; `None` with no token, or for a
    /// time-only token outside a daily note (drawn as *which day?*).
    pub due: Option<Due>,
}

impl LineReminder {
    pub fn state(&self) -> ReminderState {
        match &self.token {
            None => ReminderState::Dropped,
            Some(token) if token.struck || self.task == Some(Task::Done) => ReminderState::Closed,
            Some(_) => ReminderState::Open,
        }
    }
}

/// Reads one line (spec §4.1–§4.3). `day` is the note's day when it is a
/// daily note. `None` when the line has neither a token nor an id, or carries
/// a calendar marker. The line is read on its own: use `parse_note` for a
/// whole file, which also skips fences, front matter and comments that span
/// lines.
pub fn parse_line(line: &str, day: Option<NaiveDate>) -> Option<LineReminder> {
    read_line(line, day, false).reminder
}

/// One line of a note that carries a reminder token or id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appearance {
    /// The line's index in the note, counting from 0.
    pub line: usize,
    pub reminder: LineReminder,
    /// A mirror line in a daily note's day section (§7.1): a copy that can
    /// never be a reminder's home.
    pub mirror: bool,
}

/// Every reminder line in the note at the vault-relative `path`, top to
/// bottom. Lines in fences, front matter and comments are text (§4.2).
/// Whether `path` is in scope is `is_reminder_path`'s question, not this one.
pub fn parse_note(text: &str, path: &str, config: &ReminderConfig) -> Vec<Appearance> {
    let document = Document::split(text);
    let day = config.daily.day_of(path);
    let protected = document.protected();
    let day_sections = match day {
        Some(_) => day_sections(&document, &config.day_headings),
        None => Vec::new(),
    };
    let mut appearances = Vec::new();
    let mut in_comment = false;
    for (index, line) in document.lines.iter().enumerate() {
        if protected.get(index).copied().unwrap_or(false) {
            continue;
        }
        let read = read_line(&line.text, day, in_comment);
        in_comment = read.comment_open;
        let Some(reminder) = read.reminder else {
            continue;
        };
        let mirror = reminder.token.is_none()
            && reminder.id.is_some()
            && day_sections.iter().any(|section| section.contains(&index));
        appearances.push(Appearance {
            line: index,
            reminder,
            mirror,
        });
    }
    appearances
}

fn day_sections(document: &Document, names: &[String]) -> Vec<Range<usize>> {
    let headings = document.headings();
    headings
        .iter()
        .filter(|heading| {
            names
                .iter()
                .any(|name| heading_matches(&heading.text, name))
        })
        .map(|heading| {
            let end = headings
                .iter()
                .find(|other| other.index > heading.index && other.level <= heading.level)
                .map_or(document.lines.len(), |other| other.index);
            heading.index + 1..end
        })
        .collect()
}

/// An ATX heading of a note, outside fences and front matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteHeading {
    /// The heading's line index, counting from 0.
    pub row: usize,
    pub level: u8,
    pub text: String,
}

/// Every heading in `text`, top to bottom, the way the sync rules see them
/// (V34 API §7.2).
pub fn note_headings(text: &str) -> Vec<NoteHeading> {
    Document::split(text)
        .headings()
        .into_iter()
        .map(|heading| NoteHeading {
            row: heading.index,
            level: heading.level,
            text: heading.text,
        })
        .collect()
}

/// Heading names match as the planner's do (V26): lowercase equality, or
/// equal `heading_key`s.
pub fn heading_matches(text: &str, name: &str) -> bool {
    if text.trim().to_lowercase() == name.trim().to_lowercase() {
        return true;
    }
    let key = heading_key(name);
    !key.is_empty() && heading_key(text) == key
}

struct ReadLine {
    reminder: Option<LineReminder>,
    /// An HTML comment opened on this line is still open at its end.
    comment_open: bool,
}

fn read_line(line: &str, day: Option<NaiveDate>, in_comment: bool) -> ReadLine {
    let scan = scan_inline(line, in_comment);
    if line.contains(CALENDAR_MARKER) {
        return ReadLine {
            reminder: None,
            comment_open: scan.comment_open,
        };
    }
    let token = last_token(line, &scan.excluded);
    let task = task_of(line).map(|(task, _)| task);
    let reminder = (token.is_some() || scan.id.is_some()).then(|| LineReminder {
        due: token.as_ref().and_then(|token| token.when.due(day)),
        id: scan.id,
        token,
        task,
    });
    ReadLine {
        reminder,
        comment_open: scan.comment_open,
    }
}

struct InlineScan {
    /// Code spans and HTML comments, where a token is text.
    excluded: Vec<Range<usize>>,
    /// The last reminder id among the line's comments.
    id: Option<String>,
    comment_open: bool,
}

/// One pass over a line, left to right: a code span or an HTML comment,
/// whichever opens first, hides what it holds (CommonMark's precedence).
fn scan_inline(line: &str, in_comment: bool) -> InlineScan {
    let mut excluded = Vec::new();
    let mut id = None;
    let mut cursor = 0;
    if in_comment {
        match line.find(COMMENT_CLOSE) {
            Some(close) => {
                cursor = close + COMMENT_CLOSE.len();
                excluded.push(0..cursor);
            }
            None => {
                return InlineScan {
                    excluded: vec![0..line.len()],
                    id: None,
                    comment_open: true,
                };
            }
        }
    }
    let bytes = line.as_bytes();
    while cursor < line.len() {
        let rest = &line[cursor..];
        if let Some(inside) = rest.strip_prefix(COMMENT_OPEN) {
            match inside.find(COMMENT_CLOSE) {
                Some(offset) => {
                    let end = cursor + COMMENT_OPEN.len() + offset + COMMENT_CLOSE.len();
                    if let Some(found) = id_in_comment(&line[cursor..end]) {
                        id = Some(found.to_string());
                    }
                    excluded.push(cursor..end);
                    cursor = end;
                }
                None => {
                    excluded.push(cursor..line.len());
                    return InlineScan {
                        excluded,
                        id,
                        comment_open: true,
                    };
                }
            }
        } else if bytes[cursor] == b'`' {
            let run = rest.bytes().take_while(|byte| *byte == b'`').count();
            match closing_backticks(line, cursor + run, run) {
                Some(end) => {
                    excluded.push(cursor..end);
                    cursor = end;
                }
                None => cursor += run,
            }
        } else {
            cursor += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    InlineScan {
        excluded,
        id,
        comment_open: false,
    }
}

/// The end of the backtick run of exactly `run` that closes a code span
/// opened before `from`.
fn closing_backticks(line: &str, from: usize, run: usize) -> Option<usize> {
    let mut search = from;
    while let Some(offset) = line.get(search..)?.find('`') {
        let start = search + offset;
        let length = line[start..]
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();
        if length == run {
            return Some(start + length);
        }
        search = start + length;
    }
    None
}

fn id_in_comment(comment: &str) -> Option<&str> {
    let id = comment.strip_prefix(ID_OPEN)?.strip_suffix(COMMENT_CLOSE)?;
    is_id(id).then_some(id)
}

/// Whether `text` is a reminder id: twelve lowercase hex digits.
pub fn is_id(text: &str) -> bool {
    text.len() == ID_LENGTH
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The id made of six random bytes, which the caller draws (and draws again
/// when the index already holds it).
pub fn id_from_bytes(bytes: [u8; 6]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `<!--rem:<id>-->`.
pub fn id_marker(id: &str) -> String {
    format!("{ID_OPEN}{id}{COMMENT_CLOSE}")
}

fn last_token(line: &str, excluded: &[Range<usize>]) -> Option<Token> {
    let mut found = None;
    let mut search = 0;
    while let Some(offset) = line.get(search..).and_then(|rest| rest.find(TOKEN_OPEN)) {
        let start = search + offset;
        search = start + TOKEN_OPEN.len();
        if line[..start].ends_with('\\') {
            continue;
        }
        let Some(close) = line[search..].find(')') else {
            break;
        };
        let end = search + close + 1;
        if excluded
            .iter()
            .any(|range| range.start < end && start < range.end)
        {
            continue;
        }
        let Some(when) = parse_when(&line[search..end - 1]) else {
            continue;
        };
        let struck = line[..start].ends_with(STRIKE) && line[end..].starts_with(STRIKE);
        let range = if struck {
            start - STRIKE.len()..end + STRIKE.len()
        } else {
            start..end
        };
        found = Some(Token {
            range,
            when,
            struck,
        });
    }
    found
}

fn parse_when(text: &str) -> Option<When> {
    if let Some((date, time)) = text.split_once(' ') {
        return Some(When::DateTime(parse_iso_date(date)?, parse_clock(time)?));
    }
    if text.contains(':') {
        return parse_clock(text).map(When::Time);
    }
    parse_iso_date(text).map(When::Date)
}

/// `YYYY-MM-DD`, a real calendar date.
fn parse_iso_date(text: &str) -> Option<NaiveDate> {
    let bytes = text.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::from_ymd_opt(
        text[..4].parse().ok()?,
        text[5..7].parse().ok()?,
        text[8..].parse().ok()?,
    )
}

/// `H:MM` or `HH:MM`, 00:00 to 23:59.
fn parse_clock(text: &str) -> Option<NaiveTime> {
    let (hours, minutes) = text.split_once(':')?;
    if !(1..=2).contains(&hours.len())
        || minutes.len() != 2
        || !hours
            .bytes()
            .chain(minutes.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    NaiveTime::from_hms_opt(hours.parse().ok()?, minutes.parse().ok()?, 0)
}

/// A list item's checkbox and the byte offset of the character inside it.
fn task_of(line: &str) -> Option<(Task, usize)> {
    let trimmed = line.trim_start();
    let after_marker = strip_list_marker(trimmed);
    if after_marker.len() == trimmed.len() {
        return None;
    }
    let mark = line.len() - after_marker.len() + 1;
    let task = match after_marker.get(..4)? {
        "[ ] " => Task::Open,
        "[x] " | "[X] " => Task::Done,
        _ => return None,
    };
    Some((task, mark))
}

/// The canonical token for `due` on a line whose note's day is `home_day`:
/// `(@HH:MM)` when the note is the daily note of the due's own day and the
/// due has a time, the full date otherwise.
pub fn token_text(due: Due, home_day: Option<NaiveDate>) -> String {
    match due.time {
        Some(time) if home_day == Some(due.date) => {
            format!("{TOKEN_OPEN}{})", time.format("%H:%M"))
        }
        _ => format!("{TOKEN_OPEN}{due})"),
    }
}

// ---------------------------------------------------------------------------
// Home-line edits (§11, decision 21). Each returns `None` when it does not
// apply to the line, so the caller writes nothing.

/// Closes a reminder: ticks a task, or strikes the token in prose.
pub fn close(line: &str) -> Option<String> {
    let reminder = parse_line(line, None)?;
    if reminder.state() != ReminderState::Open {
        return None;
    }
    if let Some((Task::Open, mark)) = task_of(line) {
        return Some(replace_range(line, mark..mark + 1, "x"));
    }
    let token = reminder.token?;
    let text = &line[token.range.clone()];
    Some(replace_range(
        line,
        token.range,
        &format!("{STRIKE}{text}{STRIKE}"),
    ))
}

/// Reopens a closed reminder: unticks the task and unstrikes the token.
pub fn reopen(line: &str) -> Option<String> {
    let reminder = parse_line(line, None)?;
    if reminder.state() != ReminderState::Closed {
        return None;
    }
    let token = reminder.token?;
    let mut edited = line.to_string();
    if token.struck {
        let inner = token.range.start + STRIKE.len()..token.range.end - STRIKE.len();
        edited = replace_range(&edited, token.range, &line[inner]);
    }
    if let Some((Task::Done, mark)) = task_of(&edited) {
        edited = replace_range(&edited, mark..mark + 1, " ");
    }
    Some(edited)
}

/// Rewrites the token to `due` (`token_text` decides the form), keeping a
/// strike and the checkbox as they are. A dropped line gets the token back
/// before its trailing comments.
pub fn rewrite(line: &str, due: Due, home_day: Option<NaiveDate>) -> Option<String> {
    let reminder = parse_line(line, home_day)?;
    let text = token_text(due, home_day);
    Some(match reminder.token {
        Some(token) if token.struck => {
            replace_range(line, token.range, &format!("{STRIKE}{text}{STRIKE}"))
        }
        Some(token) => replace_range(line, token.range, &text),
        None => insert_before_trailing_comments(line, &text),
    })
}

/// Drops a reminder: removes the token and keeps the words and the id.
pub fn drop_token(line: &str) -> Option<String> {
    let token = parse_line(line, None)?.token?;
    Some(remove_range(line, token.range))
}

/// Makes `line` a reminder due at `due`: the token rewritten, or written
/// before the trailing comments, and `<!--rem:<id>-->` last. A line that
/// already has an id keeps it and `id` is not written.
pub fn attach(line: &str, due: Due, id: &str, home_day: Option<NaiveDate>) -> Option<String> {
    if line.contains(CALENDAR_MARKER) || !is_id(id) {
        return None;
    }
    let with_token = match parse_line(line, home_day) {
        Some(_) => rewrite(line, due, home_day)?,
        None => insert_before_trailing_comments(line, &token_text(due, home_day)),
    };
    Some(add_id(&with_token, id).unwrap_or(with_token))
}

/// Puts `due`'s token on `line` without touching its id: the token
/// rewritten when the line has one, otherwise written before the trailing
/// comments. `None` on a calendar line. The desk's date picker writes this;
/// the index service gives the line its id later.
pub fn place_token(line: &str, due: Due, home_day: Option<NaiveDate>) -> Option<String> {
    if line.contains(CALENDAR_MARKER) {
        return None;
    }
    match parse_line(line, home_day) {
        Some(_) => rewrite(line, due, home_day),
        None => Some(insert_before_trailing_comments(
            line,
            &token_text(due, home_day),
        )),
    }
}

/// Appends `<!--rem:<id>-->` as the line's last comment. `None` when the
/// line already has an id or carries a calendar marker.
pub fn add_id(line: &str, id: &str) -> Option<String> {
    if line.contains(CALENDAR_MARKER) || !is_id(id) {
        return None;
    }
    if parse_line(line, None).is_some_and(|reminder| reminder.id.is_some()) {
        return None;
    }
    let head = line.trim_end();
    Some(if head.is_empty() {
        id_marker(id)
    } else {
        format!("{head} {}", id_marker(id))
    })
}

fn replace_range(line: &str, range: Range<usize>, with: &str) -> String {
    let mut edited = String::with_capacity(line.len() + with.len());
    edited.push_str(&line[..range.start]);
    edited.push_str(with);
    edited.push_str(&line[range.end..]);
    edited
}

/// Removes `range` and one side's whitespace, so the words around it keep
/// one space between them.
fn remove_range(line: &str, range: Range<usize>) -> String {
    let before = &line[..range.start];
    let after = &line[range.end..];
    if after.trim().is_empty() {
        return before.trim_end().to_string();
    }
    if !after.starts_with(char::is_whitespace) {
        return format!("{}{after}", before.trim_end());
    }
    if before.is_empty() || before.ends_with(char::is_whitespace) {
        return format!("{before}{}", after.trim_start());
    }
    format!("{before}{after}")
}

/// The byte offset where the line's trailing run of HTML comments (and the
/// whitespace before them) starts, or the end of its text.
pub fn trailing_comments_start(line: &str) -> usize {
    let mut tail = line.trim_end();
    while tail.ends_with(COMMENT_CLOSE) {
        let Some(open) = tail.rfind(COMMENT_OPEN) else {
            break;
        };
        if open + COMMENT_OPEN.len() > tail.len() - COMMENT_CLOSE.len() {
            break;
        }
        tail = tail[..open].trim_end();
    }
    tail.len()
}

fn insert_before_trailing_comments(line: &str, token: &str) -> String {
    let split = trailing_comments_start(line);
    let head = &line[..split];
    let comments = line[split..].trim();
    let mut edited = String::with_capacity(line.len() + token.len() + 2);
    edited.push_str(head);
    if !head.is_empty() && !head.ends_with(' ') {
        edited.push(' ');
    }
    edited.push_str(token);
    if !comments.is_empty() {
        edited.push(' ');
        edited.push_str(comments);
    }
    edited
}

// ---------------------------------------------------------------------------
// Display text.

/// `text` without its reminder token and its trailing HTML comments,
/// everything else as written: what a row outside the editor shows (§8.5).
pub fn strip_reminder(text: &str) -> String {
    let without_token = match parse_line(text, None).and_then(|reminder| reminder.token) {
        Some(token) => remove_range(text, token.range),
        None => text.to_string(),
    };
    without_token[..trailing_comments_start(&without_token)].to_string()
}

/// Where a line's trailing metadata starts: its run of trailing HTML
/// comments, and the reminder token just before them when there is one.
/// `&text[..trailing_metadata_start(text)]` is the line as a person edits it,
/// and the rest can be put back after an edit.
pub fn trailing_metadata_start(text: &str) -> usize {
    let comments = trailing_comments_start(text);
    match parse_line(text, None).and_then(|reminder| reminder.token) {
        Some(token) if text[token.range.end..comments].trim().is_empty() => {
            text[..token.range.start].trim_end().len()
        }
        _ => comments,
    }
}

/// The words a label shows for a reminder line: no list marker, checkbox,
/// planner time, token or trailing comments (§4.3, §7.1).
pub fn reminder_text(line: &str) -> String {
    let text = strip_reminder(line);
    let trimmed = text.trim_start();
    let text = match strip_list_marker(trimmed) {
        item if item.len() < trimmed.len() => strip_checkbox(item),
        prose => prose,
    };
    strip_time_prefix(text).trim().to_string()
}

/// `text` cut to at most `max_chars` characters on a word boundary, with
/// `…` when anything was cut.
pub fn cut_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let limit = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(index, _)| index);
    let prefix = &text[..limit];
    let cut = match prefix.rfind(char::is_whitespace) {
        Some(space) if space > 0 && !text[limit..].starts_with(char::is_whitespace) => {
            &prefix[..space]
        }
        _ => prefix,
    };
    format!("{}…", cut.trim_end())
}

/// The wikilink target of a vault-relative note path: the path without `.md`.
pub fn note_link(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

/// An index link line, `- [[<note>]] <!--rem:<id>-->`, for a reminder whose
/// home is the note at `path` (§5.1, §10.3).
pub fn index_link_line(path: &str, id: &str) -> String {
    format!("- [[{}]] {}", note_link(path), id_marker(id))
}

// ---------------------------------------------------------------------------
// Mirror lines (§7.1, §7.2).

/// What a struck mirror line says happened to its reminder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorMark {
    /// `~~text~~ → YYYY-MM-DD`: moved to another day.
    Moved(NaiveDate),
    /// `~~text~~ (dropped)`.
    Dropped,
    /// `~~text~~ (removed)`.
    Removed,
}

/// A line in a daily note's day section:
/// `- [ ] [HH:MM ]<body>[ · <carried due>] · [[<note>]] <!--rem:<id>-->`,
/// where the body is the text, or `~~text~~` followed by its mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorLine {
    pub checked: bool,
    /// The planner time: the due's time for a reminder due that day.
    pub time: Option<NaiveTime>,
    pub text: String,
    pub mark: Option<MirrorMark>,
    /// The original due of a reminder carried from an earlier day.
    pub carried: Option<Due>,
    /// The home note's link target (`projects/house`).
    pub note: String,
    pub id: String,
}

impl MirrorLine {
    /// Reads a mirror line, or `None` when the line does not have the shape
    /// (a person edited it, or it is not one).
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.trim_end();
        let (checked, rest) = if let Some(rest) = rest.strip_prefix("- [ ] ") {
            (false, rest)
        } else if let Some(rest) = rest
            .strip_prefix("- [x] ")
            .or_else(|| rest.strip_prefix("- [X] "))
        {
            (true, rest)
        } else {
            return None;
        };
        let (rest, marker) = rest.rsplit_once(' ')?;
        let id = id_in_comment(marker)?.to_string();
        let rest = rest.strip_suffix("]]")?;
        let (rest, note) = rest.rsplit_once(&format!("{MIRROR_SEPARATOR}[["))?;
        let note = note.split('|').next().unwrap_or(note);
        if note.is_empty() || note.contains(['[', ']']) {
            return None;
        }
        let (rest, carried) = match rest.rsplit_once(MIRROR_SEPARATOR) {
            Some((before, due)) => match Due::parse(due) {
                Some(due) => (before, Some(due)),
                None => (rest, None),
            },
            None => (rest, None),
        };
        let (time, body) = match rest.split_once(' ') {
            Some((time, body)) => match parse_clock(time).filter(|_| time.len() == 5) {
                Some(time) => (Some(time), body),
                None => (None, rest),
            },
            None => (None, rest),
        };
        let (text, mark) = parse_mirror_body(body)?;
        Some(Self {
            checked,
            time,
            text: text.to_string(),
            mark,
            carried,
            note: note.to_string(),
            id,
        })
    }
}

fn parse_mirror_body(body: &str) -> Option<(&str, Option<MirrorMark>)> {
    let Some(struck) = body.strip_prefix(STRIKE) else {
        return (!body.is_empty()).then_some((body, None));
    };
    let (text, after) = struck.rsplit_once(STRIKE)?;
    let mark = if after == " (dropped)" {
        MirrorMark::Dropped
    } else if after == " (removed)" {
        MirrorMark::Removed
    } else {
        MirrorMark::Moved(parse_iso_date(after.strip_prefix(" → ")?)?)
    };
    Some((text, Some(mark)))
}

impl fmt::Display for MirrorLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "- [{}] ", if self.checked { "x" } else { " " })?;
        if let Some(time) = self.time {
            write!(formatter, "{} ", time.format("%H:%M"))?;
        }
        match self.mark {
            None => write!(formatter, "{}", self.text)?,
            Some(MirrorMark::Moved(date)) => {
                write!(formatter, "~~{}~~ → {}", self.text, date.format("%Y-%m-%d"))?
            }
            Some(MirrorMark::Dropped) => write!(formatter, "~~{}~~ (dropped)", self.text)?,
            Some(MirrorMark::Removed) => write!(formatter, "~~{}~~ (removed)", self.text)?,
        }
        if let Some(carried) = self.carried {
            write!(formatter, "{MIRROR_SEPARATOR}{carried}")?;
        }
        write!(
            formatter,
            "{MIRROR_SEPARATOR}[[{}]] {}",
            self.note,
            id_marker(&self.id)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    #[test]
    fn a_struck_token_spans_its_tildes() {
        let line = "Got the quote. ~~(@2026-10-18 10:00)~~ <!--rem:0d93be7a1c44-->";
        let reminder = parse_line(line, None).unwrap();
        let token = reminder.token.clone().unwrap();
        assert!(token.struck);
        assert_eq!(&line[token.range], "~~(@2026-10-18 10:00)~~");
        assert_eq!(reminder.state(), ReminderState::Closed);
    }

    #[test]
    fn a_comment_spanning_lines_hides_its_tokens() {
        let text = "<!-- a note\n(@2026-10-18) still a comment -->\nAfter (@2026-10-19)\n";
        let appearances = parse_note(text, "notes/a.md", &ReminderConfig::default());
        assert_eq!(appearances.len(), 1);
        assert_eq!(appearances[0].line, 2);
    }

    #[test]
    fn a_row_drops_the_token_and_every_trailing_comment() {
        assert_eq!(
            strip_reminder(
                "Renew passport (@2026-12-02 09:00) <!--gmail:ab--> <!--rem:3f9a1c0d4e21-->"
            ),
            "Renew passport"
        );
        assert_eq!(
            strip_reminder("Call (@tom) mum (@14:30) now"),
            "Call (@tom) mum now"
        );
        assert_eq!(strip_reminder("a <!-- b --> c"), "a <!-- b --> c");
        assert_eq!(strip_reminder("plain"), "plain");
    }

    #[test]
    fn trailing_metadata_is_the_token_and_comments_at_the_end() {
        let line = "Renew passport (@2026-12-02) <!--rem:3f9a1c0d4e21-->";
        assert_eq!(&line[..trailing_metadata_start(line)], "Renew passport");
        let line = "Call (@2026-12-02) mum <!--x-->";
        assert_eq!(
            &line[..trailing_metadata_start(line)],
            "Call (@2026-12-02) mum"
        );
        assert_eq!(trailing_metadata_start("plain"), 5);
    }

    #[test]
    fn placing_a_token_goes_before_trailing_comments() {
        let due = Due::new(date(2026, 12, 2), Some(time(10, 20)));
        assert_eq!(
            place_token("- [ ] Renew passport <!-- note -->", due, None).as_deref(),
            Some("- [ ] Renew passport (@2026-12-02 10:20) <!-- note -->")
        );
        assert_eq!(
            place_token(
                "Renew (@2026-11-01) soon <!--rem:3f9a1c0d4e21-->",
                due,
                None
            )
            .as_deref(),
            Some("Renew (@2026-12-02 10:20) soon <!--rem:3f9a1c0d4e21-->")
        );
        assert_eq!(
            place_token("Water", due, Some(date(2026, 12, 2))).as_deref(),
            Some("Water (@10:20)")
        );
        assert_eq!(place_token("Standup <!--gcal:abc-->", due, None), None);
    }

    #[test]
    fn edits_round_trip() {
        let line = "- [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->";
        let closed = close(line).unwrap();
        assert_eq!(reopen(&closed).as_deref(), Some(line));
        let prose = "Ask about it (@2026-11-02) <!--rem:7c11e0b4d2a9-->";
        assert_eq!(reopen(&close(prose).unwrap()).as_deref(), Some(prose));
        let moved = rewrite(line, Due::new(date(2026, 10, 22), Some(time(9, 0))), None).unwrap();
        assert_eq!(
            moved,
            "- [ ] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->"
        );
    }

    #[test]
    fn cut_text_breaks_on_a_word() {
        assert_eq!(cut_text("short", 80), "short");
        assert_eq!(cut_text("one two three", 9), "one two…");
        assert_eq!(cut_text("one two three", 7), "one two…");
        assert_eq!(cut_text("abcdefghij", 4), "abcd…");
    }

    #[test]
    fn mirror_lines_round_trip() {
        for line in [
            "- [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->",
            "- [ ] Send the October invoice · [[reminders]] <!--rem:a90c1377d02e-->",
            "- [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]] <!--rem:2f6b08e19a5c-->",
            "- [ ] ~~Call the plumber~~ → 2026-10-23 · [[projects/house]] <!--rem:7d3e55a1c0f2-->",
            "- [x] ~~Old thing~~ (dropped) · [[backlog]] <!--rem:2f6b08e19a5c-->",
        ] {
            let mirror = MirrorLine::parse(line).unwrap_or_else(|| panic!("{line}"));
            assert_eq!(mirror.to_string(), line);
        }
    }
}
