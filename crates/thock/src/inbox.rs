//! The Inbox landing zone (spec `v13-inbox-routine.md`): the `.thock/inbox.toml`
//! config, the `CapturedItem` every transport produces, the `InboxSource`
//! trait, and — the contract of the feature — the capture planner that turns
//! captured items into one note file each in `inbox/`. Everything here is
//! pure string-in/string-out (no network, no I/O, no GPUI); the service lives
//! in `inbox_service.rs`.

use anyhow::Result;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Utc};
use gpui::{AsyncApp, Task};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Duration;

use crate::gmail::{break_wikilinks, collapse_whitespace, slug};

/// Lives next to `config.toml` in `.thock/`. A sibling file, not a
/// `config.toml` table, for V8 §7.1's forward-compat reason. The account and
/// any client override come from `.thock/google.toml` (§7.4), never from
/// here.
pub const INBOX_CONFIG_FILE: &str = "inbox.toml";

/// The append-only triage log (spec §9.5), written by the triage skill and
/// the Inbox view; its markers are half the state-rebuild scan.
pub use thock_sync_core::TRIAGE_LOG_PATH;

const MARKER_PREFIX: &str = "<!--inbox:";
const MARKER_SUFFIX: &str = "-->";

/// Resolved `.thock/inbox.toml` (spec §10.2). Gmail routing left for
/// `.thock/gmail.toml`'s map in V15 — a `[gmail]` section here is now just
/// an ignored unknown table.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxConfig {
    /// Vault-relative landing zone.
    pub dir: String,
    pub poll_interval: Duration,
    pub tasks_enabled: bool,
    /// The Google Tasks list to capture from, by title; `None` means the
    /// account's default list.
    pub tasks_list: Option<String>,
}

