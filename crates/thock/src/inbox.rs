//! The Inbox landing zone (spec `v13-inbox-routine.md`): the `.thock/inbox.toml`
//! config, the `CapturedItem` every transport produces, the `InboxSource`
//! trait, and — the contract of the feature — the capture planner that turns
//! captured items into one note file each in `inbox/`. Everything here is
//! pure string-in/string-out (no network, no I/O, no GPUI); the service lives
//! in `inbox_service.rs`.

use anyhow::Result;
use chrono::{DateTime, FixedOffset, NaiveDate};
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

/// The append-only triage log (spec §9.5), written by the triage skill; its
/// markers are half the state-rebuild scan.
pub const TRIAGE_LOG_PATH: &str = "archives/inbox/triage-log.md";

const MARKER_PREFIX: &str = "<!--inbox:";
const MARKER_SUFFIX: &str = "-->";

/// Resolved `.thock/inbox.toml` (spec §10.2).
#[derive(Debug, Clone, PartialEq)]
pub struct InboxConfig {
    /// Vault-relative landing zone.
    pub dir: String,
    pub poll_interval: Duration,
    pub gmail_enabled: bool,
    /// The Gmail label that means "capture into the inbox", matched
    /// case-insensitively against the label's full path name.
    pub gmail_label: String,
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
            gmail_enabled: true,
            gmail_label: "thock/inbox".to_string(),
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
    gmail: InboxGmailContent,
    tasks: InboxTasksContent,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct InboxGmailContent {
    enabled: Option<bool>,
    label: Option<String>,
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
        gmail_enabled: content.gmail.enabled.unwrap_or(defaults.gmail_enabled),
        gmail_label: content
            .gmail
            .label
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty())
            .unwrap_or(defaults.gmail_label),
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
/// an inbox note's frontmatter plus every `<!--inbox:…-->` marker in the
/// triage log); `taken_stems` are the file stems already in the inbox dir.
/// `captured_at` is an RFC 3339 timestamp stamped by the caller, also the
/// filename fallback for items without their own moment.
pub fn plan_inbox_capture(
    items: &[CapturedItem],
    account: &str,
    imported: &HashSet<String>,
    vault_digests: &HashSet<String>,
    taken_stems: &HashSet<String>,
    config: &InboxConfig,
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
        let stem = [base.clone(), format!("{base}-{short}"), format!("{base}-{digest}")]
            .into_iter()
            .find(|candidate| !claimed_stems.contains(candidate))
            // Unreachable in practice: the full digest is unique per item.
            .unwrap_or_else(|| format!("{base}-{digest}"));
        claimed_stems.insert(stem.clone());
        plan.files.push(InboxFile {
            rel_path: format!("{}/{stem}.md", config.dir),
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
            &InboxConfig::default(),
            CAPTURED_AT,
        )
    }

    #[test]
    fn config_defaults_and_overrides() {
        let config = parse_inbox_config("").unwrap();
        assert_eq!(config, InboxConfig::default());
        assert_eq!(config.dir, "inbox");
        assert_eq!(config.gmail_label, "thock/inbox");
        assert!(config.gmail_enabled);
        assert!(config.tasks_enabled);
        assert_eq!(config.tasks_list, None);

        let config = parse_inbox_config(
            "schema = 1\ndir = \"/drop/zone/\"\npoll_seconds = 10\n\n\
             [gmail]\nenabled = false\nlabel = \" thock/capture \"\n\n\
             [tasks]\nlist = \"Thock\"\nunknown = 1\n",
        )
        .unwrap();
        assert_eq!(config.dir, "drop/zone");
        // Clamped to the floor.
        assert_eq!(config.poll_interval, Duration::from_secs(60));
        assert!(!config.gmail_enabled);
        assert_eq!(config.gmail_label, "thock/capture");
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
        assert_ne!(digest, capture_digest("other@example.com", "google-tasks", "task-1"));
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
            assert!(file.content.contains(needle), "missing {needle} in {}", file.content);
        }
        assert_eq!(inbox_note_digest(&file.content).as_deref(), Some(digest.as_str()));
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
        let plan = plan(
            &[captured],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        let content = &plan.files[0].content;
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
        let imported: HashSet<String> =
            [capture_digest(ACCOUNT, "google-tasks", "task-1")].into();
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
            ["4d1f9a02c7b3", "bbbb"].into_iter().map(str::to_string).collect()
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
