//! Readwise sync (spec `v31-readwise-sync.md`): the `.thock/readwise.toml`
//! category → folder map, the export API's shapes, note rendering, the vault
//! scan that rebuilds state, and the pure planner. Everything here is
//! string-in/string-out — no network, no I/O. The GPUI service, transport,
//! and token prompt live in `readwise_service.rs`.

use anyhow::{Result, bail};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Duration;

use crate::backlog::Edit;
use crate::gmail::{break_wikilinks, collapse_whitespace};
use crate::inbox::sanitize_title;

/// Lives next to `config.toml` in `.thock/`; its existence turns the feature
/// on (spec §7), the same rule as `gmail.toml`.
pub const READWISE_CONFIG_FILE: &str = "readwise.toml";

pub(crate) const MARKER_PREFIX: &str = "<!--rw:";
const MARKER_SUFFIX: &str = "-->";

/// The section new highlights are appended to (spec §4.2).
pub const HIGHLIGHTS_HEADING: &str = "Highlights";

/// Readwise's export categories (spec §4.1).
pub const KNOWN_CATEGORIES: [&str; 5] = ["books", "articles", "tweets", "podcasts", "supplementals"];

/// What the connect action writes when no config exists yet (spec §7).
pub const DEFAULT_CONFIG_TOML: &str = "schema = 1

# Poll cadence. Clamped to [15, 1440] minutes. Highlights trickle in, so hourly is plenty.
# poll_minutes = 60

# One entry per category to sync. Omitting every [[sync]] entry ships the default below.
[[sync]]
category = \"books\"
path     = \"reference/readwise/books\"
";

/// One `[[sync]]` entry: sources in `category` land as notes in the
/// vault-relative folder `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadwiseMapping {
    pub category: String,
    pub path: String,
}

/// Resolved `.thock/readwise.toml` (spec §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadwiseConfig {
    pub mappings: Vec<ReadwiseMapping>,
    pub poll_interval: Duration,
}

impl ReadwiseConfig {
    /// The landing folder for a category, `None` when it isn't mapped.
    pub fn folder_for(&self, category: &str) -> Option<&str> {
        self.mappings
            .iter()
            .find(|mapping| mapping.category == category)
            .map(|mapping| mapping.path.as_str())
    }
}

/// The shipped map: books only, the current need (spec §4.1).
pub fn default_mappings() -> Vec<ReadwiseMapping> {
    vec![ReadwiseMapping {
        category: "books".to_string(),
        path: "reference/readwise/books".to_string(),
    }]
}

