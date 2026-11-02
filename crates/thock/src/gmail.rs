//! Unified Gmail sync (spec `v15-unified-gmail-sync.md`): the `.thock/gmail.toml`
//! label → folder map, the transport trait, marker scanning, and the digest
//! bridge from V9. Everything here is pure string-in/string-out (no network,
//! no I/O); the GPUI service lives in `gmail_service.rs`, the note planner in
//! `inbox.rs`, and the backlog landing integration in `backlog.rs`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use gpui::{AsyncApp, Task};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Duration;
use thock_sync_core::{DecisionClass, decision_class, parse_triage_log_line};

use crate::calendar::GoogleClientOverride;
use crate::inbox::CapturedItem;

/// Lives next to `config.toml` in `.thock/`. A separate file for the same
/// forward-compat reason as `calendar.toml` (V8 §7.1): `config.toml` is
/// `deny_unknown_fields`, so a new table there would make older builds
/// declare the whole vault invalid.
pub const GMAIL_CONFIG_FILE: &str = "gmail.toml";

pub(crate) const MARKER_PREFIX: &str = "<!--gmail:";
pub(crate) const MARKER_SUFFIX: &str = "-->";

/// One `[[sync]]` entry: threads carrying `label` land as notes in the
/// vault-relative folder `path`. Labels route, folders mean (spec §4.1).
#[derive(Debug, Clone, PartialEq)]
pub struct SyncMapping {
    /// Matched case-insensitively against the label's full path name
    /// (`thock/inbox` is simply a label whose name contains a slash).
    pub label: String,
    /// Vault-relative landing folder, created on demand.
    pub path: String,
}

/// Resolved `.thock/gmail.toml` (spec §6). `account` and `google` come from
/// the cross-file Google settings resolution (V13 §7.4), never from here.
#[derive(Debug, Clone, PartialEq)]
pub struct GmailConfig {
    pub account: Option<String>,
    /// Ordered — the first mapping whose label a thread carries claims it.
    pub mappings: Vec<SyncMapping>,
    pub poll_interval: Duration,
    pub google: GoogleClientOverride,
    /// `[archive]`: which inbox decisions also archive the email in Gmail
    /// (V53 §6).
    pub archive: ArchiveConfig,
}

/// The `[archive]` table of `.thock/gmail.toml` (V53 §6).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ArchiveConfig {
    /// No table, or an empty `on`.
    #[default]
    Off,
    On(BTreeSet<DecisionClass>),
    /// The table is there but isn't in the expected shape: this feature is
    /// off and says so, while capture keeps running.
    Unreadable,
}

impl ArchiveConfig {
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On(_))
    }

    fn parse(value: Option<toml::Value>) -> Self {
        let Some(value) = value else {
            return Self::Off;
        };
        let Some(table) = value.as_table() else {
            return Self::Unreadable;
        };
        let Some(on) = table.get("on") else {
            return Self::Off;
        };
        let Some(names) = on.as_array() else {
            return Self::Unreadable;
        };
        let mut classes = BTreeSet::new();
        for name in names {
            let Some(name) = name.as_str() else {
                return Self::Unreadable;
            };
            match DecisionClass::from_name(name) {
                Some(class) => {
                    classes.insert(class);
                }
                None => log::warn!(
                    "Thock: ignoring unknown decision {name:?} in gmail.toml's [archive] on"
                ),
            }
        }
        if classes.is_empty() {
            Self::Off
        } else {
            Self::On(classes)
        }
    }
}

/// The shipped map, used when the file has no `[[sync]]` entries: backlog
/// first, so a both-labels thread keeps taking the fast lane (spec §4.1).
pub fn default_mappings() -> Vec<SyncMapping> {
    vec![
        SyncMapping {
            label: "thock/backlog".to_string(),
            path: crate::backlog::EMAIL_ARCHIVE_DIR.to_string(),
        },
        SyncMapping {
            label: "thock/inbox".to_string(),
            // Matches `InboxConfig`'s default `dir` — the setup skill keeps
            // the two aligned when either is customized (spec §6).
            path: "inbox".to_string(),
        },
    ]
}

