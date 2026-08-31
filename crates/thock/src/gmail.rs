//! Unified Gmail sync (spec `v15-unified-gmail-sync.md`): the `.thock/gmail.toml`
//! label → folder map, the transport trait, marker scanning, and the digest
//! bridge from V9. Everything here is pure string-in/string-out (no network,
//! no I/O); the GPUI service lives in `gmail_service.rs`, the note planner in
//! `inbox.rs`, and the backlog landing integration in `backlog.rs`.

use anyhow::Result;
use gpui::{AsyncApp, Task};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Duration;

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
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GmailConfigContent {
    schema: Option<u32>,
    poll_seconds: Option<u64>,
    sync: Vec<SyncContent>,
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
}

/// A transport's answer for one poll.
#[derive(Debug, Clone, PartialEq)]
pub struct GmailFetched {
    /// Index-aligned with the configured mappings.
    pub mappings: Vec<MappingFetched>,
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
            archive_frontmatter_digest("---\nsubject: x\nthread: abc123\n---\n\nbody\n")
                .as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn subjects_are_sanitized() {
        assert_eq!(sanitize_subject("Re: RE: fwd: Invoice"), "Invoice");
        assert_eq!(sanitize_subject("Sneaky <!--gmail:beef--> subject"), "Sneaky gmail:beef--> subject");
        assert_eq!(sanitize_subject("  spread \n out  "), "spread out");
        assert_eq!(sanitize_subject("re: "), "(no subject)");
    }
}