impl Default for ReadwiseConfig {
    fn default() -> Self {
        Self {
            mappings: default_mappings(),
            poll_interval: Duration::from_secs(60 * 60),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReadwiseConfigContent {
    schema: Option<u32>,
    poll_minutes: Option<u64>,
    sync: Vec<SyncContent>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SyncContent {
    category: Option<String>,
    path: Option<String>,
}

/// Parses `.thock/readwise.toml` (spec §7). Unlike the Gmail map, a bad
/// `[[sync]]` entry is an error rather than a skipped line: a duplicate or
/// unknown category means the user is trying to route something and it
/// isn't going where they think, so the status row says so (spec §4.1).
pub fn parse_readwise_config(text: &str) -> Result<ReadwiseConfig> {
    let content: ReadwiseConfigContent = toml::from_str(text)?;
    let mut mappings: Vec<ReadwiseMapping> = Vec::new();
    for entry in content.sync {
        let category = entry
            .category
            .map(|category| category.trim().to_ascii_lowercase())
            .filter(|category| !category.is_empty());
        let path = entry
            .path
            .map(|path| path.trim().trim_matches('/').to_string())
            .filter(|path| !path.is_empty());
        let (Some(category), Some(path)) = (category, path) else {
            bail!("a [[sync]] entry needs both a category and a path");
        };
        if !KNOWN_CATEGORIES.contains(&category.as_str()) {
            bail!("unknown category \"{category}\"");
        }
        if mappings.iter().any(|mapping| mapping.category == category) {
            bail!("two entries for \"{category}\"");
        }
        mappings.push(ReadwiseMapping { category, path });
    }
    if mappings.is_empty() {
        mappings = default_mappings();
    }
    let poll_minutes = content.poll_minutes.unwrap_or(60).clamp(15, 1440);
    Ok(ReadwiseConfig {
        mappings,
        poll_interval: Duration::from_secs(poll_minutes * 60),
    })
}

/// One page of `GET /api/v2/export/` (spec §8.1).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExportPage {
    #[serde(default)]
    pub results: Vec<ReadwiseSource>,
    #[serde(rename = "nextPageCursor", default)]
    pub next_page_cursor: Option<String>,
}

/// A source (book, article, podcast…) with its highlights nested inside.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ReadwiseSource {
    pub user_book_id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub cover_image_url: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub readwise_url: Option<String>,
    #[serde(default)]
    pub asin: Option<String>,
    #[serde(default)]
    pub book_tags: Vec<ReadwiseTag>,
    #[serde(default)]
    pub is_deleted: bool,
    #[serde(default)]
    pub highlights: Vec<ReadwiseHighlight>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ReadwiseTag {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ReadwiseHighlight {
    pub id: u64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub location: Option<i64>,
    #[serde(default)]
    pub location_type: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub highlighted_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub readwise_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<ReadwiseTag>,
    #[serde(default)]
    pub is_discard: bool,
    #[serde(default)]
    pub is_deleted: bool,
}

impl ReadwiseHighlight {
    /// `highlighted_at`, falling back to `created_at` (spec §4.3).
    fn moment(&self) -> Option<DateTime<Utc>> {
        [&self.highlighted_at, &self.created_at]
            .into_iter()
            .flatten()
            .find_map(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
            .map(|moment| moment.with_timezone(&Utc))
    }

    /// The highlight's day in `tz`, the date its marker carries.
    pub fn day<Tz: TimeZone>(&self, tz: &Tz) -> Option<NaiveDate> {
        self.moment()
            .map(|moment| moment.with_timezone(tz).date_naive())
    }
}

/// `<!--rw:<id>@<YYYY-MM-DD>-->`, or without the date when the highlight
/// carries no timestamp at all.
pub fn highlight_marker(id: u64, day: Option<NaiveDate>) -> String {
    match day {
        Some(day) => format!("{MARKER_PREFIX}{id}@{}{MARKER_SUFFIX}", day.format("%Y-%m-%d")),
        None => format!("{MARKER_PREFIX}{id}{MARKER_SUFFIX}"),
    }
}

/// Every `rw:` marker id in `text`, all sections included — the note's half
/// of the dedup record (spec §4.3).
pub fn scan_markers(text: &str) -> HashSet<u64> {
    let mut markers = HashSet::new();
    for chunk in text.split(MARKER_PREFIX).skip(1) {
        let Some(inner) = chunk.split(MARKER_SUFFIX).next() else {
            continue;
        };
        let id = inner.split('@').next().unwrap_or_default().trim();
        if let Ok(id) = id.parse::<u64>() {
            markers.insert(id);
        }
    }
    markers
}

/// The `readwise_id:` from a note's frontmatter, for the rebuild scan.
pub fn note_readwise_id(content: &str) -> Option<u64> {
    let rest = content.strip_prefix("---\n")?;
    let (frontmatter, _) = rest.split_once("\n---")?;
    frontmatter.lines().find_map(|line| {
        line.strip_prefix("readwise_id:")?
            .trim()
            .trim_matches('"')
            .parse()
            .ok()
    })
}

/// Title → file stem: V13's title sanitizing plus the characters no file
/// system accepts, capped so a pasted-paragraph title stays a sane name.
pub fn note_stem(title: &str) -> String {
    let title = sanitize_title(title);
    let replaced: String = title
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            other => other,
        })
        .collect();
    let mut stem = collapse_whitespace(&replaced);
    if stem.chars().count() > 120 {
        stem = stem.chars().take(120).collect::<String>().trim_end().to_string();
    }
    let stem = stem.trim_matches(|character: char| character == '.' || character.is_whitespace());
    if stem.is_empty() {
        "(untitled)".to_string()
    } else {
        stem.to_string()
    }
}

/// `Title`, then `Title (2)`, `Title (3)`… — the first stem not in `taken`
/// (spec §6).
pub fn unique_stem(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    (2..)
        .map(|counter| format!("{base} ({counter})"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or_else(|| base.to_string())
}

/// Highlight or note text as it goes into a line: marker forgery and
/// wikilink forgery removed, newlines collapsed so one highlight is one line
/// and its marker stays trailing.
fn clean_text(text: &str) -> String {
    collapse_whitespace(&break_wikilinks(&text.replace("<!--", "")))
}

/// `#a #b` from a tag list; `None` when empty. Spaces inside a tag become
/// dashes so the tag stays one token.
fn tag_list(tags: &[ReadwiseTag]) -> Option<String> {
    let tags: Vec<String> = tags
        .iter()
        .map(|tag| clean_text(&tag.name).replace(' ', "-"))
        .filter(|tag| !tag.is_empty())
        .map(|tag| format!("#{tag}"))
        .collect();
    (!tags.is_empty()).then(|| tags.join(" "))
}

fn time_offset_label(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (hours, minutes, rest) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{rest:02}")
    } else {
        format!("{minutes}:{rest:02}")
    }
}

/// The parenthesized location per `location_type` (spec §6), `None` when the
/// highlight has nothing to point at.
fn location_label(highlight: &ReadwiseHighlight, source: &ReadwiseSource) -> Option<String> {
    let view_highlight = || {
        highlight
            .readwise_url
            .as_deref()
            .map(|url| format!("[View Highlight]({url})"))
    };
    match (highlight.location_type.as_deref(), highlight.location) {
        (Some("location"), Some(location)) => Some(match source.asin.as_deref() {
            Some(asin) if !asin.is_empty() => format!(
                "[Location {location}](https://readwise.io/to_kindle?action=open&asin={asin}&location={location})"
            ),
            _ => format!("Location {location}"),
        }),
        (Some("page"), Some(page)) => Some(format!("Page {page}")),
        (Some("time_offset"), Some(seconds)) => {
            let label = time_offset_label(seconds);
            let url = highlight
                .url
                .as_deref()
                .or(source.source_url.as_deref())
                .filter(|url| !url.is_empty());
            Some(match url {
                Some(url) => format!("[{label}]({url})"),
                None => view_highlight().unwrap_or(label),
            })
        }
        _ => view_highlight(),
    }
}

/// One highlight as it lands in the note: the bullet with its location and
/// marker, then `- Note:` and `- Tags:` sub-bullets when present.
pub fn render_highlight<Tz: TimeZone>(
    highlight: &ReadwiseHighlight,
    source: &ReadwiseSource,
    tz: &Tz,
) -> String {
    let mut line = format!("- {}", clean_text(&highlight.text));
    if let Some(location) = location_label(highlight, source) {
        let _ = write!(line, " ({location})");
    }
    let _ = writeln!(
        line,
        " {}",
        highlight_marker(highlight.id, highlight.day(tz))
    );
    if let Some(note) = highlight
        .note
        .as_deref()
        .map(clean_text)
        .filter(|note| !note.is_empty())
    {
        let _ = writeln!(line, "    - Note: {note}");
    }
    if let Some(tags) = tag_list(&highlight.tags) {
        let _ = writeln!(line, "    - Tags: {tags}");
    }
    line
}

/// A whole new note (spec §6): the plugin's template in the body, identity in
/// the frontmatter and the trailing markers.
pub fn render_note<Tz: TimeZone>(
    source: &ReadwiseSource,
    highlights: &[&ReadwiseHighlight],
    tz: &Tz,
) -> String {
    let title = sanitize_title(&source.title);
    let mut note = String::new();
    let _ = writeln!(note, "---");
    let _ = writeln!(note, "source: readwise");
    let _ = writeln!(note, "readwise_id: {}", source.user_book_id);
    let _ = writeln!(note, "category: {}", clean_text(&source.category));
    let _ = writeln!(note, "---");
    let _ = writeln!(note, "# {title}");
    if let Some(cover) = source
        .cover_image_url
        .as_deref()
        .map(collapse_whitespace)
        .filter(|cover| !cover.is_empty())
    {
        let _ = writeln!(note, "\n![rw-book-cover]({cover})");
    }
    let _ = writeln!(note, "\n## Metadata");
    if let Some(author) = source
        .author
        .as_deref()
        .map(clean_text)
        .filter(|author| !author.is_empty())
    {
        let _ = writeln!(note, "- Author: [[{author}]]");
    }
    let _ = writeln!(note, "- Full Title: {title}");
    let _ = writeln!(note, "- Category: #{}", clean_text(&source.category));
    if let Some(tags) = tag_list(&source.book_tags) {
        let _ = writeln!(note, "- Tags: {tags}");
    }
    if let Some(url) = source
        .source_url
        .as_deref()
        .map(collapse_whitespace)
        .filter(|url| !url.is_empty())
    {
        let _ = writeln!(note, "- URL: {url}");
    }
    let _ = writeln!(note, "\n## {HIGHLIGHTS_HEADING}");
    for highlight in highlights {
        note.push_str(&render_highlight(highlight, source, tz));
    }
    note
}

/// Orders by `location`, then `highlighted_at`, then id (spec §6). Unlocated
/// highlights sort last.
fn sort_highlights(highlights: &mut [&ReadwiseHighlight]) {
    highlights.sort_by_key(|highlight| {
        (
            highlight.location.unwrap_or(i64::MAX),
            highlight.moment(),
            highlight.id,
        )
    });
}

/// A synced note found by the vault scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScannedNote {
    pub rel_path: String,
    pub markers: HashSet<u64>,
}

/// What one poll learned from the mapped folders before fetching (spec §4.3).
#[derive(Debug, Clone, Default)]
pub struct ReadwiseVaultScan {
    /// `readwise_id` → its note, across every mapped folder.
    pub notes: HashMap<u64, ScannedNote>,
    /// Folder → the `.md` stems already in it.
    pub stems: HashMap<String, HashSet<String>>,
}

impl ReadwiseVaultScan {
    /// Records one `.md` file of a mapped folder.
    pub fn record(&mut self, folder: &str, stem: &str, rel_path: &str, content: &str) {
        self.stems
            .entry(folder.to_string())
            .or_default()
            .insert(stem.to_string());
        if let Some(readwise_id) = note_readwise_id(content) {
            self.notes.insert(
                readwise_id,
                ScannedNote {
                    rel_path: rel_path.to_string(),
                    markers: scan_markers(content),
                },
            );
        }
    }
}

/// The `landed.jsonl` cache: every highlight that ever landed and every
/// source that ever had a note (spec §4.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandedState {
    pub highlights: HashSet<u64>,
    pub books: HashSet<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteWrite {
    Create { contents: String },
    Append { lines: Vec<String> },
}

/// One note's share of a sync (spec §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteChange {
    pub rel_path: String,
    pub book_id: u64,
    pub highlight_ids: Vec<u64>,
    pub write: NoteWrite,
}

/// The planner (spec §8.2) — pure, no I/O. A source's note is found by
/// `readwise_id`; what is already landed (state) or marked (note) is skipped;
/// a note the user deleted comes back only with highlights that never
/// landed; discarded and deleted highlights, and deleted sources, never land.
pub fn plan_readwise_sync<Tz: TimeZone>(
    sources: &[ReadwiseSource],
    config: &ReadwiseConfig,
    scan: &ReadwiseVaultScan,
    landed: &LandedState,
    tz: &Tz,
) -> Vec<NoteChange> {
    let mut changes = Vec::new();
    let mut claimed: HashMap<String, HashSet<String>> = scan.stems.clone();
    let mut planned_books: HashSet<u64> = HashSet::new();
    for source in sources {
        let Some(folder) = config.folder_for(&source.category) else {
            continue;
        };
        if source.is_deleted || !planned_books.insert(source.user_book_id) {
            continue;
        }
        let existing = scan.notes.get(&source.user_book_id);
        let mut fresh: Vec<&ReadwiseHighlight> = source
            .highlights
            .iter()
            .filter(|highlight| !highlight.is_discard && !highlight.is_deleted)
            .filter(|highlight| !landed.highlights.contains(&highlight.id))
            .filter(|highlight| {
                existing.is_none_or(|note| !note.markers.contains(&highlight.id))
            })
            .collect();
        let mut seen = HashSet::new();
        fresh.retain(|highlight| seen.insert(highlight.id));
        if fresh.is_empty() {
            continue;
        }
        sort_highlights(&mut fresh);
        let highlight_ids = fresh.iter().map(|highlight| highlight.id).collect();
        match existing {
            Some(note) => changes.push(NoteChange {
                rel_path: note.rel_path.clone(),
                book_id: source.user_book_id,
                highlight_ids,
                write: NoteWrite::Append {
                    lines: fresh
                        .iter()
                        .map(|highlight| render_highlight(highlight, source, tz))
                        .collect(),
                },
            }),
            None => {
                let taken = claimed.entry(folder.to_string()).or_default();
                let stem = unique_stem(&note_stem(&source.title), taken);
                taken.insert(stem.clone());
                changes.push(NoteChange {
                    rel_path: format!("{folder}/{stem}.md"),
                    book_id: source.user_book_id,
                    highlight_ids,
                    write: NoteWrite::Create {
                        contents: render_note(source, &fresh, tz),
                    },
                });
            }
        }
    }
    changes
}

/// Whether `line` is the `## Highlights` heading, tolerating decoration
/// (`## Highlights:`, `## 📌 Highlights`).
fn is_highlights_heading(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("## ") else {
        return false;
    };
    rest.trim()
        .trim_end_matches(':')
        .trim_end_matches('#')
        .trim()
        .split_whitespace()
        .last()
        .is_some_and(|word| word.eq_ignore_ascii_case(HIGHLIGHTS_HEADING))
}

fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|character| *character == '#').count();
    (hashes > 0 && line[hashes..].starts_with(' ')).then_some(hashes)
}