impl Default for GmailConfig {
    fn default() -> Self {
        Self {
            account: None,
            mappings: default_mappings(),
            poll_interval: Duration::from_secs(300),
            google: GoogleClientOverride::default(),
            archive: ArchiveConfig::Off,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GmailConfigContent {
    schema: Option<u32>,
    poll_seconds: Option<u64>,
    sync: Vec<SyncContent>,
    archive: Option<toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SyncContent {
    label: Option<String>,
    path: Option<String>,
}

/// Parses `.thock/gmail.toml` (spec §6). Every field is optional; unknown
/// fields — including every schema-1 key — are ignored so hand-edited and
/// pre-V15 files don't break this build. An unparseable file is the caller's
/// cue to log and disable sync, never a panic.
pub fn parse_gmail_config(text: &str) -> Result<GmailConfig> {
    let content: GmailConfigContent = toml::from_str(text)?;
    let mut mappings: Vec<SyncMapping> = Vec::new();
    for entry in content.sync {
        let label = entry
            .label
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty());
        let path = entry
            .path
            .map(|path| path.trim().trim_matches('/').to_string())
            .filter(|path| !path.is_empty());
        let (Some(label), Some(path)) = (label, path) else {
            log::warn!("Thock: ignoring a [[sync]] entry missing label or path in gmail.toml");
            continue;
        };
        if mappings
            .iter()
            .any(|mapping| mapping.label.eq_ignore_ascii_case(&label))
        {
            log::warn!("Thock: ignoring duplicate [[sync]] label {label:?} in gmail.toml");
            continue;
        }
        mappings.push(SyncMapping { label, path });
    }
    if mappings.is_empty() {
        mappings = default_mappings();
    }
    Ok(GmailConfig {
        account: None,
        mappings,
        poll_interval: Duration::from_secs(content.poll_seconds.unwrap_or(300).clamp(60, 3600)),
        google: GoogleClientOverride::default(),
        archive: ArchiveConfig::parse(content.archive),
    })
}

/// One mapping's share of a poll, index-aligned with `GmailConfig::mappings`.
#[derive(Debug, Clone, PartialEq)]
pub enum MappingFetched {
    Items(Vec<CapturedItem>),
    /// The mapped label doesn't exist in the account — a holding state, not
    /// an error: creating the label is the last step of onboarding.
    LabelNotFound,
}

/// Transport abstraction (spec §7.1): Gmail REST is the implementation; IMAP
/// or Outlook can follow without touching the service. One fetch covers every
/// mapping so the claim pass — a thread lands once, first mapping wins — is
/// the transport's job, not something two services coordinate on.
pub trait MailTransport: Send + Sync {
    /// `skip` holds every digest the vault already knows (both V15 and V9
    /// constructions), so an already-captured thread costs no per-message
    /// request.
    fn fetch(&self, skip: &HashSet<String>, cx: &AsyncApp) -> Task<Result<GmailFetched>>;

    /// Takes the thread out of the Gmail inbox or puts it back, touching no
    /// other label (V53 §7.3). A thread that no longer exists is done, not an
    /// error. Fails with [`MissingPermission`] when the grant lacks
    /// `gmail.modify`, without sending anything when the grant is known.
    fn modify_thread(&self, thread_id: &str, change: ThreadMove, cx: &AsyncApp)
    -> Task<Result<()>>;

    /// Whether the grant allows [`MailTransport::modify_thread`], as far as
    /// the transport knows without asking; `None` when it can't tell.
    fn archive_permission(&self) -> Option<bool> {
        None
    }
}

/// What a Gmail write does to a thread's `INBOX` label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadMove {
    Archive,
    Unarchive,
}

/// The Google grant doesn't include `gmail.modify`: Gmail archive holds
/// until the user allows it, and capture carries on.
#[derive(Debug)]
pub struct MissingPermission;

impl std::fmt::Display for MissingPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Thock isn't allowed to archive in Gmail")
    }
}

impl std::error::Error for MissingPermission {}

/// A transport's answer for one poll.
#[derive(Debug, Clone, PartialEq)]
pub struct GmailFetched {
    /// Index-aligned with the configured mappings.
    pub mappings: Vec<MappingFetched>,
}

/// How long a decision sits in the log before Gmail hears of it, so an undo
/// right after the decision never reaches Google (V53 §7.1).
pub const ARCHIVE_GRACE: Duration = Duration::from_secs(60);
/// How long after Thock archived a thread an undo still puts it back.
/// Tidying old log lines by hand never touches Gmail.
pub const UNDO_HORIZON: Duration = Duration::from_secs(24 * 60 * 60);

/// One captured thread from `.thock/state/gmail/imported.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedThread {
    pub thread: String,
    /// The mapping folder that captured it.
    pub path: String,
}

/// Digest → thread, from the capture state file.
pub type ImportedIndex = HashMap<String, ImportedThread>;

/// Reads the capture state file; records without a thread or path (written
/// by older builds) can't be archived and are skipped.
pub fn parse_imported_index(text: &str) -> ImportedIndex {
    let mut index = ImportedIndex::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let field = |name: &str| record.get(name).and_then(|value| value.as_str());
        if let (Some(digest), Some(thread), Some(path)) =
            (field("digest"), field("thread"), field("path"))
        {
            index.insert(
                digest.to_string(),
                ImportedThread {
                    thread: thread.to_string(),
                    path: path.trim_matches('/').to_string(),
                },
            );
        }
    }
    index
}

/// One decision in the triage log that names a captured item.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggedDecision {
    pub digest: String,
    pub class: DecisionClass,
}

/// Every marked line of the triage log, in order, classified with the
/// vault's Backlog headings.
pub fn logged_decisions(
    log: &str,
    soon_heading: &str,
    someday_heading: &str,
) -> Vec<LoggedDecision> {
    log.lines()
        .filter_map(parse_triage_log_line)
        .map(|line| LoggedDecision {
            class: decision_class(&line.destination, soon_heading, someday_heading),
            digest: line.digest,
        })
        .collect()
}

