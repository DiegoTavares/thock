//! Calendar sync into the daily note (spec `v8-calendar-sync.md`): the
//! provider trait, the `.thock/calendar.toml` config, marker-id derivation,
//! and — the contract of the feature — the reconciler that maintains the
//! `## Calendar` subsection of the Day Planner. Everything here is pure
//! string-in/string-out (no network, no I/O), so every branch is
//! unit-testable; the GPUI service lives in `calendar_service.rs`.

use anyhow::Result;
use chrono::NaiveDate;
use gpui::{AsyncApp, Task};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::ops::Range;
use std::time::Duration;

use crate::day_plan;

/// Lives next to `config.toml` in `.thock/`. A separate file, not a
/// `[calendar]` table: `config.toml` is `deny_unknown_fields`, so a new table
/// there would make older builds declare the whole vault invalid (spec §7.1).
pub const CALENDAR_CONFIG_FILE: &str = "calendar.toml";

const MARKER_PREFIX: &str = "<!--gcal:";
const MARKER_SUFFIX: &str = "-->";
const CANCELLED_SUFFIX: &str = "~~ (cancelled)";

/// A normalized event: a time, a title, an id, and Google's event type —
/// nothing else survives normalization (spec §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    /// The short digest id that travels in the line marker
    /// (`event_marker_id`).
    pub id: String,
    pub title: String,
    /// Local minutes since midnight, clamped to the day; `None` for all-day
    /// events, which are written without a time token.
    pub time: Option<(u32, u32)>,
    pub kind: EventKind,
}

/// Google's `eventType`, narrowed to the distinction the planner acts on.
/// Focus time and out of office are *status* blocks: wide containers for the
/// day rather than things to do, so the Day Planner gives them their own
/// narrow lane instead of letting them squeeze every real block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EventKind {
    #[default]
    Default,
    FocusTime,
    OutOfOffice,
}

impl EventKind {
    /// Maps a Google `eventType` value. Types the planner treats like any
    /// meeting (`default`, `birthday`, `fromGmail`, `workingLocation`, and
    /// anything Google adds later) all land on `Default`.
    pub fn from_google(event_type: &str) -> Self {
        match event_type {
            "focusTime" => Self::FocusTime,
            "outOfOffice" => Self::OutOfOffice,
            _ => Self::Default,
        }
    }

    /// The marker suffix that records this kind in the note. `Default` has
    /// none, so an ordinary line's marker stays exactly as V8 wrote it.
    fn marker_suffix(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::FocusTime => Some("focus"),
            Self::OutOfOffice => Some("ooo"),
        }
    }

    fn from_marker_suffix(suffix: &str) -> Option<Self> {
        match suffix {
            "focus" => Some(Self::FocusTime),
            "ooo" => Some(Self::OutOfOffice),
            _ => None,
        }
    }

    /// Whether the Day Planner lays this out in its low-weight lane.
    pub fn is_status(self) -> bool {
        self != Self::Default
    }
}

/// A provider's answer for one day.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
    /// Events overlapping the local day, already normalized and filtered.
    Events(Vec<CalendarEvent>),
    /// The provider proved nothing moved (every calendar answered 304).
    Unchanged,
}

/// Transport abstraction (spec §4.3): Google REST is the V8 implementation;
/// EventKit or an MCP connector can follow without touching the reconciler.
pub trait CalendarProvider: Send + Sync {
    /// Events overlapping the local day, already normalized and filtered.
    /// `Fetched::Unchanged` means the provider proved nothing moved.
    fn fetch_day(&self, date: NaiveDate, cx: &AsyncApp) -> Task<Result<Fetched>>;
}

/// Which fetched events get written into the note (spec §7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilters {
    pub accepted_only: bool,
    pub include_solo: bool,
    pub all_day: bool,
    pub private_busy: bool,
}

impl Default for EventFilters {
    fn default() -> Self {
        Self {
            accepted_only: true,
            include_solo: true,
            all_day: false,
            private_busy: false,
        }
    }
}

/// Optional `[google]` override of the bundled desktop OAuth client
/// (spec §6.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoogleClientOverride {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

/// Resolved `.thock/calendar.toml` plus the planner heading it reconciles
/// under (which lives in `config.toml`, not here).
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarConfig {
    pub account: Option<String>,
    /// Calendar ids to sync; empty means nothing syncs.
    pub calendars: Vec<String>,
    /// Child heading of the planner section that the syncer maintains.
    pub section: String,
    pub poll_interval: Duration,
    pub filters: EventFilters,
    pub google: GoogleClientOverride,
    /// `[day_planner].heading` from the vault config.
    pub planner_heading: String,
}