impl Default for InboxConfig {
    fn default() -> Self {
        Self {
            dir: "inbox".to_string(),
            poll_interval: Duration::from_secs(300),
            tasks_enabled: true,
            tasks_list: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct InboxConfigContent {
    schema: Option<u32>,
    dir: Option<String>,
    poll_seconds: Option<u64>,
    tasks: InboxTasksContent,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct InboxTasksContent {
    enabled: Option<bool>,
    list: Option<String>,
}

/// Parses `.thock/inbox.toml`. Every field is optional with the defaults
/// above; unknown fields are ignored so future keys don't break this build.
/// An unparseable file is the caller's cue to log and disable capture —
/// never a panic.
pub fn parse_inbox_config(text: &str) -> Result<InboxConfig> {
    let content: InboxConfigContent = toml::from_str(text)?;
    let defaults = InboxConfig::default();
    Ok(InboxConfig {
        dir: content
            .dir
            .map(|dir| dir.trim().trim_matches('/').to_string())
            .filter(|dir| !dir.is_empty())
            .unwrap_or(defaults.dir),
        poll_interval: Duration::from_secs(content.poll_seconds.unwrap_or(300).clamp(60, 3600)),
        tasks_enabled: content.tasks.enabled.unwrap_or(defaults.tasks_enabled),
        tasks_list: content
            .tasks
            .list
            .map(|list| list.trim().to_string())
            .filter(|list| !list.is_empty()),
    })
}

/// What every transport produces (spec §7): one landing zone, many sources.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedItem {
    pub source: &'static str,
    /// Gmail thread id, Google Tasks task id — stable per source.
    pub external_id: String,
    pub title: String,
    /// The sender, when the source knows one (spec v15 §4.2).
    pub from: Option<String>,
    /// The thing the item is *about*, when there is one.
    pub url: Option<String>,
    /// A back-link into the source (a Gmail thread URL); never a `url`.
    pub link: Option<String>,
    pub body: Option<String>,
    /// The item's own moment (the email's date, the task's update time),
    /// already in the local offset; capture time is the fallback.
    pub occurred_at: Option<DateTime<FixedOffset>>,
    /// Only when the source carried one. Metadata for triage, not a schedule.
    pub due: Option<NaiveDate>,
}

/// What one poll asks a transport for (spec §7). Errors isolate per source.
pub trait InboxSource: Send + Sync {
    fn id(&self) -> &'static str;
    /// `skip` holds the digests the service already has, so an
    /// already-captured item costs no per-item request (V9's optimization —
    /// a source may ignore it; the planner dedups regardless).
    fn fetch(&self, skip: &HashSet<String>, cx: &AsyncApp) -> Task<Result<InboxFetched>>;
}

/// A source's answer for one poll.
#[derive(Debug, Clone, PartialEq)]
pub enum InboxFetched {
    Items(Vec<CapturedItem>),
    /// The configured label or list doesn't exist — a holding state, not an
    /// error: creating it is the last step of onboarding.
    Holding(String),
}

/// The dedup digest that travels in a note's `capture:` frontmatter and the
/// triage log's markers: the first 12 hex of
/// `sha256(account + "\0" + source + "\0" + external_id)` — V9 §5.2's
/// construction with the source folded in (spec §6).
pub fn capture_digest(account: &str, source: &str, external_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(account.as_bytes());
    hasher.update([0u8]);
    hasher.update(source.as_bytes());
    hasher.update([0u8]);
    hasher.update(external_id.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(6)
        .fold(String::with_capacity(12), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Title → frontmatter/heading text (spec §6, V9 §5.1 rule 4): `<!--`
/// stripped so a hostile title cannot forge a marker, `[[`/`]]` broken apart,
/// whitespace (newlines included) collapsed, `(untitled)` when empty.
pub fn sanitize_title(raw: &str) -> String {
    let title = collapse_whitespace(&break_wikilinks(&raw.replace("<!--", "")));
    if title.is_empty() {
        "(untitled)".to_string()
    } else {
        title
    }
}

/// A ready-to-write inbox note, vault-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxFile {
    pub rel_path: String,
    /// The filename without directory or `.md`.
    pub stem: String,
    pub digest: String,
    pub content: String,
}

/// What to remember about a capture once it is applied (spec §4.3): appended
/// to `.thock/state/inbox/imported.jsonl` only after the note write landed.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRecord {
    pub digest: String,
    pub source: &'static str,
    pub external_id: String,
    pub title: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct InboxPlan {
    /// Written first, create-if-missing; a crash after this leaves notes
    /// whose frontmatter repairs the state on the next poll (spec §10.3).
    pub files: Vec<InboxFile>,
    pub newly_imported: Vec<ImportRecord>,
}

impl InboxPlan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.newly_imported.is_empty()
    }
}

/// The capture planner (spec §8) — pure, no I/O. `imported` is the loaded
/// state; `vault_digests` is the rebuild scan's answer (every `capture:` in
/// a landed note's frontmatter plus every `<!--inbox:…-->` marker in the
/// triage log); `taken_stems` are the file stems already in `dir`, the
/// vault-relative landing folder. `captured_at` is an RFC 3339 timestamp
/// stamped by the caller, also the filename fallback for items without their
/// own moment.
pub fn plan_inbox_capture(
    items: &[CapturedItem],
    account: &str,
    imported: &HashSet<String>,
    vault_digests: &HashSet<String>,
    taken_stems: &HashSet<String>,
    dir: &str,
    captured_at: &str,
) -> InboxPlan {
    let mut items: Vec<&CapturedItem> = items.iter().collect();
    items.sort_by_key(|item| item.occurred_at);

    let mut plan = InboxPlan::default();
    let mut planned: HashSet<String> = HashSet::new();
    let mut claimed_stems: HashSet<String> = taken_stems.clone();

    for item in items {
        let digest = capture_digest(account, item.source, &item.external_id);
        if imported.contains(&digest) || !planned.insert(digest.clone()) {
            continue;
        }
        let title = sanitize_title(&item.title);
        let record = ImportRecord {
            digest: digest.clone(),
            source: item.source,
            external_id: item.external_id.clone(),
            title: title.clone(),
        };
        // Second guard (spec §4.3): a digest already in an inbox note or the
        // triage log means the state lost this item — repair the state,
        // never write a second file.
        if vault_digests.contains(&digest) {
            plan.newly_imported.push(record);
            continue;
        }
        let moment = item
            .occurred_at
            .or_else(|| DateTime::parse_from_rfc3339(captured_at).ok());
        let stamp = match moment {
            Some(moment) => moment.format("%Y-%m-%d-%H%M").to_string(),
            None => "undated".to_string(),
        };
        let base = format!("{stamp}-{}", slug(&title, "item"));
        // A collision with a *different* item appends the digest's first 4
        // hex (spec §6); a collision with the *same* item never reaches here
        // — its frontmatter digest already skipped it above.
        let short = digest.get(..4).unwrap_or(&digest);
        let stem = [
            base.clone(),
            format!("{base}-{short}"),
            format!("{base}-{digest}"),
        ]
        .into_iter()
        .find(|candidate| !claimed_stems.contains(candidate))
        // Unreachable in practice: the full digest is unique per item.
        .unwrap_or_else(|| format!("{base}-{digest}"));
        claimed_stems.insert(stem.clone());
        plan.files.push(InboxFile {
            rel_path: format!("{dir}/{stem}.md"),
            stem,
            digest: digest.clone(),
            content: render_inbox_note(item, &title, &digest, captured_at),
        });
        plan.newly_imported.push(record);
    }
    plan
}

/// One inbox note (spec §6): frontmatter the machinery reads, then the item's
/// text unaltered — a capture is never dropped silently, so an item with no
/// body and no url still gets a placeholder body.
fn render_inbox_note(item: &CapturedItem, title: &str, digest: &str, captured_at: &str) -> String {
    let mut note = String::from("---\n");
    let mut field = |key: &str, value: &str| {
        let _ = writeln!(note, "{:<9} {value}", format!("{key}:"));
    };
    field("source", item.source);
    field("capture", digest);
    field("captured", captured_at);
    field("title", title);
    if let Some(from) = &item.from {
        field("from", &collapse_whitespace(from));
    }
    if let Some(url) = &item.url {
        field("url", &collapse_whitespace(url));
    }
    if let Some(link) = &item.link {
        field("link", &collapse_whitespace(link));
    }
    if let Some(due) = &item.due {
        field("due", &due.format("%Y-%m-%d").to_string());
    }
    note.push_str("---\n\n");
    let _ = writeln!(note, "# {title}");
    let body = item
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty());
    if let Some(url) = &item.url {
        note.push('\n');
        let _ = writeln!(note, "{}", collapse_whitespace(url));
    }
    match body {
        Some(body) => {
            note.push('\n');
            note.push_str(body);
            note.push('\n');
        }
        None if item.url.is_none() => {
            note.push_str("\n_(no content)_\n");
        }
        None => {}
    }
    note
}

/// The `capture:` digest from an inbox note's frontmatter, for rebuilding
/// state from the vault (spec §4.3). A hand-written note without frontmatter
/// is a valid inbox item and simply has no digest.
pub fn inbox_note_digest(content: &str) -> Option<String> {
    let rest = content.strip_prefix("---\n")?;
    let (frontmatter, _) = rest.split_once("\n---")?;
    frontmatter.lines().find_map(|line| {
        let value = line.strip_prefix("capture:")?.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Every `<!--inbox:…-->` marker digest in the triage log — the other half of
/// the rebuild scan (spec §9.5).
pub fn scan_triage_log_markers(text: &str) -> HashSet<String> {
    let mut markers = HashSet::new();
    for chunk in text.split(MARKER_PREFIX).skip(1) {
        if let Some(id) = chunk.split(MARKER_SUFFIX).next() {
            let id = id.trim();
            if !id.is_empty() && id.len() <= 64 && !id.contains('\n') {
                markers.insert(id.to_string());
            }
        }
    }
    markers
}

/// One waiting item as the Inbox view reads it (V41 §8.1): the frontmatter
/// fields the capture planner writes, the body after it, and the two rules a
/// decision needs. Every field is optional because a hand-written note with
/// no frontmatter is a valid item (V13 §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct InboxItem {
    /// Vault-relative, `inbox/<name>.md`.
    pub path: String,
    pub stem: String,
    /// The level-1 heading when there is one (the phone edits the heading,
    /// not the frontmatter, so it is the newer of the two), else the
    /// frontmatter title, else the first heading or line, else the stem.
    pub title: String,
    pub source: Option<String>,
    pub from: Option<String>,
    pub url: Option<String>,
    pub link: Option<String>,
    /// RFC 3339 as written.
    pub captured: Option<String>,
    /// `YYYY-MM-DD` as written.
    pub due: Option<String>,
    /// The `capture:` digest.
    pub digest: Option<String>,
    /// Everything after the frontmatter.
    pub body: String,
    /// V40 §5's rule: a non-blank line after the heading.
    pub has_body: bool,
    /// An image line in the body.
    pub has_image: bool,
}

/// Parses an inbox note the way `render_inbox_note` writes one, forgivingly:
/// missing frontmatter, missing fields and an empty body are all valid.
pub fn parse_inbox_note(path: &str, content: &str) -> InboxItem {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.strip_suffix(".md").unwrap_or(name).to_string();
    let (fields, body) = split_frontmatter(content);
    let field = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    };
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let heading = first_line
        .strip_prefix('#')
        .map(|rest| rest.trim_start_matches('#').trim())
        .filter(|text| !text.is_empty());
    let title = match heading {
        Some(text) if first_line.starts_with("# ") => text.to_string(),
        _ => field("title")
            .or_else(|| heading.map(str::to_string))
            .or_else(|| (!first_line.is_empty()).then(|| first_line.to_string()))
            .unwrap_or_else(|| stem.clone()),
    };
    let has_image = body.lines().any(|line| {
        let line = line.trim();
        line.starts_with("![") && line.contains("](")
    });
    InboxItem {
        path: path.to_string(),
        stem,
        title,
        source: field("source"),
        from: field("from"),
        url: field("url"),
        link: field("link"),
        captured: field("captured"),
        due: field("due"),
        digest: field("capture"),
        body: body.to_string(),
        has_body: thock_sync_core::inbox_has_body(content),
        has_image,
    }
}

/// The frontmatter's `key: value` pairs and the text after its terminator.
/// An unterminated frontmatter swallows the note, as the phone reads it.
fn split_frontmatter(content: &str) -> (Vec<(String, String)>, &str) {
    let mut fields = Vec::new();
    let Some(rest) = content.strip_prefix("---") else {
        return (fields, content);
    };
    let Some(first_break) = rest.find('\n') else {
        return (fields, "");
    };
    if !rest[..first_break].trim().is_empty() {
        return (fields, content);
    }
    let mut cursor = &rest[first_break + 1..];
    loop {
        let (line, after) = match cursor.find('\n') {
            Some(index) => (&cursor[..index], &cursor[index + 1..]),
            None => (cursor, ""),
        };
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            return (fields, after);
        }
        if let Some((key, value)) = line.split_once(':') {
            fields.push((key.trim().to_string(), value.trim().to_string()));
        }
        if after.is_empty() {
            return (fields, "");
        }
        cursor = after;
    }
}

/// The identity of a note's bytes for the suggestion cache (V41 §7.5): an
/// edited item is asked again, the same bytes never cost twice.
pub fn note_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// Reads `.thock/state/inbox/last-triage`: one ISO timestamp, with or
/// without an offset, or a bare date.
pub fn parse_watermark(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(moment) = DateTime::parse_from_rfc3339(text) {
        return Some(moment.with_timezone(&Utc));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Some(naive.and_utc());
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
}

/// The ritual's own New / Carried over split (`triage-inbox.md` §1): the
/// note's `captured:` moment, or its mtime for a hand-written note, after
/// the last triage's watermark. Without a watermark everything is new.
pub fn is_new(
    item: &InboxItem,
    modified: Option<DateTime<Utc>>,
    watermark: Option<DateTime<Utc>>,
) -> bool {
    let Some(watermark) = watermark else {
        return true;
    };
    let moment = item
        .captured
        .as_deref()
        .and_then(|captured| DateTime::parse_from_rfc3339(captured).ok())
        .map(|captured| captured.with_timezone(&Utc))
        .or(modified);
    moment.is_none_or(|moment| moment > watermark)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    const ACCOUNT: &str = "diego@example.com";
    const CAPTURED_AT: &str = "2026-08-23T18:22:07-07:00";

    fn item(source: &'static str, id: &str, title: &str) -> CapturedItem {
        CapturedItem {
            source,
            external_id: id.to_string(),
            title: title.to_string(),
            from: None,
            url: None,
            link: None,
            body: None,
            occurred_at: Some(
                FixedOffset::west_opt(7 * 3600)
                    .unwrap()
                    .with_ymd_and_hms(2026, 8, 23, 18, 10, 0)
                    .unwrap(),
            ),
            due: None,
        }
    }

    fn plan(
        items: &[CapturedItem],
        imported: &HashSet<String>,
        vault_digests: &HashSet<String>,
        taken: &HashSet<String>,
    ) -> InboxPlan {
        plan_inbox_capture(
            items,
            ACCOUNT,
            imported,
            vault_digests,
            taken,
            "inbox",
            CAPTURED_AT,
        )
    }

    fn parse_rendered(captured: CapturedItem) -> (InboxItem, String) {
        let plan = plan(
            &[captured],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        let file = &plan.files[0];
        (
            parse_inbox_note(&file.rel_path, &file.content),
            file.content.clone(),
        )
    }

    #[test]
    fn parse_round_trips_every_rendered_shape() {
        let mut link = item("google-tasks", "task-1", "Ship it — a practical guide");
        link.url = Some("https://example.com/ship-it".to_string());
        link.due = NaiveDate::from_ymd_opt(2026, 8, 27);
        let (parsed, content) = parse_rendered(link);
        assert_eq!(
            parsed.path,
            "inbox/2026-08-23-1810-ship-it-a-practical-guide.md"
        );
        assert_eq!(parsed.stem, "2026-08-23-1810-ship-it-a-practical-guide");
        assert_eq!(parsed.title, "Ship it — a practical guide");
        assert_eq!(parsed.source.as_deref(), Some("google-tasks"));
        assert_eq!(parsed.url.as_deref(), Some("https://example.com/ship-it"));
        assert_eq!(parsed.due.as_deref(), Some("2026-08-27"));
        assert_eq!(parsed.captured.as_deref(), Some(CAPTURED_AT));
        assert_eq!(
            parsed.digest.as_deref(),
            Some(capture_digest(ACCOUNT, "google-tasks", "task-1").as_str())
        );
        assert_eq!(parsed.from, None);
        assert_eq!(parsed.link, None);
        assert!(content.ends_with(&parsed.body), "{content}");
        assert!(parsed.body.starts_with("\n# Ship it"), "{}", parsed.body);
        // The url line after the heading is a body (V40 §5).
        assert!(parsed.has_body);
        assert!(!parsed.has_image);

        let mut email = item("gmail", "t-1", "From the road");
        email.body = Some("Two thoughts.\n\n![whiteboard](/images/w.jpg)\n".to_string());
        email.link = Some("https://mail.google.com/mail/u/d@e.com/#all/t-1".to_string());
        email.from = Some("Ana <ana@example.com>".to_string());
        let (parsed, _) = parse_rendered(email);
        assert_eq!(parsed.from.as_deref(), Some("Ana <ana@example.com>"));
        assert_eq!(
            parsed.link.as_deref(),
            Some("https://mail.google.com/mail/u/d@e.com/#all/t-1")
        );
        assert_eq!(parsed.url, None);
        assert!(parsed.has_body);
        assert!(parsed.has_image);

        let (parsed, _) = parse_rendered(item("google-tasks", "task-2", ""));
        assert_eq!(parsed.title, "(untitled)");
        // The placeholder is the only line after the heading, so it is a body
        // the phone would link to as well.
        assert!(parsed.has_body);
    }

    #[test]
    fn hand_written_notes_are_items() {
        let parsed = parse_inbox_note("inbox/idea.md", "# Routines could carry state\n\nMaybe.\n");
        assert_eq!(parsed.title, "Routines could carry state");
        assert_eq!(parsed.digest, None);
        assert_eq!(parsed.source, None);
        assert!(parsed.has_body);

        let parsed = parse_inbox_note("inbox/scrap.md", "just a line\n");
        assert_eq!(parsed.title, "just a line");
        assert!(!parsed.has_body);

        let parsed = parse_inbox_note("inbox/empty.md", "");
        assert_eq!(parsed.title, "empty");
        assert_eq!(parsed.body, "");

        // A phone-edited heading wins over the frontmatter title.
        let parsed = parse_inbox_note(
            "inbox/x.md",
            "---\ntitle: Old\ncapture: abc\n---\n\n# New title\n",
        );
        assert_eq!(parsed.title, "New title");
        assert_eq!(parsed.digest.as_deref(), Some("abc"));
        assert!(!parsed.has_body);

        // Frontmatter without a level-1 heading keeps its title; a deeper
        // heading is only the fallback.
        let parsed = parse_inbox_note("inbox/y.md", "---\ntitle: Kept\n---\n\n## Sub\nbody\n");
        assert_eq!(parsed.title, "Kept");
        let parsed = parse_inbox_note("inbox/z.md", "## Sub\n");
        assert_eq!(parsed.title, "Sub");

        // An image line alone after the heading is both a body and a picture.
        let parsed = parse_inbox_note("inbox/p.md", "# Pic\n![a](/images/a.png)\n");
        assert!(parsed.has_body);
        assert!(parsed.has_image);
    }

    #[test]
    fn watermark_splits_new_from_carried_over() {
        let watermark = parse_watermark("2026-10-09T18:00:00Z");
        assert!(watermark.is_some());
        assert_eq!(parse_watermark("2026-10-09T18:00:00"), watermark);
        assert_eq!(parse_watermark(" 2026-10-09T18:00:00+00:00\n"), watermark);
        assert!(parse_watermark("2026-10-09").is_some());
        assert_eq!(parse_watermark("yesterday"), None);

        let mut captured = parse_inbox_note(
            "inbox/a.md",
            "---\ncaptured: 2026-10-09T18:42:00Z\n---\n# A\n",
        );
        assert!(is_new(&captured, None, watermark));
        captured.captured = Some("2026-10-09T17:00:00Z".to_string());
        assert!(!is_new(&captured, None, watermark));
        // Without a watermark the whole queue is new.
        assert!(is_new(&captured, None, None));

        let hand_written = parse_inbox_note("inbox/b.md", "# B\n");
        let later = parse_watermark("2026-10-09T19:00:00Z");
        assert!(is_new(&hand_written, later, watermark));
        let earlier = parse_watermark("2026-10-09T10:00:00Z");
        assert!(!is_new(&hand_written, earlier, watermark));
        // Nothing to date it by: shown as new rather than hidden.
        assert!(is_new(&hand_written, None, watermark));
    }

    #[test]
    fn note_hash_is_sha256_of_the_bytes() {
        assert_eq!(
            note_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_ne!(note_hash("a"), note_hash("b"));
    }

    #[test]
    fn config_defaults_and_overrides() {
        let config = parse_inbox_config("").unwrap();
        assert_eq!(config, InboxConfig::default());
        assert_eq!(config.dir, "inbox");
        assert!(config.tasks_enabled);
        assert_eq!(config.tasks_list, None);

        // A pre-V15 [gmail] section is just an unknown table now.
        let config = parse_inbox_config(
            "schema = 1\ndir = \"/drop/zone/\"\npoll_seconds = 10\n\n\
             [gmail]\nenabled = false\nlabel = \" thock/capture \"\n\n\
             [tasks]\nlist = \"Thock\"\nunknown = 1\n",
        )
        .unwrap();
        assert_eq!(config.dir, "drop/zone");
        // Clamped to the floor.
        assert_eq!(config.poll_interval, Duration::from_secs(60));
        assert!(config.tasks_enabled);
        assert_eq!(config.tasks_list.as_deref(), Some("Thock"));

        assert!(parse_inbox_config("dir = }").is_err());
    }

    #[test]
    fn digest_is_short_stable_and_source_qualified() {
        let digest = capture_digest(ACCOUNT, "google-tasks", "task-1");
        assert_eq!(digest.len(), 12);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(digest, capture_digest(ACCOUNT, "google-tasks", "task-1"));
        assert_ne!(digest, capture_digest(ACCOUNT, "gmail", "task-1"));
        assert_ne!(
            digest,
            capture_digest("other@example.com", "google-tasks", "task-1")
        );
    }

    #[test]
    fn titles_are_sanitized() {
        assert_eq!(sanitize_title("  Ship\n it   now "), "Ship it now");
        assert_eq!(
            sanitize_title("Sneaky <!--inbox:beef--> title"),
            "Sneaky inbox:beef--> title"
        );
        assert_eq!(sanitize_title("Click [[evil]] now"), "Click [ [evil] ] now");
        assert_eq!(sanitize_title("   "), "(untitled)");
    }

    #[test]
    fn fresh_item_renders_one_note() {
        let mut captured = item("google-tasks", "task-1", "Ship it — a practical guide");
        captured.url = Some("https://example.com/ship-it".to_string());
        captured.due = NaiveDate::from_ymd_opt(2026, 8, 27);
        let plan = plan(
            &[captured],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(plan.files.len(), 1);
        let file = &plan.files[0];
        let digest = capture_digest(ACCOUNT, "google-tasks", "task-1");
        assert_eq!(file.stem, "2026-08-23-1810-ship-it-a-practical-guide");
        assert_eq!(file.rel_path, format!("inbox/{}.md", file.stem));
        for needle in [
            "source:   google-tasks",
            &format!("capture:  {digest}"),
            &format!("captured: {CAPTURED_AT}"),
            "title:    Ship it — a practical guide",
            "url:      https://example.com/ship-it",
            "due:      2026-08-27",
            "# Ship it — a practical guide\n\nhttps://example.com/ship-it\n",
        ] {
            assert!(
                file.content.contains(needle),
                "missing {needle} in {}",
                file.content
            );
        }
        assert_eq!(
            inbox_note_digest(&file.content).as_deref(),
            Some(digest.as_str())
        );
        assert_eq!(plan.newly_imported.len(), 1);
        assert_eq!(plan.newly_imported[0].digest, digest);
    }

    #[test]
    fn empty_item_is_never_dropped_silently() {
        let plan = plan(
            &[item("google-tasks", "task-1", "")],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        let file = &plan.files[0];
        assert!(file.stem.ends_with("-untitled"), "{}", file.stem);
        assert!(file.content.contains("title:    (untitled)"));
        assert!(file.content.contains("# (untitled)\n\n_(no content)_\n"));
    }

    #[test]
    fn body_and_link_are_carried() {
        let mut captured = item("gmail", "t-1", "From the road");
        captured.body = Some("Two thoughts.\n\nAnd a third.\n".to_string());
        captured.link = Some("https://mail.google.com/mail/u/d@e.com/#all/t-1".to_string());
        captured.from = Some("Ana <ana@example.com>".to_string());
        let plan = plan(
            &[captured],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        let content = &plan.files[0].content;
        assert!(
            content.contains("from:     Ana <ana@example.com>"),
            "{content}"
        );
        assert!(content.contains("link:     https://mail.google.com/mail/u/d@e.com/#all/t-1"));
        assert!(!content.contains("url:"), "{content}");
        assert!(
            content.ends_with("# From the road\n\nTwo thoughts.\n\nAnd a third.\n"),
            "{content}"
        );
    }

    #[test]
    fn item_without_a_moment_falls_back_to_capture_time() {
        let mut captured = item("google-tasks", "task-1", "Undated");
        captured.occurred_at = None;
        let plan = plan(
            &[captured],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(plan.files[0].stem, "2026-08-23-1822-undated");
    }

    #[test]
    fn stem_collisions_get_digest_suffixes() {
        let items = vec![
            item("google-tasks", "task-1", "Same title"),
            item("google-tasks", "task-2", "Same title"),
        ];
        let taken: HashSet<String> = ["2026-08-23-1810-same-title".to_string()].into();
        let plan = plan(&items, &HashSet::new(), &HashSet::new(), &taken);
        assert_eq!(plan.files.len(), 2);
        let digest_1 = capture_digest(ACCOUNT, "google-tasks", "task-1");
        let digest_2 = capture_digest(ACCOUNT, "google-tasks", "task-2");
        // Both dodge the on-disk stem, and each other.
        assert_eq!(
            plan.files[0].stem,
            format!("2026-08-23-1810-same-title-{}", &digest_1[..4])
        );
        assert_eq!(
            plan.files[1].stem,
            format!("2026-08-23-1810-same-title-{}", &digest_2[..4])
        );
    }

    #[test]
    fn state_and_vault_digests_both_dedup() {
        let items = vec![
            item("google-tasks", "task-1", "In the state"),
            item("google-tasks", "task-2", "In the vault"),
            item("google-tasks", "task-3", "Fresh"),
        ];
        let imported: HashSet<String> = [capture_digest(ACCOUNT, "google-tasks", "task-1")].into();
        let vault: HashSet<String> = [capture_digest(ACCOUNT, "google-tasks", "task-2")].into();
        let plan = plan(&items, &imported, &vault, &HashSet::new());
        // task-1 skipped outright, task-2 skipped but repaired into the
        // state, task-3 captured.
        assert_eq!(plan.files.len(), 1);
        assert!(plan.files[0].content.contains("title:    Fresh"));
        let repaired: Vec<&str> = plan
            .newly_imported
            .iter()
            .map(|record| record.external_id.as_str())
            .collect();
        assert_eq!(repaired, vec!["task-2", "task-3"]);
    }

    #[test]
    fn plan_is_idempotent_with_and_without_state() {
        let items = vec![
            item("google-tasks", "task-1", "First"),
            item("gmail", "t-2", "Second"),
        ];
        let first = plan(&items, &HashSet::new(), &HashSet::new(), &HashSet::new());
        assert_eq!(first.files.len(), 2);

        let imported: HashSet<String> = first
            .newly_imported
            .iter()
            .map(|record| record.digest.clone())
            .collect();
        let written_stems: HashSet<String> =
            first.files.iter().map(|file| file.stem.clone()).collect();
        let written_digests: HashSet<String> = first
            .files
            .iter()
            .map(|file| inbox_note_digest(&file.content).unwrap())
            .collect();

        // With the state updated (the normal next poll): nothing at all.
        let replan = plan(&items, &imported, &written_digests, &written_stems);
        assert!(replan.is_empty(), "{replan:?}");

        // With the state lost (crash between file write and state write):
        // the frontmatter scan catches what the state doesn't — no files,
        // and the state is repaired.
        let replan = plan(&items, &HashSet::new(), &written_digests, &written_stems);
        assert!(replan.files.is_empty(), "{replan:?}");
        assert_eq!(replan.newly_imported.len(), 2);
    }

    #[test]
    fn frontmatter_digest_requires_frontmatter() {
        assert_eq!(inbox_note_digest("# Hand written\ncapture: nope\n"), None);
        assert_eq!(
            inbox_note_digest("---\nsource: gmail\ncapture:  abc123\n---\n\nbody\n").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn triage_log_markers_scan_and_ignore_junk() {
        let text = "- 2026-08-23 · Ship it → Backlog · Someday <!--inbox:4d1f9a02c7b3-->\n\
                    prose <!--inbox: bbbb -->\n<!--inbox:-->\n<!--gmail:cccc-->\n";
        assert_eq!(
            scan_triage_log_markers(text),
            ["4d1f9a02c7b3", "bbbb"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn hostile_bodies_cannot_forge_frontmatter() {
        let mut captured = item("gmail", "t-1", "Sneaky");
        captured.body = Some("---\ncapture: ffffffffffff\n---\n".to_string());
        let plan = plan(
            &[captured],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        let digest = capture_digest(ACCOUNT, "gmail", "t-1");
        // The scanner stops at the real frontmatter terminator, so the body's
        // fake block is inert.
        assert_eq!(
            inbox_note_digest(&plan.files[0].content).as_deref(),
            Some(digest.as_str())
        );
    }
}