/// Where one digest stands with Gmail archive.
#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveMark {
    /// Decided before the feature (or its class) was on: never archived.
    Baseline,
    /// Waiting out the grace period since `at`.
    Seen { at: DateTime<Utc> },
    /// Thock took the thread out of the Gmail inbox at `at`.
    Archived { thread: String, at: DateTime<Utc> },
    /// An undo put it back; a new decision may archive it again.
    Unarchived,
    /// The line left the log before the grace period ran out.
    Withdrawn,
}

/// One record of `.thock/state/gmail/archive.jsonl` (V53 §7.2).
#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveEvent {
    /// The classes `[archive] on` listed when this was written, so a class
    /// added later can be baselined even across restarts.
    On {
        classes: BTreeSet<DecisionClass>,
        at: DateTime<Utc>,
    },
    Baseline {
        digest: String,
        at: DateTime<Utc>,
    },
    Seen {
        digest: String,
        thread: String,
        at: DateTime<Utc>,
    },
    Archived {
        digest: String,
        thread: String,
        at: DateTime<Utc>,
    },
    Unarchived {
        digest: String,
        thread: String,
        at: DateTime<Utc>,
    },
    Withdrawn {
        digest: String,
        at: DateTime<Utc>,
    },
    /// A write that didn't go through. Informational: the mark stays, so the
    /// next pass retries.
    Failed {
        digest: String,
        thread: String,
        at: DateTime<Utc>,
        error: String,
    },
}

impl ArchiveEvent {
    pub fn to_json_line(&self) -> String {
        let at = |at: &DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let value = match self {
            Self::On { classes, at: when } => serde_json::json!({
                "event": "on",
                "classes": classes.iter().map(|class| class.name()).collect::<Vec<_>>(),
                "at": at(when),
            }),
            Self::Baseline { digest, at: when } => {
                serde_json::json!({"digest": digest, "event": "baseline", "at": at(when)})
            }
            Self::Withdrawn { digest, at: when } => {
                serde_json::json!({"digest": digest, "event": "withdrawn", "at": at(when)})
            }
            Self::Seen {
                digest,
                thread,
                at: when,
            } => serde_json::json!({
                "digest": digest, "thread": thread, "event": "seen", "at": at(when),
            }),
            Self::Archived {
                digest,
                thread,
                at: when,
            } => serde_json::json!({
                "digest": digest, "thread": thread, "event": "archived", "at": at(when),
            }),
            Self::Unarchived {
                digest,
                thread,
                at: when,
            } => serde_json::json!({
                "digest": digest, "thread": thread, "event": "unarchived", "at": at(when),
            }),
            Self::Failed {
                digest,
                thread,
                at: when,
                error,
            } => serde_json::json!({
                "digest": digest, "thread": thread, "event": "failed", "at": at(when),
                "error": error,
            }),
        };
        value.to_string()
    }

    pub fn parse(line: &str) -> Option<Self> {
        let record: serde_json::Value = serde_json::from_str(line).ok()?;
        let field = |name: &str| record.get(name).and_then(|value| value.as_str());
        let at = DateTime::parse_from_rfc3339(field("at")?)
            .ok()?
            .with_timezone(&Utc);
        if field("event")? == "on" {
            let classes = record
                .get("classes")?
                .as_array()?
                .iter()
                .filter_map(|name| name.as_str().and_then(DecisionClass::from_name))
                .collect();
            return Some(Self::On { classes, at });
        }
        let digest = field("digest")?.to_string();
        let thread = || field("thread").map(str::to_string);
        Some(match field("event")? {
            "baseline" => Self::Baseline { digest, at },
            "withdrawn" => Self::Withdrawn { digest, at },
            "seen" => Self::Seen {
                digest,
                thread: thread()?,
                at,
            },
            "archived" => Self::Archived {
                digest,
                thread: thread()?,
                at,
            },
            "unarchived" => Self::Unarchived {
                digest,
                thread: thread()?,
                at,
            },
            "failed" => Self::Failed {
                digest,
                thread: thread()?,
                at,
                error: field("error").unwrap_or_default().to_string(),
            },
            _ => return None,
        })
    }
}

/// The archive state file folded into the latest mark per digest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArchiveState {
    pub marks: BTreeMap<String, ArchiveMark>,
    /// The classes of the last `on` record; `None` before the first one.
    pub last_on: Option<BTreeSet<DecisionClass>>,
}

impl ArchiveState {
    /// Unreadable lines are skipped: losing a record costs at most one
    /// redundant `INBOX` removal.
    pub fn from_jsonl(text: &str) -> Self {
        let mut state = Self::default();
        for event in text.lines().filter_map(ArchiveEvent::parse) {
            state.apply(&event);
        }
        state
    }