impl CalendarConfig {
    pub fn with_planner_heading(planner_heading: &str) -> Self {
        Self {
            account: None,
            calendars: Vec::new(),
            section: "Calendar".to_string(),
            poll_interval: Duration::from_secs(300),
            filters: EventFilters::default(),
            google: GoogleClientOverride::default(),
            planner_heading: planner_heading.to_string(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CalendarConfigContent {
    schema: Option<u32>,
    account: Option<String>,
    calendars: Option<Vec<String>>,
    section: Option<String>,
    poll_seconds: Option<u64>,
    filters: FiltersContent,
    google: GoogleContent,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FiltersContent {
    accepted_only: Option<bool>,
    include_solo: Option<bool>,
    all_day: Option<bool>,
    private_busy: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GoogleContent {
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// Parses `.thock/calendar.toml` (spec §7.1). Every field is optional;
/// unknown fields are ignored so future keys don't break this build. An
/// unparseable file is the caller's cue to log and disable sync — never a
/// panic.
pub fn parse_calendar_config(text: &str, planner_heading: &str) -> Result<CalendarConfig> {
    let content: CalendarConfigContent = toml::from_str(text)?;
    let defaults = EventFilters::default();
    Ok(CalendarConfig {
        account: content.account.filter(|account| !account.trim().is_empty()),
        calendars: content.calendars.unwrap_or_default(),
        section: content
            .section
            .filter(|section| !section.trim().is_empty())
            .unwrap_or_else(|| "Calendar".to_string()),
        poll_interval: Duration::from_secs(content.poll_seconds.unwrap_or(300).clamp(60, 3600)),
        filters: EventFilters {
            accepted_only: content.filters.accepted_only.unwrap_or(defaults.accepted_only),
            include_solo: content.filters.include_solo.unwrap_or(defaults.include_solo),
            all_day: content.filters.all_day.unwrap_or(defaults.all_day),
            private_busy: content.filters.private_busy.unwrap_or(defaults.private_busy),
        },
        google: GoogleClientOverride {
            client_id: content.google.client_id,
            client_secret: content.google.client_secret,
        },
        planner_heading: planner_heading.to_string(),
    })
}

/// The id that travels in a synced line's marker: the first 12 hex characters
/// of `sha256(calendar_id + "\0" + event_id)` (spec §5.2). Google event ids
/// are long and recurring instances carry a timestamp suffix; a short digest
/// keeps the line readable while staying stable across renames and moves.
pub fn event_marker_id(calendar_id: &str, event_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(calendar_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(event_id.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(6)
        .fold(String::with_capacity(12), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The marker a synced line ends with: the event id, plus a suffix for the
/// kinds the planner lays out differently (`<!--gcal:9f2c1ab4e7d0:focus-->`).
fn render_marker(id: &str, kind: EventKind) -> String {
    let mut marker = String::from(MARKER_PREFIX);
    marker.push_str(id);
    if let Some(suffix) = kind.marker_suffix() {
        marker.push(':');
        marker.push_str(suffix);
    }
    marker.push_str(MARKER_SUFFIX);
    marker
}

/// The `(id, kind suffix)` of a well-formed marker payload. The id must be
/// hex; an unrecognized suffix is returned verbatim so a marker written by a
/// newer build still identifies its line instead of being re-inserted.
fn parse_marker_payload(payload: &str) -> Option<(&str, Option<&str>)> {
    let (id, suffix) = match payload.split_once(':') {
        Some((id, suffix)) => (id, Some(suffix)),
        None => (payload, None),
    };
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if suffix.is_some_and(|suffix| {
        suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_lowercase())
    }) {
        return None;
    }
    Some((id, suffix))
}

/// The event kind recorded in a line's trailing sync marker. Lines the syncer
/// never wrote — a hand-typed task — are `Default`, so the planner can ask
/// this of any line.
pub fn line_event_kind(line: &str) -> EventKind {
    let trimmed = line.trim_end();
    let Some(marker_start) = trimmed.rfind(MARKER_PREFIX) else {
        return EventKind::default();
    };
    trimmed[marker_start + MARKER_PREFIX.len()..]
        .strip_suffix(MARKER_SUFFIX)
        .and_then(parse_marker_payload)
        .and_then(|(_, suffix)| suffix)
        .and_then(EventKind::from_marker_suffix)
        .unwrap_or_default()
}

/// One edit to the note, in the note's *original* row coordinates.
/// `Insert` places a new line before original row `row` (`row == line count`
/// appends at the end); `Replace` rewrites that row. There is deliberately no
/// `Delete` (spec §8.3). Apply with [`apply_line_edits`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineEdit {
    pub row: usize,
    pub kind: LineEditKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineEditKind {
    Insert(String),
    Replace(String),
}

/// A synced line whose title no longer matches its event: the user renamed
/// it, the rename wins, and the line is frozen (spec §8.4). Reported so the
/// service can record the reason in the sync log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub id: String,
    pub line: String,
    pub event_title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reconciled {
    /// The planner heading is absent from the note; sync holds rather than
    /// inventing one (spec §5.1 rule 4).
    NoPlannerSection,
    Edits {
        edits: Vec<LineEdit>,
        diverged: Vec<Divergence>,
    },
}

/// The contract (spec §8): computes the minimal ordered edit list that brings
/// the note's Calendar section in line with `events`. Inserts new meetings at
/// their sorted position, corrects times on moved ones, marks cancelled ones,
/// and never touches a line the user has edited. Pure; idempotent
/// (`reconcile(apply(note, reconcile(note, events)), events)` is empty).
pub fn reconcile(note: &str, events: &[CalendarEvent], config: &CalendarConfig) -> Reconciled {
    let lines: Vec<&str> = note.lines().collect();
    let Some((planner_range, planner_level)) =
        day_plan::planner_section(&lines, &config.planner_heading)
    else {
        return Reconciled::NoPlannerSection;
    };

    // Two calendars can carry the same derived id; the first occurrence wins.
    let mut seen = HashSet::new();
    let events: Vec<CalendarEvent> = events
        .iter()
        .filter(|event| seen.insert(event.id.clone()))
        .map(|event| CalendarEvent {
            id: event.id.clone(),
            title: sanitize_title(&event.title),
            time: event.time,
            kind: event.kind,
        })
        .collect();

    let section = find_child_section(
        &lines,
        &planner_range,
        planner_level,
        &config.section,
    );

    let mut edits = Vec::new();
    let mut diverged = Vec::new();

    let Some(section_range) = section else {
        // Created only on the first sync that yields at least one event —
        // never speculatively (spec §5.1 rule 2, G6). A level-6 planner
        // heading can have no child heading, so creating one would terminate
        // the planner section and re-duplicate events on every poll; the
        // events simply hold instead.
        if !events.is_empty() && planner_level < 6 {
            edits.extend(section_creation_edits(
                &lines,
                &planner_range,
                planner_level,
                &config.section,
                &events,
            ));
        }
        return Reconciled::Edits { edits, diverged };
    };

    let synced: Vec<SyncedLine> = section_range
        .clone()
        .filter_map(|row| lines.get(row).and_then(|line| SyncedLine::parse(row, line)))
        .collect();
    let mut lines_by_id: HashMap<&str, &SyncedLine> = HashMap::new();
    for line in &synced {
        lines_by_id.entry(line.id.as_str()).or_insert(line);
    }
    let event_ids: HashSet<&str> = events.iter().map(|event| event.id.as_str()).collect();

    let mut inserts: Vec<&CalendarEvent> = Vec::new();
    for event in &events {
        let Some(line) = lines_by_id.get(event.id.as_str()) else {
            inserts.push(event);
            continue;
        };
        if let Some(replacement) = line.updated_for(event) {
            if replacement != line.line {
                edits.push(LineEdit {
                    row: line.row,
                    kind: LineEditKind::Replace(replacement),
                });
            }
        } else {
            diverged.push(Divergence {
                id: event.id.clone(),
                line: line.line.clone(),
                event_title: event.title.clone(),
            });
        }
    }

    // An id in the note but absent from the fetch is a cancellation — a mark,
    // never a delete (spec §8.3).
    for line in &synced {
        if !event_ids.contains(line.id.as_str()) && !line.is_cancelled() {
            edits.push(LineEdit {
                row: line.row,
                kind: LineEditKind::Replace(line.cancelled_form()),
            });
        }
    }

    // Sorting applies to inserts only; existing lines are never reordered
    // (spec §8.2).
    inserts.sort_by(|a, b| event_sort_key(a).cmp(&event_sort_key(b)));
    // Past the last synced line means past the whole section content, not
    // right after that line — the user may have nested sub-bullets under it.
    let fallback_row = after_last_content_row(&lines, &section_range);
    for event in inserts {
        let key = sort_start(event.time);
        let row = synced
            .iter()
            .find(|line| line.sort_start > key)
            .map(|line| line.row)
            .unwrap_or(fallback_row);
        edits.push(LineEdit {
            row,
            kind: LineEditKind::Insert(render_synced_line(event)),
        });
    }

    edits.sort_by_key(|edit| edit.row);
    Reconciled::Edits { edits, diverged }
}

/// Applies an edit list from [`reconcile`] to the note text. Inserts land
/// before their original row; multiple inserts at one row keep list order.
pub fn apply_line_edits(note: &str, edits: &[LineEdit]) -> String {
    let lines: Vec<&str> = note.lines().collect();
    let mut replacements: HashMap<usize, &str> = HashMap::new();
    let mut inserts: HashMap<usize, Vec<&str>> = HashMap::new();
    for edit in edits {
        match &edit.kind {
            LineEditKind::Insert(text) => inserts.entry(edit.row).or_default().push(text),
            LineEditKind::Replace(text) => {
                replacements.insert(edit.row, text);
            }
        }
    }
    let mut output: Vec<&str> = Vec::with_capacity(lines.len() + edits.len());
    for (row, line) in lines.iter().enumerate() {
        if let Some(inserted) = inserts.remove(&row) {
            output.extend(inserted);
        }
        output.push(replacements.get(&row).copied().unwrap_or(line));
    }
    let mut trailing: Vec<(usize, Vec<&str>)> = inserts.into_iter().collect();
    trailing.sort_by_key(|(row, _)| *row);
    for (_, inserted) in trailing {
        output.extend(inserted);
    }
    let mut text = output.join("\n");
    if note.ends_with('\n') || (note.is_empty() && !text.is_empty()) {
        text.push('\n');
    }
    text
}

fn sort_start(time: Option<(u32, u32)>) -> Option<u32> {
    // `None < Some(_)`: all-day events sort before timed ones.
    time.map(|(start, _)| start)
}

fn event_sort_key(event: &CalendarEvent) -> (Option<u32>, Option<u32>, &str, &str) {
    (
        sort_start(event.time),
        event.time.map(|(_, end)| end),
        event.title.as_str(),
        event.id.as_str(),
    )
}

/// The title as it may appear in a note line: no `<!--` (a hostile calendar
/// entry cannot forge a marker) and no newlines (spec §5.2).
fn sanitize_title(title: &str) -> String {
    title
        .replace("<!--", "")
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn format_time_token(start: u32, end: u32) -> String {
    format!(
        "{:02}:{:02} - {:02}:{:02}",
        start / 60,
        start % 60,
        end / 60,
        end % 60
    )
}

fn render_synced_line(event: &CalendarEvent) -> String {
    let mut line = String::from("- [ ] ");
    if let Some((start, end)) = event.time {
        line.push_str(&format_time_token(start, end));
        line.push(' ');
    }
    if !event.title.is_empty() {
        line.push_str(&event.title);
        line.push(' ');
    }
    line.push_str(&render_marker(&event.id, event.kind));
    line
}

/// The `config.section` child heading inside the planner section: matched
/// case-insensitively at exactly one level below the planner heading, located
/// by heading text, never by remembered line number (spec §5.1 rules 1, 3).
/// Returns the content range below the heading.
fn find_child_section(
    lines: &[&str],
    planner_range: &Range<usize>,
    planner_level: usize,
    section: &str,
) -> Option<Range<usize>> {
    let wanted = section.trim().to_lowercase();
    let child_level = planner_level + 1;
    let start = planner_range.clone().find(|&row| {
        day_plan::heading_level_and_text(lines[row])
            .is_some_and(|(level, text)| level == child_level && text.to_lowercase() == wanted)
    })?;
    let end = (start + 1..planner_range.end)
        .find(|&row| {
            day_plan::heading_level_and_text(lines[row])
                .is_some_and(|(level, _)| level <= child_level)
        })
        .unwrap_or(planner_range.end);
    Some(start + 1..end)
}

/// The row just past the last non-blank line of `range` — where fresh synced
/// lines land in a section that has no synced lines yet.
fn after_last_content_row(lines: &[&str], range: &Range<usize>) -> usize {
    range
        .clone()
        .rev()
        .find(|&row| !lines[row].trim().is_empty())
        .map(|row| row + 1)
        .unwrap_or(range.start)
}

/// The edit block that creates the section on the first sync with events:
/// last child subsection of the planner section, preceded by a blank line
/// (spec §5.1 rule 2).
fn section_creation_edits(
    lines: &[&str],
    planner_range: &Range<usize>,
    planner_level: usize,
    section: &str,
    events: &[CalendarEvent],
) -> Vec<LineEdit> {
    let row = planner_range.end;
    let mut block: Vec<String> = Vec::new();
    if row > 0 && !lines[row - 1].trim().is_empty() {
        block.push(String::new());
    }
    block.push(format!("{} {}", "#".repeat(planner_level + 1), section));
    block.push(String::new());
    let mut sorted: Vec<&CalendarEvent> = events.iter().collect();
    sorted.sort_by(|a, b| event_sort_key(a).cmp(&event_sort_key(b)));
    block.extend(sorted.into_iter().map(render_synced_line));
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

/// A line in the Calendar section matching the synced-line grammar
/// (spec §5.2), parsed with byte spans so edits splice the original line
/// instead of re-rendering it — indentation, bullet style, checkbox, and any
/// user trailing text survive verbatim. Lines that don't parse are opaque and
/// never modified.
#[derive(Debug)]
struct SyncedLine {
    row: usize,
    line: String,
    id: String,
    /// Start minute parsed from the time token, for insert ordering.
    sort_start: Option<u32>,
    /// Byte span of the time token (no surrounding whitespace).
    token_span: Option<Range<usize>>,
    /// Byte span of the text between the time token (or checkbox) and the
    /// marker, trimmed.
    text_span: Range<usize>,
    /// Byte span of the whole `<!--gcal:…-->` marker.
    marker_span: Range<usize>,
    /// The marker's kind suffix as written, `None` when it has none. Kept raw
    /// so a suffix this build doesn't know is left alone rather than reset.
    marker_suffix: Option<String>,
}

impl SyncedLine {
    fn parse(row: usize, line: &str) -> Option<Self> {
        let trimmed = line.trim_end();
        let marker_start = trimmed.rfind(MARKER_PREFIX)?;
        let payload =
            trimmed[marker_start + MARKER_PREFIX.len()..].strip_suffix(MARKER_SUFFIX)?;
        let (id, marker_suffix) = parse_marker_payload(payload)?;
        let marker_span = marker_start..trimmed.len();

        let text_offset = checkbox_text_offset(line)?;
        let before_marker = &line[text_offset..marker_start];
        let (sort_start, token_len) = match leading_time_token(before_marker) {
            Some((start, len)) => (Some(start), len),
            None => (None, 0),
        };
        let token_span =
            (token_len > 0).then(|| text_offset..text_offset + token_len);
        let after_token = &line[text_offset + token_len..marker_start];
        let text_start = text_offset + token_len + (after_token.len() - after_token.trim_start().len());
        let text_end = text_start + after_token.trim().len();

        Some(Self {
            row,
            line: line.to_string(),
            id: id.to_string(),
            sort_start,
            token_span,
            text_span: text_start..text_end,
            marker_span,
            marker_suffix: marker_suffix.map(str::to_string),
        })
    }

    /// The marker to write for `kind`, or `None` when the line's marker
    /// already says that — including the case of a suffix this build doesn't
    /// recognize, which is left exactly as the note has it.
    fn marker_update_for(&self, kind: EventKind) -> Option<String> {
        let current = self.marker_suffix.as_deref();
        if current == kind.marker_suffix() {
            return None;
        }
        if current.is_some_and(|suffix| EventKind::from_marker_suffix(suffix).is_none()) {
            return None;
        }
        Some(render_marker(&self.id, kind))
    }

    fn text(&self) -> &str {
        &self.line[self.text_span.clone()]
    }

    fn token(&self) -> Option<&str> {
        self.token_span.clone().map(|span| &self.line[span])
    }

    /// `Some((inner, trailing))` when the text is our cancelled form
    /// (spec §8.3).
    fn cancelled_parts(&self) -> Option<(&str, &str)> {
        let text = self.text().strip_prefix("~~")?;
        let end = text.find(CANCELLED_SUFFIX)?;
        Some((&text[..end], &text[end + CANCELLED_SUFFIX.len()..]))
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled_parts().is_some()
    }

    /// The line with its text region replaced by the cancelled form. The
    /// whole text is struck through — with the event gone there is no way to
    /// split the title from user trailing text, and a struck-through line is
    /// still entirely visible.
    fn cancelled_form(&self) -> String {
        let mut text = String::new();
        let _ = write!(text, "~~{}{CANCELLED_SUFFIX}", self.text());
        self.with_text_region(self.token(), &text)
    }

    /// The line brought in sync with `event`, or `None` when the user renamed
    /// it and the line is theirs (spec §8.4). `Some(line)` equal to the
    /// current line means nothing to do.
    fn updated_for(&self, event: &CalendarEvent) -> Option<String> {
        let expected_token = event.time.map(|(start, end)| format_time_token(start, end));
        // A line written before this build knew about event types gets its
        // marker upgraded in place, without touching the user's text.
        let marker = self.marker_update_for(event.kind);
        if let Some((_, trailing)) = self.cancelled_parts() {
            // A re-created event with the same id un-marks the cancellation.
            let mut text = event.title.clone();
            text.push_str(trailing);
            return Some(self.rebuilt(
                expected_token.as_deref(),
                text.trim_end(),
                marker.as_deref(),
            ));
        }
        let trailing = title_trailing(self.text(), &event.title)?;
        if self.token() == expected_token.as_deref() {
            return Some(match marker.as_deref() {
                Some(marker) => self.rebuilt(self.token(), self.text(), Some(marker)),
                None => self.line.clone(),
            });
        }
        // Time moved: replace the time token only; checkbox, title, and
        // trailing text stay untouched (spec §8.2).
        let mut text = event.title.clone();
        text.push_str(trailing);
        Some(self.rebuilt(
            expected_token.as_deref(),
            text.trim_end(),
            marker.as_deref(),
        ))
    }

    /// Rebuilds the line with the region between the checkbox and the marker
    /// replaced by `token` + `text`; everything outside it (indent, bullet,
    /// checkbox, marker, spacing at the edges) is spliced through verbatim.
    fn with_text_region(&self, token: Option<&str>, text: &str) -> String {
        self.rebuilt(token, text, None)
    }

    /// `with_text_region` plus an optional replacement marker; `None` keeps
    /// the line's own marker byte for byte.
    fn rebuilt(&self, token: Option<&str>, text: &str, marker: Option<&str>) -> String {
        let content_start = self
            .token_span
            .as_ref()
            .map(|span| span.start)
            .unwrap_or(self.text_span.start);
        let mut content = String::new();
        if let Some(token) = token {
            content.push_str(token);
            if !text.is_empty() {
                content.push(' ');
            }
        }
        content.push_str(text);
        format!(
            "{}{}{}{}{}",
            &self.line[..content_start],
            content,
            &self.line[self.text_span.end..self.marker_span.start],
            marker.unwrap_or(&self.line[self.marker_span.clone()]),
            &self.line[self.marker_span.end..]
        )
    }
}

/// `(done, byte offset of the task text)` for a checkbox line. Liberal like
/// the Day Planner's parser (any bullet, any indent), so a synced line the
/// user re-indented under another task is still recognized instead of being
/// re-inserted as a duplicate.
fn checkbox_text_offset(line: &str) -> Option<usize> {
    let after_indent = line.trim_start();
    let after_bullet = after_indent.strip_prefix(['-', '*', '+'])?;
    let after_space = after_bullet.trim_start();
    if after_space.len() == after_bullet.len() {
        return None;
    }
    let mut chars = after_space.strip_prefix('[')?.chars();
    match chars.next()? {
        ' ' | 'x' | 'X' => {}
        _ => return None,
    }
    let after_checkbox = chars.as_str().strip_prefix(']')?;
    let text = after_checkbox.trim_start();
    if text.len() == after_checkbox.len() && !text.is_empty() {
        return None;
    }
    Some(line.len() - text.len())
}

/// The byte length of a leading time token (range or start-only, any of the
/// V4 separators) and its start minute. The token must be followed by
/// whitespace or nothing, like the planner's own parser.
fn leading_time_token(text: &str) -> Option<(u32, usize)> {
    let (start, after_start) = day_plan::parse_time_prefix(text)?;
    if let Some((_, after_end)) = day_plan::parse_range_end(after_start)
        && (after_end.is_empty() || after_end.starts_with(char::is_whitespace))
    {
        return Some((start, text.len() - after_end.len()));
    }
    if after_start.is_empty() || after_start.starts_with(char::is_whitespace) {
        return Some((start, text.len() - after_start.len()));
    }
    None
}

/// When `text` is `event_title` plus optional whitespace-separated trailing
/// content, returns that trailing content (`""` for an exact match). `None`
/// means the user rewrote the title.
fn title_trailing<'a>(text: &'a str, event_title: &str) -> Option<&'a str> {
    if event_title.is_empty() {
        return Some(text);
    }
    let rest = text.strip_prefix(event_title)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CalendarConfig {
        CalendarConfig::with_planner_heading("Day planner")
    }

    fn event(id: &str, title: &str, start: u32, end: u32) -> CalendarEvent {
        CalendarEvent {
            id: id.to_string(),
            title: title.to_string(),
            time: Some((start, end)),
            kind: EventKind::Default,
        }
    }

    fn focus_event(id: &str, title: &str, start: u32, end: u32) -> CalendarEvent {
        CalendarEvent {
            kind: EventKind::FocusTime,
            ..event(id, title, start, end)
        }
    }

    fn all_day(id: &str, title: &str) -> CalendarEvent {
        CalendarEvent {
            id: id.to_string(),
            title: title.to_string(),
            time: None,
            kind: EventKind::Default,
        }
    }

    fn run(note: &str, events: &[CalendarEvent]) -> (String, Vec<Divergence>) {
        match reconcile(note, events, &config()) {
            Reconciled::NoPlannerSection => panic!("unexpected NoPlannerSection"),
            Reconciled::Edits { edits, diverged } => (apply_line_edits(note, &edits), diverged),
        }
    }

    #[test]
    fn config_defaults_and_clamping() {
        let parsed = parse_calendar_config("", "Day planner").unwrap();
        assert_eq!(parsed, config());

        let parsed = parse_calendar_config(
            "schema = 1\naccount = \"diego@example.com\"\ncalendars = [\"primary\"]\n\
             section = \"Meetings\"\npoll_seconds = 5\n\n[filters]\nall_day = true\n\
             \n[google]\nclient_id = \"me\"\n\nfuture_key = 1\n",
            "Plan",
        )
        .unwrap();
        assert_eq!(parsed.account.as_deref(), Some("diego@example.com"));
        assert_eq!(parsed.calendars, vec!["primary"]);
        assert_eq!(parsed.section, "Meetings");
        assert_eq!(parsed.poll_interval, Duration::from_secs(60));
        assert!(parsed.filters.all_day);
        assert!(parsed.filters.accepted_only);
        assert_eq!(parsed.google.client_id.as_deref(), Some("me"));
        assert_eq!(parsed.planner_heading, "Plan");

        assert_eq!(
            parse_calendar_config("poll_seconds = 999999", "x")
                .unwrap()
                .poll_interval,
            Duration::from_secs(3600)
        );
        assert!(parse_calendar_config("not [valid", "x").is_err());
    }

    #[test]
    fn marker_id_is_short_stable_hex() {
        let id = event_marker_id("primary", "abc123");
        assert_eq!(id.len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id, event_marker_id("primary", "abc123"));
        assert_ne!(id, event_marker_id("other", "abc123"));
        assert_ne!(id, event_marker_id("primary", "abc124"));
    }

    #[test]
    fn focus_time_lines_carry_their_kind_in_the_marker() {
        let note = "# Day planner\n\n## Calendar\n\n";
        let (applied, _) = run(note, &[focus_event("aaaaaaaaaaaa", "Focus time", 540, 1020)]);
        assert!(
            applied.contains("- [ ] 09:00 - 17:00 Focus time <!--gcal:aaaaaaaaaaaa:focus-->"),
            "{applied}"
        );
        assert_eq!(
            line_event_kind("- [ ] 09:00 - 17:00 Focus time <!--gcal:aaaaaaaaaaaa:focus-->"),
            EventKind::FocusTime
        );
        assert_eq!(
            line_event_kind("- [ ] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->"),
            EventKind::Default
        );
        assert_eq!(line_event_kind("- [ ] A task of my own"), EventKind::Default);
    }

    #[test]
    fn an_existing_line_gets_its_kind_added_without_touching_the_text() {
        // Written by a build that didn't know about event types, and renamed
        // by the user since — the marker still upgrades, the rename survives.
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [x] 09:00 - 17:00 Focus time (deep work) <!--gcal:aaaaaaaaaaaa-->\n";
        let (applied, diverged) = run(note, &[focus_event("aaaaaaaaaaaa", "Focus time", 540, 1020)]);
        assert_eq!(
            applied,
            "# Day planner\n\n## Calendar\n\n\
             - [x] 09:00 - 17:00 Focus time (deep work) <!--gcal:aaaaaaaaaaaa:focus-->\n"
        );
        assert!(diverged.is_empty());
    }

    #[test]
    fn an_unknown_marker_suffix_identifies_its_line_and_is_left_alone() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [ ] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa:brunch-->\n";
        let (applied, _) = run(note, &[event("aaaaaaaaaaaa", "Standup", 600, 630)]);
        assert_eq!(applied, note, "no duplicate insert, no marker rewrite");
    }

    #[test]
    fn a_malformed_marker_is_not_a_synced_line() {
        for payload in ["", "zz:focus", "aaaaaaaaaaaa:", "aaaaaaaaaaaa:Focus"] {
            assert_eq!(parse_marker_payload(payload), None, "{payload:?}");
        }
        assert_eq!(
            parse_marker_payload("aaaaaaaaaaaa"),
            Some(("aaaaaaaaaaaa", None))
        );
        assert_eq!(
            parse_marker_payload("aaaaaaaaaaaa:focus"),
            Some(("aaaaaaaaaaaa", Some("focus")))
        );
    }

    #[test]
    fn missing_planner_heading_holds() {
        let result = reconcile("# Journal\n", &[event("aaaaaaaaaaaa", "T", 600, 630)], &config());
        assert_eq!(result, Reconciled::NoPlannerSection);
    }

    #[test]
    fn no_events_never_creates_the_section() {
        let note = "# Day planner\n\n- [ ] Workout\n";
        let (applied, _) = run(note, &[]);
        assert_eq!(applied, note);
    }

    #[test]
    fn first_sync_creates_the_section_last_with_blank_lines() {
        let note = "# Monday\n\n# Day planner\n\n- [ ] Workout\n\n# Personal\n- [ ] Call home\n";
        let (applied, _) = run(
            note,
            &[
                event("bbbbbbbbbbbb", "1:1 Ramon", 870, 930),
                event("aaaaaaaaaaaa", "API Leads meeting", 600, 630),
            ],
        );
        assert_eq!(
            applied,
            "# Monday\n\n# Day planner\n\n- [ ] Workout\n\n## Calendar\n\n\
             - [ ] 10:00 - 10:30 API Leads meeting <!--gcal:aaaaaaaaaaaa-->\n\
             - [ ] 14:30 - 15:30 1:1 Ramon <!--gcal:bbbbbbbbbbbb-->\n\n\
             # Personal\n- [ ] Call home\n"
        );
    }

    #[test]
    fn section_creation_at_end_of_file() {
        let note = "# Day planner\n- [ ] Workout";
        let (applied, _) = run(note, &[event("aaaaaaaaaaaa", "Standup", 570, 600)]);
        assert_eq!(
            applied,
            "# Day planner\n- [ ] Workout\n\n## Calendar\n\n\
             - [ ] 09:30 - 10:00 Standup <!--gcal:aaaaaaaaaaaa-->"
        );
    }

    #[test]
    fn inserts_sort_among_synced_lines_without_reordering_them() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n\
                    some free text the user wrote\n\
                    - [ ] 15:00 - 16:00 Review <!--gcal:cccccccccccc-->\n";
        let (applied, _) = run(
            note,
            &[
                event("aaaaaaaaaaaa", "Standup", 600, 630),
                event("cccccccccccc", "Review", 900, 960),
                event("bbbbbbbbbbbb", "Lunch", 720, 780),
                event("dddddddddddd", "Late sync", 1020, 1050),
                event("000000000000", "Early run", 420, 450),
            ],
        );
        assert_eq!(
            applied,
            "# Day planner\n\n## Calendar\n\n\
             - [ ] 07:00 - 07:30 Early run <!--gcal:000000000000-->\n\
             - [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n\
             some free text the user wrote\n\
             - [ ] 12:00 - 13:00 Lunch <!--gcal:bbbbbbbbbbbb-->\n\
             - [ ] 15:00 - 16:00 Review <!--gcal:cccccccccccc-->\n\
             - [ ] 17:00 - 17:30 Late sync <!--gcal:dddddddddddd-->\n"
        );
    }

    #[test]
    fn insert_after_the_last_synced_line_lands_below_its_sub_bullets() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n\
                    \t- prep the demo first\n";
        let (applied, _) = run(
            note,
            &[
                event("aaaaaaaaaaaa", "Standup", 600, 630),
                event("bbbbbbbbbbbb", "Review", 900, 960),
            ],
        );
        assert_eq!(
            applied,
            "# Day planner\n\n## Calendar\n\n\
             - [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n\
             \t- prep the demo first\n\
             - [ ] 15:00 - 16:00 Review <!--gcal:bbbbbbbbbbbb-->\n"
        );
    }

    #[test]
    fn level_six_planner_heading_never_creates_the_section() {
        let note = "###### Day planner\n\n- [ ] Workout\n";
        let events = [event("aaaaaaaaaaaa", "Standup", 600, 630)];
        let (applied, _) = run(note, &events);
        assert_eq!(applied, note);
        let (again, _) = run(&applied, &events);
        assert_eq!(again, applied);
    }

    #[test]
    fn moved_event_replaces_the_time_token_only() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    \t* [x] 10:00 - 10:30 Standup with notes <!--gcal:aaaaaaaaaaaa-->\n";
        let (applied, diverged) = run(note, &[event("aaaaaaaaaaaa", "Standup", 660, 690)]);
        assert!(diverged.is_empty());
        // Indent, bullet style, checkbox, and the user's trailing text all
        // survive; only the token changed.
        assert!(applied.contains(
            "\t* [x] 11:00 - 11:30 Standup with notes <!--gcal:aaaaaaaaaaaa-->"
        ));
    }

    #[test]
    fn renamed_line_is_frozen_and_reported() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [ ] 10:00 - 10:30 Coffee with the API folks <!--gcal:aaaaaaaaaaaa-->\n";
        // Even the time change is not applied once the title diverged.
        let (applied, diverged) = run(note, &[event("aaaaaaaaaaaa", "API Leads meeting", 660, 690)]);
        assert_eq!(applied, note);
        assert_eq!(diverged.len(), 1);
        assert_eq!(diverged[0].id, "aaaaaaaaaaaa");
        assert_eq!(diverged[0].event_title, "API Leads meeting");
    }

    #[test]
    fn vanished_event_is_marked_cancelled_not_deleted() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n";
        let (applied, _) = run(note, &[]);
        assert_eq!(
            applied,
            "# Day planner\n\n## Calendar\n\n\
             - [x] 10:00 - 10:30 ~~Standup~~ (cancelled) <!--gcal:aaaaaaaaaaaa-->\n"
        );
        // Marking again is a no-op.
        let (again, _) = run(&applied, &[]);
        assert_eq!(again, applied);
    }

    #[test]
    fn recreated_event_unmarks_the_cancellation() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [x] 10:00 - 10:30 ~~Standup~~ (cancelled) <!--gcal:aaaaaaaaaaaa-->\n";
        let (applied, _) = run(note, &[event("aaaaaaaaaaaa", "Standup", 660, 690)]);
        assert_eq!(
            applied,
            "# Day planner\n\n## Calendar\n\n\
             - [x] 11:00 - 11:30 Standup <!--gcal:aaaaaaaaaaaa-->\n"
        );
    }

    #[test]
    fn all_day_events_have_no_time_token() {
        let note = "# Day planner\n\n## Calendar\n\n\
                    - [ ] 09:00 - 10:00 Planning <!--gcal:bbbbbbbbbbbb-->\n";
        let (applied, _) = run(
            note,
            &[
                all_day("aaaaaaaaaaaa", "Diego's birthday"),
                event("bbbbbbbbbbbb", "Planning", 540, 600),
            ],
        );
        assert_eq!(
            applied,
            "# Day planner\n\n## Calendar\n\n\
             - [ ] Diego's birthday <!--gcal:aaaaaaaaaaaa-->\n\
             - [ ] 09:00 - 10:00 Planning <!--gcal:bbbbbbbbbbbb-->\n"
        );
    }

    #[test]
    fn hostile_titles_cannot_forge_markers() {
        let note = "# Day planner\n\n## Calendar\n";
        let (applied, _) = run(
            note,
            &[event("aaaaaaaaaaaa", "sneaky <!--gcal:ffffffffffff--> \n newline", 600, 630)],
        );
        assert!(applied.contains(
            "- [ ] 10:00 - 10:30 sneaky gcal:ffffffffffff--> newline <!--gcal:aaaaaaaaaaaa-->"
        ));
        // The line still round-trips to the real id.
        let reparsed = SyncedLine::parse(0, applied.lines().last().unwrap()).unwrap();
        assert_eq!(reparsed.id, "aaaaaaaaaaaa");
    }

    #[test]
    fn section_heading_is_found_case_insensitively_and_by_config_name() {
        let mut config = config();
        config.section = "Meetings".to_string();
        let note = "# Day planner\n\n## MEETINGS\n\n- [ ] 10:00 - 10:30 Old <!--gcal:aaaaaaaaaaaa-->\n";
        let Reconciled::Edits { edits, .. } =
            reconcile(note, &[event("aaaaaaaaaaaa", "Old", 600, 630)], &config)
        else {
            panic!("expected edits");
        };
        assert!(edits.is_empty(), "{edits:?}");
    }

    #[test]
    fn non_synced_content_in_the_section_is_opaque() {
        let note = "# Day planner\n\n## Calendar\n\nA note to self\n\
                    - [ ] my own task\n- [ ] 08:00 not synced either\n";
        let (applied, _) = run(note, &[event("aaaaaaaaaaaa", "Standup", 570, 600)]);
        assert_eq!(
            applied,
            "# Day planner\n\n## Calendar\n\nA note to self\n\
             - [ ] my own task\n- [ ] 08:00 not synced either\n\
             - [ ] 09:30 - 10:00 Standup <!--gcal:aaaaaaaaaaaa-->\n"
        );
    }

    #[test]
    fn duplicate_event_ids_are_deduped() {
        let note = "# Day planner\n\n## Calendar\n";
        let (applied, _) = run(
            note,
            &[
                event("aaaaaaaaaaaa", "Standup", 570, 600),
                event("aaaaaaaaaaaa", "Standup again", 600, 630),
            ],
        );
        assert_eq!(applied.matches("gcal:aaaaaaaaaaaa").count(), 1);
    }

    #[test]
    fn apply_line_edits_orders_inserts_and_replaces() {
        let note = "a\nb\nc\n";
        let edits = vec![
            LineEdit {
                row: 1,
                kind: LineEditKind::Insert("x".to_string()),
            },
            LineEdit {
                row: 1,
                kind: LineEditKind::Insert("y".to_string()),
            },
            LineEdit {
                row: 1,
                kind: LineEditKind::Replace("B".to_string()),
            },
            LineEdit {
                row: 3,
                kind: LineEditKind::Insert("z".to_string()),
            },
        ];
        assert_eq!(apply_line_edits(note, &edits), "a\nx\ny\nB\nc\nz\n");
        assert_eq!(apply_line_edits("a\nb\nc", &edits), "a\nx\ny\nB\nc\nz");
    }

    #[test]
    fn reconcile_is_idempotent() {
        let scenarios: Vec<(&str, Vec<CalendarEvent>)> = vec![
            ("# Day planner\n", vec![event("aaaaaaaaaaaa", "Standup", 570, 600)]),
            ("# Day planner\n- [ ] Workout", vec![]),
            (
                "# Day planner\n\n## Calendar\n\n- [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n",
                vec![
                    event("aaaaaaaaaaaa", "Standup", 660, 690),
                    event("bbbbbbbbbbbb", "Lunch", 720, 780),
                    all_day("cccccccccccc", "Holiday"),
                ],
            ),
            (
                "# Day planner\n\n## Calendar\n\n- [ ] 10:00 - 10:30 Renamed by me <!--gcal:aaaaaaaaaaaa-->\n",
                vec![event("aaaaaaaaaaaa", "Standup", 660, 690)],
            ),
            (
                "# Day planner\n\n## Calendar\n\n- [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n\n## Misc\n- [ ] Errand\n",
                vec![],
            ),
            (
                "# Day planner\n\n## Calendar\n\n- [ ] 10:00 - 10:30 ~~Standup~~ (cancelled) <!--gcal:aaaaaaaaaaaa-->\n",
                vec![event("aaaaaaaaaaaa", "Standup", 600, 630)],
            ),
            (
                "# Monday\n# Day planner\n- [ ] Workout\n# Personal\n",
                vec![event("aaaaaaaaaaaa", "A", 600, 630), all_day("bbbbbbbbbbbb", "B")],
            ),
        ];
        for (note, events) in scenarios {
            let (applied, _) = run(note, &events);
            let Reconciled::Edits { edits, .. } = reconcile(&applied, &events, &config()) else {
                panic!("planner section vanished for {note:?}");
            };
            assert!(
                edits.is_empty(),
                "not idempotent for {note:?}:\napplied: {applied:?}\nedits: {edits:?}"
            );
        }
    }
}