/// The edit landing `lines` at the end of the `## Highlights` section —
/// after its last non-blank line, so a highlight's sub-bullets stay with it
/// — or under a recreated heading at the end of the file when the section
/// is gone (spec §8.2). Never touches an existing byte.
pub fn append_highlights_edit(text: &str, lines: &[String]) -> Edit {
    let block: String = lines.concat();
    let mut offset = 0;
    let mut heading_end: Option<usize> = None;
    let mut last_content_end: Option<usize> = None;
    for segment in text.split_inclusive('\n') {
        let content = segment.strip_suffix('\n').unwrap_or(segment);
        let end = offset + segment.len();
        match heading_end {
            None if is_highlights_heading(content) => heading_end = Some(end),
            None => {}
            Some(_) => {
                if heading_level(content).is_some_and(|level| level <= 2) {
                    break;
                }
                if !content.trim().is_empty() {
                    last_content_end = Some(end);
                }
            }
        }
        offset = end;
    }
    match heading_end {
        Some(heading_end) => {
            let anchor = last_content_end.unwrap_or(heading_end);
            let needs_newline = !text[..anchor].ends_with('\n');
            Edit {
                range: anchor..anchor,
                new_text: format!("{}{block}", if needs_newline { "\n" } else { "" }),
            }
        }
        None => {
            let prefix = if text.is_empty() {
                ""
            } else if text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            Edit {
                range: text.len()..text.len(),
                new_text: format!("{prefix}## {HIGHLIGHTS_HEADING}\n{block}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::apply_edits;
    use chrono::FixedOffset;

    fn tz() -> FixedOffset {
        FixedOffset::west_opt(7 * 3600).unwrap()
    }

    fn book() -> ReadwiseSource {
        ReadwiseSource {
            user_book_id: 28374651,
            title: "A Fé Na Era Do Ceticismo".to_string(),
            author: Some("Timothy Keller".to_string()),
            category: "books".to_string(),
            cover_image_url: Some(
                "https://m.media-amazon.com/images/I/91kqjaTsjHL._SY160.jpg".to_string(),
            ),
            asin: Some("B06XTSG7LR".to_string()),
            book_tags: vec![
                ReadwiseTag {
                    name: "faith".to_string(),
                },
                ReadwiseTag {
                    name: "apologetics".to_string(),
                },
            ],
            highlights: vec![highlight(512340987, 426, "2026-09-28T23:30:00Z")],
            ..Default::default()
        }
    }

    fn highlight(id: u64, location: i64, highlighted_at: &str) -> ReadwiseHighlight {
        ReadwiseHighlight {
            id,
            text: format!("Highlight {id}"),
            location: Some(location),
            location_type: Some("location".to_string()),
            highlighted_at: Some(highlighted_at.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn config_defaults_clamping_and_errors() {
        let config = parse_readwise_config("").unwrap();
        assert_eq!(config, ReadwiseConfig::default());
        assert_eq!(config.mappings, default_mappings());
        assert_eq!(config.poll_interval, Duration::from_secs(3600));
        assert_eq!(
            parse_readwise_config(DEFAULT_CONFIG_TOML).unwrap(),
            ReadwiseConfig::default()
        );

        let config = parse_readwise_config(
            "poll_minutes = 5\n\n[[sync]]\ncategory = \" Podcasts \"\npath = \"/reference/pods/\"\n\
             unknown = 1\n\n[[sync]]\ncategory = \"books\"\npath = \"reading/books\"\n",
        )
        .unwrap();
        assert_eq!(config.poll_interval, Duration::from_secs(15 * 60));
        assert_eq!(
            config.mappings,
            vec![
                ReadwiseMapping {
                    category: "podcasts".to_string(),
                    path: "reference/pods".to_string(),
                },
                ReadwiseMapping {
                    category: "books".to_string(),
                    path: "reading/books".to_string(),
                },
            ]
        );
        assert_eq!(config.folder_for("books"), Some("reading/books"));
        assert_eq!(config.folder_for("articles"), None);
        assert_eq!(
            parse_readwise_config("poll_minutes = 100000")
                .unwrap()
                .poll_interval,
            Duration::from_secs(1440 * 60)
        );

        let duplicate = parse_readwise_config(
            "[[sync]]\ncategory = \"books\"\npath = \"a\"\n\n[[sync]]\ncategory = \"BOOKS\"\npath = \"b\"\n",
        )
        .unwrap_err();
        assert_eq!(duplicate.to_string(), "two entries for \"books\"");
        let unknown =
            parse_readwise_config("[[sync]]\ncategory = \"bookz\"\npath = \"a\"\n").unwrap_err();
        assert_eq!(unknown.to_string(), "unknown category \"bookz\"");
        assert!(parse_readwise_config("[[sync]]\ncategory = \"books\"\n").is_err());
        assert!(parse_readwise_config("sync = }").is_err());
    }

    #[test]
    fn export_page_parses_the_api_shape() {
        let page: ExportPage = serde_json::from_str(
            r#"{"count": 1, "nextPageCursor": "abc", "results": [{
                "user_book_id": 7, "title": "T", "author": null, "category": "books",
                "book_tags": [{"id": 1, "name": "x"}], "asin": null, "cover_image_url": null,
                "highlights": [{"id": 9, "text": "hi", "location": null, "location_type": "none",
                    "note": "", "highlighted_at": null, "created_at": "2026-01-02T03:04:05.123Z",
                    "tags": [], "is_discard": false, "readwise_url": "https://readwise.io/open/9"}]
            }]}"#,
        )
        .unwrap();
        assert_eq!(page.next_page_cursor.as_deref(), Some("abc"));
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].highlights[0].id, 9);
        assert_eq!(
            page.results[0].highlights[0].day(&Utc),
            Some(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap())
        );
    }

    #[test]
    fn note_renders_in_the_plugin_template() {
        let book = book();
        let mut highlight = book.highlights[0].clone();
        highlight.text = "“Qual é seu maior problema em relação ao cristianismo?”".to_string();
        highlight.note = Some("use".to_string());
        let note = render_note(&book, &[&highlight], &tz());
        assert_eq!(
            note,
            "---\nsource: readwise\nreadwise_id: 28374651\ncategory: books\n---\n\
             # A Fé Na Era Do Ceticismo\n\n\
             ![rw-book-cover](https://m.media-amazon.com/images/I/91kqjaTsjHL._SY160.jpg)\n\n\
             ## Metadata\n- Author: [[Timothy Keller]]\n- Full Title: A Fé Na Era Do Ceticismo\n\
             - Category: #books\n- Tags: #faith #apologetics\n\n\
             ## Highlights\n\
             - “Qual é seu maior problema em relação ao cristianismo?” ([Location 426](https://readwise.io/to_kindle?action=open&asin=B06XTSG7LR&location=426)) <!--rw:512340987@2026-09-28-->\n\
             \x20   - Note: use\n"
        );
        assert_eq!(note_readwise_id(&note), Some(28374651));
        assert_eq!(scan_markers(&note), HashSet::from([512340987]));
    }

    #[test]
    fn marker_dates_follow_the_vault_time_zone() {
        // 23:30 UTC is still the 28th at UTC-7, but the 29th at UTC+3.
        let highlight = highlight(1, 1, "2026-09-28T23:30:00Z");
        assert_eq!(
            highlight_marker(1, highlight.day(&tz())),
            "<!--rw:1@2026-09-28-->"
        );
        let east = FixedOffset::east_opt(3 * 3600).unwrap();
        assert_eq!(
            highlight_marker(1, highlight.day(&east)),
            "<!--rw:1@2026-09-29-->"
        );
        // `created_at` is the fallback; nothing at all gives a dateless marker.
        let created_only = ReadwiseHighlight {
            id: 2,
            created_at: Some("2026-01-05T12:00:00+00:00".to_string()),
            ..Default::default()
        };
        assert_eq!(
            highlight_marker(2, created_only.day(&Utc)),
            "<!--rw:2@2026-01-05-->"
        );
        assert_eq!(
            highlight_marker(3, ReadwiseHighlight::default().day(&Utc)),
            "<!--rw:3-->"
        );
        assert_eq!(
            scan_markers("a <!--rw:1@2026-09-28--> b <!--rw:3--> <!--rw:x--> <!--gmail:4-->"),
            HashSet::from([1, 3])
        );
    }

    #[test]
    fn highlight_lines_follow_location_type_and_carry_tags() {
        let mut source = book();
        source.asin = None;
        source.source_url = Some("https://example.com/episode".to_string());
        let tz = tz();

        let mut highlight = highlight(1, 42, "2026-09-28T12:00:00Z");
        assert_eq!(
            render_highlight(&highlight, &source, &tz),
            "- Highlight 1 (Location 42) <!--rw:1@2026-09-28-->\n"
        );
        highlight.location_type = Some("page".to_string());
        assert_eq!(
            render_highlight(&highlight, &source, &tz),
            "- Highlight 1 (Page 42) <!--rw:1@2026-09-28-->\n"
        );
        highlight.location_type = Some("time_offset".to_string());
        highlight.location = Some(3725);
        assert_eq!(
            render_highlight(&highlight, &source, &tz),
            "- Highlight 1 ([1:02:05](https://example.com/episode)) <!--rw:1@2026-09-28-->\n"
        );
        highlight.location_type = Some("order".to_string());
        highlight.readwise_url = Some("https://readwise.io/open/1".to_string());
        highlight.tags = vec![
            ReadwiseTag {
                name: "key idea".to_string(),
            },
            ReadwiseTag {
                name: "faith".to_string(),
            },
        ];
        highlight.note = Some("my\nnote [[forged]] <!--rw:9-->".to_string());
        assert_eq!(
            render_highlight(&highlight, &source, &tz),
            "- Highlight 1 ([View Highlight](https://readwise.io/open/1)) <!--rw:1@2026-09-28-->\n\
             \x20   - Note: my note [ [forged] ] rw:9-->\n\
             \x20   - Tags: #key-idea #faith\n"
        );
        highlight.readwise_url = None;
        highlight.tags.clear();
        highlight.note = None;
        highlight.text = "Multi\nline <!--rw:7--> text".to_string();
        assert_eq!(
            render_highlight(&highlight, &source, &tz),
            "- Multi line rw:7--> text <!--rw:1@2026-09-28-->\n"
        );
    }

    #[test]
    fn stems_are_file_safe_and_collisions_get_suffixes() {
        assert_eq!(note_stem("Zero to One: Notes/Startups"), "Zero to One Notes Startups");
        assert_eq!(note_stem("  spaced \n out.  "), "spaced out");
        assert_eq!(note_stem("???"), "(untitled)");
        assert_eq!(note_stem("").len(), "(untitled)".len());
        let long = "x".repeat(200);
        assert_eq!(note_stem(&long).chars().count(), 120);

        let taken: HashSet<String> = ["Title".to_string(), "Title (2)".to_string()].into();
        assert_eq!(unique_stem("Title", &taken), "Title (3)");
        assert_eq!(unique_stem("Other", &taken), "Other");
    }

    fn config() -> ReadwiseConfig {
        ReadwiseConfig::default()
    }

    #[test]
    fn planner_creates_a_note_for_a_new_source_in_order() {
        let mut source = book();
        source.highlights = vec![
            highlight(3, 900, "2026-09-28T12:00:00Z"),
            highlight(1, 100, "2026-09-27T12:00:00Z"),
            highlight(2, 100, "2026-09-26T12:00:00Z"),
            ReadwiseHighlight {
                is_discard: true,
                ..highlight(4, 50, "2026-09-25T12:00:00Z")
            },
            ReadwiseHighlight {
                is_deleted: true,
                ..highlight(5, 60, "2026-09-25T12:00:00Z")
            },
        ];
        let changes = plan_readwise_sync(
            &[source],
            &config(),
            &ReadwiseVaultScan::default(),
            &LandedState::default(),
            &tz(),
        );
        assert_eq!(changes.len(), 1);
        let change = &changes[0];
        assert_eq!(
            change.rel_path,
            "reference/readwise/books/A Fé Na Era Do Ceticismo.md"
        );
        assert_eq!(change.book_id, 28374651);
        // Same location: the earlier highlight first; discards never land.
        assert_eq!(change.highlight_ids, vec![2, 1, 3]);
        let NoteWrite::Create { contents } = &change.write else {
            panic!("expected a create, got {:?}", change.write);
        };
        let order: Vec<usize> = ["Highlight 2", "Highlight 1", "Highlight 3"]
            .iter()
            .map(|text| contents.find(text).unwrap())
            .collect();
        assert!(order[0] < order[1] && order[1] < order[2], "{contents}");
        assert!(!contents.contains("Highlight 4"), "{contents}");
        assert!(!contents.contains("Highlight 5"), "{contents}");
    }

    #[test]
    fn planner_appends_only_what_is_new_and_skips_unmapped() {
        let mut source = book();
        source.highlights = vec![
            highlight(1, 100, "2026-09-27T12:00:00Z"),
            highlight(2, 200, "2026-09-28T12:00:00Z"),
            highlight(3, 50, "2026-09-29T12:00:00Z"),
        ];
        let mut article = book();
        article.user_book_id = 99;
        article.category = "articles".to_string();
        let mut gone = book();
        gone.user_book_id = 98;
        gone.is_deleted = true;

        let mut scan = ReadwiseVaultScan::default();
        scan.record(
            "reference/readwise/books",
            "A Fé Na Era Do Ceticismo",
            "reference/readwise/books/A Fé Na Era Do Ceticismo.md",
            "---\nsource: readwise\nreadwise_id: 28374651\n---\n## Highlights\n- x <!--rw:1@2026-09-27-->\n",
        );
        let landed = LandedState {
            highlights: HashSet::from([2]),
            books: HashSet::from([28374651]),
        };
        let changes =
            plan_readwise_sync(&[source, article, gone], &config(), &scan, &landed, &tz());
        assert_eq!(changes.len(), 1);
        let change = &changes[0];
        assert_eq!(
            change.rel_path,
            "reference/readwise/books/A Fé Na Era Do Ceticismo.md"
        );
        // 1 is marked in the note, 2 landed and was deleted from the note by
        // the user — only 3 is new, and it goes at the end even though it
        // sits earlier in the book.
        assert_eq!(change.highlight_ids, vec![3]);
        assert_eq!(
            change.write,
            NoteWrite::Append {
                lines: vec![
                    "- Highlight 3 ([Location 50](https://readwise.io/to_kindle?action=open&asin=B06XTSG7LR&location=50)) <!--rw:3@2026-09-29-->\n"
                        .to_string()
                ],
            }
        );
    }

    #[test]
    fn planner_recreates_a_deleted_note_with_only_new_highlights() {
        let mut source = book();
        source.highlights = vec![
            highlight(1, 100, "2026-09-27T12:00:00Z"),
            highlight(2, 200, "2026-09-28T12:00:00Z"),
        ];
        let landed = LandedState {
            highlights: HashSet::from([1]),
            books: HashSet::from([28374651]),
        };
        let mut scan = ReadwiseVaultScan::default();
        // The user's own file sits where the note wants to go.
        scan.record(
            "reference/readwise/books",
            "A Fé Na Era Do Ceticismo",
            "reference/readwise/books/A Fé Na Era Do Ceticismo.md",
            "# My own notes on the book\n",
        );
        let changes = plan_readwise_sync(&[source.clone()], &config(), &scan, &landed, &tz());
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].rel_path,
            "reference/readwise/books/A Fé Na Era Do Ceticismo (2).md"
        );
        assert_eq!(changes[0].highlight_ids, vec![2]);
        let NoteWrite::Create { contents } = &changes[0].write else {
            panic!("expected a create");
        };
        assert!(!contents.contains("Highlight 1"), "{contents}");
        assert!(contents.contains("Highlight 2"), "{contents}");

        // Nothing new: nothing planned, no empty note.
        let landed = LandedState {
            highlights: HashSet::from([1, 2]),
            books: HashSet::from([28374651]),
        };
        assert!(plan_readwise_sync(&[source], &config(), &scan, &landed, &tz()).is_empty());
    }

    #[test]
    fn append_edit_lands_after_the_last_highlight_block() {
        let note = "---\nreadwise_id: 1\n---\n# T\n\n## Metadata\n- Author: [[A]]\n\n\
                    ## Highlights\n- one <!--rw:1-->\n    - Note: mine\n\n## My thoughts\n\nKeep.\n";
        let lines = vec!["- two <!--rw:2-->\n".to_string()];
        let edited = apply_edits(note, vec![append_highlights_edit(note, &lines)]);
        assert_eq!(
            edited,
            "---\nreadwise_id: 1\n---\n# T\n\n## Metadata\n- Author: [[A]]\n\n\
             ## Highlights\n- one <!--rw:1-->\n    - Note: mine\n- two <!--rw:2-->\n\n## My thoughts\n\nKeep.\n"
        );

        // An empty section, a decorated heading, and a file without a final
        // newline all land cleanly.
        let sparse = "# T\n\n## 📌 Highlights:\n\n## Later\n";
        assert_eq!(
            apply_edits(sparse, vec![append_highlights_edit(sparse, &lines)]),
            "# T\n\n## 📌 Highlights:\n- two <!--rw:2-->\n\n## Later\n"
        );
        let unterminated = "## Highlights\n- one <!--rw:1-->";
        assert_eq!(
            apply_edits(
                unterminated,
                vec![append_highlights_edit(unterminated, &lines)]
            ),
            "## Highlights\n- one <!--rw:1-->\n- two <!--rw:2-->\n"
        );
        // A level-3 heading inside the section belongs to it.
        let nested = "## Highlights\n- one <!--rw:1-->\n### Chapter 2\n- three <!--rw:3-->\n";
        assert_eq!(
            apply_edits(nested, vec![append_highlights_edit(nested, &lines)]),
            "## Highlights\n- one <!--rw:1-->\n### Chapter 2\n- three <!--rw:3-->\n- two <!--rw:2-->\n"
        );
    }

    #[test]
    fn append_edit_recreates_a_missing_heading_at_the_end() {
        let note = "# T\n\n## Metadata\n- Author: [[A]]\n";
        let lines = vec!["- two <!--rw:2-->\n".to_string()];
        assert_eq!(
            apply_edits(note, vec![append_highlights_edit(note, &lines)]),
            "# T\n\n## Metadata\n- Author: [[A]]\n\n## Highlights\n- two <!--rw:2-->\n"
        );
        let unterminated = "# T\n\nprose";
        assert_eq!(
            apply_edits(
                unterminated,
                vec![append_highlights_edit(unterminated, &lines)]
            ),
            "# T\n\nprose\n\n## Highlights\n- two <!--rw:2-->\n"
        );
        assert_eq!(
            apply_edits("", vec![append_highlights_edit("", &lines)]),
            "## Highlights\n- two <!--rw:2-->\n"
        );
    }
}