    pub fn apply(&mut self, event: &ArchiveEvent) {
        let (digest, mark) = match event {
            ArchiveEvent::On { classes, .. } => {
                self.last_on = Some(classes.clone());
                return;
            }
            ArchiveEvent::Failed { .. } => return,
            ArchiveEvent::Baseline { digest, .. } => (digest, ArchiveMark::Baseline),
            ArchiveEvent::Seen { digest, at, .. } => (digest, ArchiveMark::Seen { at: *at }),
            ArchiveEvent::Archived { digest, thread, at } => (
                digest,
                ArchiveMark::Archived {
                    thread: thread.clone(),
                    at: *at,
                },
            ),
            ArchiveEvent::Unarchived { digest, .. } => (digest, ArchiveMark::Unarchived),
            ArchiveEvent::Withdrawn { digest, .. } => (digest, ArchiveMark::Withdrawn),
        };
        self.marks.insert(digest.clone(), mark);
    }
}

/// A Gmail write the reconciler wants sent.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchiveAction {
    pub digest: String,
    pub thread: String,
    pub change: ThreadMove,
}

/// One reconcile pass: records to append now, and writes to send.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArchivePlan {
    pub events: Vec<ArchiveEvent>,
    pub actions: Vec<ArchiveAction>,
}

/// The pure reconciler (V53 §7.1). Archives a logged decision once it has
/// been seen for [`ARCHIVE_GRACE`], when its class is in `on` and its thread
/// was captured into `inbox_dir`; puts back a thread Thock archived less than
/// [`UNDO_HORIZON`] ago whose line left the log. Decisions logged before a
/// class was turned on are baselined and never touched, and a thread is
/// archived at most once per decision, so a move back in Gmail sticks.
pub fn plan_archive(
    log: &[LoggedDecision],
    imported: &ImportedIndex,
    state: &ArchiveState,
    on: &BTreeSet<DecisionClass>,
    inbox_dir: &str,
    now: DateTime<Utc>,
) -> ArchivePlan {
    let mut plan = ArchivePlan::default();
    let inbox_dir = inbox_dir.trim_matches('/');
    let mut present: Vec<(&str, DecisionClass, &ImportedThread)> = Vec::new();
    // Every digest still in the log counts as present, captured here or not:
    // an unreadable capture record or a moved inbox folder must never read
    // as an undo.
    let mut present_digests: HashSet<&str> = HashSet::new();
    for decision in log.iter().rev() {
        // The newest line for a digest is its decision.
        if !present_digests.insert(&decision.digest) {
            continue;
        }
        if let Some(thread) = imported
            .get(&decision.digest)
            .filter(|thread| thread.path == inbox_dir)
        {
            present.push((&decision.digest, decision.class, thread));
        }
    }
    present.reverse();

    let mut baselined: HashSet<&str> = HashSet::new();
    if state.last_on.as_ref() != Some(on) {
        let previous = state.last_on.clone().unwrap_or_default();
        for (digest, class, _) in &present {
            if !on.contains(class) || previous.contains(class) {
                continue;
            }
            if matches!(
                state.marks.get(*digest),
                Some(ArchiveMark::Archived { .. } | ArchiveMark::Baseline)
            ) {
                continue;
            }
            baselined.insert(digest);
            plan.events.push(ArchiveEvent::Baseline {
                digest: digest.to_string(),
                at: now,
            });
        }
        plan.events.push(ArchiveEvent::On {
            classes: on.clone(),
            at: now,
        });
    }

    let grace = chrono::Duration::from_std(ARCHIVE_GRACE).unwrap_or_default();
    for (digest, class, thread) in &present {
        if !on.contains(class) || baselined.contains(digest) {
            continue;
        }
        match state.marks.get(*digest) {
            Some(ArchiveMark::Baseline | ArchiveMark::Archived { .. }) => {}
            Some(ArchiveMark::Seen { at }) => {
                if now - *at >= grace {
                    plan.actions.push(ArchiveAction {
                        digest: digest.to_string(),
                        thread: thread.thread.clone(),
                        change: ThreadMove::Archive,
                    });
                }
            }
            None | Some(ArchiveMark::Unarchived | ArchiveMark::Withdrawn) => {
                plan.events.push(ArchiveEvent::Seen {
                    digest: digest.to_string(),
                    thread: thread.thread.clone(),
                    at: now,
                });
            }
        }
    }

    let horizon = chrono::Duration::from_std(UNDO_HORIZON).unwrap_or_default();
    for (digest, mark) in &state.marks {
        if present_digests.contains(digest.as_str()) {
            continue;
        }
        match mark {
            ArchiveMark::Archived { thread, at } if now - *at < horizon => {
                plan.actions.push(ArchiveAction {
                    digest: digest.clone(),
                    thread: thread.clone(),
                    change: ThreadMove::Unarchive,
                });
            }
            ArchiveMark::Seen { .. } => plan.events.push(ArchiveEvent::Withdrawn {
                digest: digest.clone(),
                at: now,
            }),
            _ => {}
        }
    }
    plan
}

/// V9's digest: the first 12 hex characters of `sha256(account + "\0" +
/// thread_id)`. Kept read-only as the migration bridge (spec §9) — threads
/// recorded by V9/V13 state, `thread:` frontmatter, or old backlog markers
/// are recognized through it and never captured twice. New captures use
/// [`crate::inbox::capture_digest`]. Remove once the dogfood vault's record
/// has fully rolled over.
pub fn thread_marker_id(account: &str, thread_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(account.as_bytes());
    hasher.update([0u8]);
    hasher.update(thread_id.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(6)
        .fold(String::with_capacity(12), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The `u/<account>` form pins the link to the connected account, so
/// multi-account browsers open the right mailbox (V9 §5.1).
pub fn gmail_thread_url(account: &str, thread_id: &str) -> String {
    format!("https://mail.google.com/mail/u/{account}/#all/{thread_id}")
}

/// Every `<!--gmail:…-->` marker digest in the text, all sections included —
/// the backlog's half of the dedup contract (V9 §4.3, §5.2).
pub fn scan_backlog_markers(text: &str) -> HashSet<String> {
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

/// The `thread:` digest from a V9 archive file's frontmatter — the legacy
/// half of the rebuild scan (spec §9); new notes carry `capture:` instead
/// and are read by [`crate::inbox::inbox_note_digest`].
pub fn archive_frontmatter_digest(content: &str) -> Option<String> {
    let rest = content.strip_prefix("---\n")?;
    let (frontmatter, _) = rest.split_once("\n---")?;
    frontmatter.lines().find_map(|line| {
        let value = line.strip_prefix("thread:")?.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Subject → title text (V9 §5.1 rules 4–5): RFC 2047 decoding already
/// happened in the transport; here reply prefixes are stripped, marker
/// forgery is removed, and whitespace (newlines included) collapses to
/// single spaces.
pub fn sanitize_subject(raw: &str) -> String {
    let mut subject = raw.replace("<!--", "");
    loop {
        let trimmed = subject.trim_start();
        let lowered = trimmed.to_lowercase();
        let stripped = ["re:", "fwd:", "fw:"]
            .iter()
            .find(|prefix| lowered.starts_with(**prefix))
            .map(|prefix| trimmed[prefix.len()..].to_string());
        match stripped {
            Some(rest) => subject = rest,
            None => break,
        }
    }
    let subject = collapse_whitespace(&subject);
    if subject.is_empty() {
        "(no subject)".to_string()
    } else {
        subject
    }
}

pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Breaks apart `[[` / `]]` so a title cannot forge a wikilink.
pub(crate) fn break_wikilinks(text: &str) -> String {
    let mut text = text.to_string();
    while text.contains("[[") {
        text = text.replace("[[", "[ [");
    }
    while text.contains("]]") {
        text = text.replace("]]", "] ]");
    }
    text
}

/// Lowercase ASCII alphanumerics and dashes, ≤ 60 chars, `fallback` when
/// nothing survives (V9 §5.3). Shared with the inbox planner (V13 §6).
pub(crate) fn slug(text: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 60 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_overrides() {
        let config = parse_gmail_config("").unwrap();
        assert_eq!(config, GmailConfig::default());
        assert_eq!(config.mappings, default_mappings());
        assert_eq!(config.mappings[0].label, "thock/backlog");
        assert_eq!(config.mappings[0].path, "archives/emails");
        assert_eq!(config.mappings[1].label, "thock/inbox");
        assert_eq!(config.mappings[1].path, "inbox");
        assert_eq!(config.poll_interval, Duration::from_secs(300));

        let config = parse_gmail_config(
            "schema = 2\npoll_seconds = 10\n\n\
             [[sync]]\nlabel = \" thock/reading \"\npath = \"/reading/queue/\"\nunknown = 1\n\n\
             [[sync]]\nlabel = \"thock/backlog\"\npath = \"archives/emails\"\n",
        )
        .unwrap();
        // Clamped to the floor; explicit entries replace the defaults
        // entirely, in order.
        assert_eq!(config.poll_interval, Duration::from_secs(60));
        assert_eq!(
            config.mappings,
            vec![
                SyncMapping {
                    label: "thock/reading".to_string(),
                    path: "reading/queue".to_string(),
                },
                SyncMapping {
                    label: "thock/backlog".to_string(),
                    path: "archives/emails".to_string(),
                },
            ]
        );

        assert!(parse_gmail_config("sync = }").is_err());
    }

    #[test]
    fn legacy_schema_1_keys_are_ignored() {
        // A pre-V15 file — top-level label/import/archive_dir/account —
        // parses as schema 2 running the default map.
        let config = parse_gmail_config(
            "account = \"d@e.com\"\nlabel = \"thock/backlog\"\nimport = \"full\"\n\
             archive_dir = \"mail\"\nschema = 1\n\n[google]\nclient_id = \"id\"\n",
        )
        .unwrap();
        assert_eq!(config.mappings, default_mappings());
        assert_eq!(config.account, None);
    }

    #[test]
    fn invalid_and_duplicate_sync_entries_are_dropped() {
        let config = parse_gmail_config(
            "[[sync]]\nlabel = \"thock/inbox\"\npath = \"inbox\"\n\n\
             [[sync]]\nlabel = \"\"\npath = \"nowhere\"\n\n\
             [[sync]]\nlabel = \"no-path\"\n\n\
             [[sync]]\nlabel = \"THOCK/INBOX\"\npath = \"elsewhere\"\n",
        )
        .unwrap();
        assert_eq!(
            config.mappings,
            vec![SyncMapping {
                label: "thock/inbox".to_string(),
                path: "inbox".to_string(),
            }]
        );

        // Entries present but none usable: fall back to the defaults rather
        // than silently syncing nothing.
        let config = parse_gmail_config("[[sync]]\nlabel = \"only-label\"\n").unwrap();
        assert_eq!(config.mappings, default_mappings());
    }

    #[test]
    fn legacy_digest_is_short_and_stable() {
        let digest = thread_marker_id("diego@example.com", "t-1");
        assert_eq!(digest.len(), 12);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(digest, thread_marker_id("diego@example.com", "t-1"));
        assert_ne!(digest, thread_marker_id("other@example.com", "t-1"));
        assert_ne!(
            digest,
            crate::inbox::capture_digest("diego@example.com", "gmail", "t-1")
        );
    }

    #[test]
    fn backlog_markers_scan_and_ignore_junk() {
        let text = "- [ ] Pay invoice <!--gmail:4d1f9a02c7b3-->\n\
                    prose <!--gmail: bbbb -->\n<!--gmail:-->\n<!--inbox:cccc-->\n";
        assert_eq!(
            scan_backlog_markers(text),
            ["4d1f9a02c7b3", "bbbb"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn archive_frontmatter_digest_requires_frontmatter() {
        assert_eq!(archive_frontmatter_digest("# Note\nthread: nope\n"), None);
        assert_eq!(
            archive_frontmatter_digest("---\nsubject: x\nthread: abc123\n---\n\nbody\n").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn subjects_are_sanitized() {
        assert_eq!(sanitize_subject("Re: RE: fwd: Invoice"), "Invoice");
        assert_eq!(
            sanitize_subject("Sneaky <!--gmail:beef--> subject"),
            "Sneaky gmail:beef--> subject"
        );
        assert_eq!(sanitize_subject("  spread \n out  "), "spread out");
        assert_eq!(sanitize_subject("re: "), "(no subject)");
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_793_000_000 + seconds, 0).unwrap()
    }

    fn classes(names: &[DecisionClass]) -> BTreeSet<DecisionClass> {
        names.iter().copied().collect()
    }

    fn decision(digest: &str, class: DecisionClass) -> LoggedDecision {
        LoggedDecision {
            digest: digest.to_string(),
            class,
        }
    }

    fn imported() -> ImportedIndex {
        parse_imported_index(
            "{\"digest\":\"d-1\",\"thread\":\"t-1\",\"path\":\"inbox\"}\n\
             {\"digest\":\"d-2\",\"thread\":\"t-2\",\"path\":\"inbox\"}\n\
             {\"digest\":\"d-backlog\",\"thread\":\"t-b\",\"path\":\"archives/emails\"}\n\
             {\"digest\":\"d-old\"}\n",
        )
    }

    /// Runs passes the way the service does: plan, record, send, record.
    fn run(
        state: &mut ArchiveState,
        log: &[LoggedDecision],
        on: &BTreeSet<DecisionClass>,
        now: DateTime<Utc>,
    ) -> Vec<ArchiveAction> {
        let plan = plan_archive(log, &imported(), state, on, "inbox", now);
        for event in &plan.events {
            state.apply(event);
        }
        for action in &plan.actions {
            state.apply(&match action.change {
                ThreadMove::Archive => ArchiveEvent::Archived {
                    digest: action.digest.clone(),
                    thread: action.thread.clone(),
                    at: now,
                },
                ThreadMove::Unarchive => ArchiveEvent::Unarchived {
                    digest: action.digest.clone(),
                    thread: action.thread.clone(),
                    at: now,
                },
            });
        }
        plan.actions
    }

    fn archive(digest: &str, thread: &str) -> ArchiveAction {
        ArchiveAction {
            digest: digest.to_string(),
            thread: thread.to_string(),
            change: ThreadMove::Archive,
        }
    }

    fn unarchive(digest: &str, thread: &str) -> ArchiveAction {
        ArchiveAction {
            change: ThreadMove::Unarchive,
            ..archive(digest, thread)
        }
    }

    #[test]
    fn archive_config_parses_its_table() {
        assert_eq!(parse_gmail_config("").unwrap().archive, ArchiveConfig::Off);
        assert_eq!(
            parse_gmail_config("[archive]\non = []\n").unwrap().archive,
            ArchiveConfig::Off
        );
        assert_eq!(
            parse_gmail_config("[archive]\n").unwrap().archive,
            ArchiveConfig::Off
        );
        assert_eq!(
            parse_gmail_config("[archive]\non = [\"archive\", \"Soon\", \"trash\"]\n")
                .unwrap()
                .archive,
            ArchiveConfig::On(classes(&[DecisionClass::Soon, DecisionClass::Archive]))
        );
        // A table in the wrong shape turns off only this feature; capture's
        // mappings still parse.
        let config = parse_gmail_config(
            "archive = \"yes\"\n[[sync]]\nlabel = \"thock/inbox\"\npath = \"inbox\"\n",
        )
        .unwrap();
        assert_eq!(config.archive, ArchiveConfig::Unreadable);
        assert_eq!(config.mappings.len(), 1);
        assert_eq!(
            parse_gmail_config("[archive]\non = \"archive\"\n")
                .unwrap()
                .archive,
            ArchiveConfig::Unreadable
        );
        assert_eq!(
            parse_gmail_config("[archive]\non = [1]\n").unwrap().archive,
            ArchiveConfig::Unreadable
        );
    }

    #[test]
    fn decisions_read_from_the_log() {
        let log = "# Triage log\n\
                   - 2026-11-02 · Q4 → Archived <!--inbox:d-1-->\n\
                   - 2026-11-02 · Milk → Archived\n\
                   - 2026-11-02 · Call → Backlog · Next <!--inbox:d-2-->\n";
        assert_eq!(
            logged_decisions(log, "Next", "Later"),
            vec![
                decision("d-1", DecisionClass::Archive),
                decision("d-2", DecisionClass::Soon),
            ]
        );
    }

    #[test]
    fn state_round_trips_through_its_file() {
        let events = vec![
            ArchiveEvent::On {
                classes: classes(&[DecisionClass::Archive]),
                at: at(0),
            },
            ArchiveEvent::Baseline {
                digest: "d-0".to_string(),
                at: at(0),
            },
            ArchiveEvent::Seen {
                digest: "d-1".to_string(),
                thread: "t-1".to_string(),
                at: at(1),
            },
            ArchiveEvent::Archived {
                digest: "d-1".to_string(),
                thread: "t-1".to_string(),
                at: at(61),
            },
            ArchiveEvent::Failed {
                digest: "d-1".to_string(),
                thread: "t-1".to_string(),
                at: at(70),
                error: "503".to_string(),
            },
            ArchiveEvent::Seen {
                digest: "d-2".to_string(),
                thread: "t-2".to_string(),
                at: at(2),
            },
            ArchiveEvent::Withdrawn {
                digest: "d-2".to_string(),
                at: at(3),
            },
            ArchiveEvent::Unarchived {
                digest: "d-3".to_string(),
                thread: "t-3".to_string(),
                at: at(4),
            },
        ];
        let mut text = String::new();
        for event in &events {
            let line = event.to_json_line();
            assert_eq!(ArchiveEvent::parse(&line).as_ref(), Some(event), "{line}");
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str("not json\n");
        let state = ArchiveState::from_jsonl(&text);
        assert_eq!(state.last_on, Some(classes(&[DecisionClass::Archive])));
        assert_eq!(state.marks.get("d-0"), Some(&ArchiveMark::Baseline));
        // A failure leaves the mark it failed to move.
        assert_eq!(
            state.marks.get("d-1"),
            Some(&ArchiveMark::Archived {
                thread: "t-1".to_string(),
                at: at(61),
            })
        );
        assert_eq!(state.marks.get("d-2"), Some(&ArchiveMark::Withdrawn));
        assert_eq!(state.marks.get("d-3"), Some(&ArchiveMark::Unarchived));
    }

    #[test]
    fn decisions_before_the_opt_in_are_never_archived() {
        let on = classes(&[DecisionClass::Archive]);
        let mut state = ArchiveState::default();
        let log = vec![decision("d-1", DecisionClass::Archive)];
        assert_eq!(run(&mut state, &log, &on, at(0)), vec![]);
        assert_eq!(state.marks.get("d-1"), Some(&ArchiveMark::Baseline));
        assert_eq!(run(&mut state, &log, &on, at(3600)), vec![]);

        // A decision after the opt-in waits out the grace period.
        let log = vec![
            decision("d-1", DecisionClass::Archive),
            decision("d-2", DecisionClass::Archive),
        ];
        assert_eq!(run(&mut state, &log, &on, at(4000)), vec![]);
        assert_eq!(run(&mut state, &log, &on, at(4059)), vec![]);
        assert_eq!(
            run(&mut state, &log, &on, at(4060)),
            vec![archive("d-2", "t-2")]
        );
        // G7: archived once, never again, though the line stays.
        assert_eq!(run(&mut state, &log, &on, at(9000)), vec![]);
    }

    #[test]
    fn only_listed_classes_archive_and_new_classes_baseline() {
        let mut state = ArchiveState::default();
        let archive_only = classes(&[DecisionClass::Archive]);
        assert_eq!(run(&mut state, &[], &archive_only, at(0)), vec![]);

        let log = vec![decision("d-1", DecisionClass::Soon)];
        assert_eq!(run(&mut state, &log, &archive_only, at(10)), vec![]);
        assert_eq!(run(&mut state, &log, &archive_only, at(1000)), vec![]);
        assert_eq!(state.marks.get("d-1"), None);

        // Adding `soon` later baselines the `soon` line already logged…
        let with_soon = classes(&[DecisionClass::Archive, DecisionClass::Soon]);
        assert_eq!(run(&mut state, &log, &with_soon, at(2000)), vec![]);
        assert_eq!(state.marks.get("d-1"), Some(&ArchiveMark::Baseline));
        assert_eq!(run(&mut state, &log, &with_soon, at(3000)), vec![]);
        // …and applies to the ones that come after.
        let log = vec![
            decision("d-1", DecisionClass::Soon),
            decision("d-2", DecisionClass::Soon),
        ];
        assert_eq!(run(&mut state, &log, &with_soon, at(3100)), vec![]);
        assert_eq!(
            run(&mut state, &log, &with_soon, at(3160)),
            vec![archive("d-2", "t-2")]
        );
    }

    #[test]
    fn backlog_and_unknown_digests_are_ignored() {
        let on = classes(&[DecisionClass::Archive]);
        let mut state = ArchiveState::default();
        run(&mut state, &[], &on, at(0));
        let log = vec![
            decision("d-backlog", DecisionClass::Archive),
            decision("d-unknown", DecisionClass::Archive),
            decision("d-old", DecisionClass::Archive),
        ];
        assert_eq!(run(&mut state, &log, &on, at(10)), vec![]);
        assert_eq!(run(&mut state, &log, &on, at(1000)), vec![]);
        assert!(state.marks.is_empty(), "{state:?}");
    }

    #[test]
    fn undo_puts_the_thread_back_within_the_horizon() {
        let on = classes(&[DecisionClass::Archive]);
        let mut state = ArchiveState::default();
        run(&mut state, &[], &on, at(0));
        let log = vec![
            decision("d-1", DecisionClass::Archive),
            decision("d-2", DecisionClass::Archive),
        ];
        run(&mut state, &log, &on, at(10));
        assert_eq!(
            run(&mut state, &log, &on, at(100)),
            vec![archive("d-1", "t-1"), archive("d-2", "t-2")]
        );

        // d-1's line goes within a day: back in the inbox, once.
        let log = vec![decision("d-2", DecisionClass::Archive)];
        assert_eq!(
            run(&mut state, &log, &on, at(200)),
            vec![unarchive("d-1", "t-1")]
        );
        assert_eq!(run(&mut state, &log, &on, at(300)), vec![]);

        // A new decision for an undone thread archives it again.
        let log = vec![
            decision("d-2", DecisionClass::Archive),
            decision("d-1", DecisionClass::Archive),
        ];
        assert_eq!(run(&mut state, &log, &on, at(400)), vec![]);
        assert_eq!(
            run(&mut state, &log, &on, at(460)),
            vec![archive("d-1", "t-1")]
        );

        // d-2's line goes after a day: Gmail is left alone.
        let log = vec![decision("d-1", DecisionClass::Archive)];
        assert_eq!(run(&mut state, &log, &on, at(100 + 24 * 3600)), vec![]);
    }

    #[test]
    fn a_line_still_in_the_log_is_never_undone() {
        let on = classes(&[DecisionClass::Archive]);
        let mut state = ArchiveState::default();
        run(&mut state, &[], &on, at(0));
        let log = vec![decision("d-1", DecisionClass::Archive)];
        run(&mut state, &log, &on, at(10));
        assert_eq!(
            run(&mut state, &log, &on, at(100)),
            vec![archive("d-1", "t-1")]
        );
        // The capture record went missing, then the inbox folder moved: the
        // line is still there, so Gmail is left alone.
        let missing = plan_archive(&log, &ImportedIndex::new(), &state, &on, "inbox", at(200));
        assert_eq!(missing.actions, vec![]);
        let moved = plan_archive(&log, &imported(), &state, &on, "elsewhere", at(200));
        assert_eq!(moved.actions, vec![]);
    }

    #[test]
    fn undo_inside_the_grace_period_never_reaches_gmail() {
        let on = classes(&[DecisionClass::Archive]);
        let mut state = ArchiveState::default();
        run(&mut state, &[], &on, at(0));
        let log = vec![decision("d-1", DecisionClass::Archive)];
        assert_eq!(run(&mut state, &log, &on, at(10)), vec![]);
        assert_eq!(run(&mut state, &[], &on, at(30)), vec![]);
        assert_eq!(state.marks.get("d-1"), Some(&ArchiveMark::Withdrawn));
        assert_eq!(run(&mut state, &[], &on, at(1000)), vec![]);
        // Decided again later, the grace period starts over.
        assert_eq!(run(&mut state, &log, &on, at(2000)), vec![]);
        assert_eq!(run(&mut state, &log, &on, at(2030)), vec![]);
        assert_eq!(
            run(&mut state, &log, &on, at(2060)),
            vec![archive("d-1", "t-1")]
        );
    }

    #[test]
    fn turning_off_and_on_again_is_not_retroactive() {
        let on = classes(&[DecisionClass::Archive]);
        let mut state = ArchiveState::default();
        run(&mut state, &[], &on, at(0));
        let log = vec![decision("d-1", DecisionClass::Archive)];
        run(&mut state, &log, &on, at(10));
        // Off (the service records an empty `on`) before the grace ran out…
        state.apply(&ArchiveEvent::On {
            classes: BTreeSet::new(),
            at: at(20),
        });
        // …then on again: the pending line is now from before the opt-in.
        assert_eq!(run(&mut state, &log, &on, at(5000)), vec![]);
        assert_eq!(state.marks.get("d-1"), Some(&ArchiveMark::Baseline));
    }
}
