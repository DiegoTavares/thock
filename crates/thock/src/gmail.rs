//! Gmail capture into the Backlog (spec `v9-gmail-backlog-capture.md`): the
//! provider trait, the `.thock/gmail.toml` config, marker-id derivation, and
//! — the contract of the feature — the capture planner that turns labeled
//! emails into archive files plus one append-to-Someday edit. Everything here
//! is pure string-in/string-out (no network, no I/O); the GPUI service lives
//! in `gmail_service.rs`.

use anyhow::{Result, bail};
use chrono::{DateTime, Local};
use gpui::{AsyncApp, Task};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Duration;

use crate::backlog::{Edit, SectionKind, append_to_section_edit};
use crate::calendar::GoogleClientOverride;

/// Lives next to `config.toml` in `.thock/`. A separate file for the same
/// forward-compat reason as `calendar.toml` (V8 §7.1): `config.toml` is
/// `deny_unknown_fields`, so a new table there would make older builds
/// declare the whole vault invalid.
pub const GMAIL_CONFIG_FILE: &str = "gmail.toml";

const MARKER_PREFIX: &str = "<!--gmail:";
const MARKER_SUFFIX: &str = "-->";

/// What a captured task carries (spec §5.1): `Title` links out to the email
/// in Gmail; `Full` archives the body into the vault and links the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Title,
    Full,
}

/// Resolved `.thock/gmail.toml` (spec §7).
#[derive(Debug, Clone, PartialEq)]
pub struct GmailConfig {
    pub account: Option<String>,
    /// The Gmail label that means "capture me", matched case-insensitively
    /// by name.
    pub label: String,
    pub import: ImportMode,
    /// Vault-relative directory for archived emails (full mode).
    pub archive_dir: String,
    pub poll_interval: Duration,
    pub google: GoogleClientOverride,
}

impl Default for GmailConfig {
    fn default() -> Self {
        Self {
            account: None,
            label: "backlog".to_string(),
            import: ImportMode::Title,
            archive_dir: "archives/emails".to_string(),
            poll_interval: Duration::from_secs(300),
            google: GoogleClientOverride::default(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GmailConfigContent {
    schema: Option<u32>,
    account: Option<String>,
    label: Option<String>,
    import: Option<String>,
    archive_dir: Option<String>,
    poll_seconds: Option<u64>,
    google: GoogleContent,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GoogleContent {
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// Parses `.thock/gmail.toml` (spec §7). Every field is optional with the
/// defaults above; unknown fields are ignored so future keys don't break this
/// build. An unparseable file is the caller's cue to log and disable capture
/// — never a panic.
pub fn parse_gmail_config(text: &str) -> Result<GmailConfig> {
    let content: GmailConfigContent = toml::from_str(text)?;
    let defaults = GmailConfig::default();
    let import = match content.import.as_deref().map(str::trim) {
        None | Some("") => defaults.import,
        Some(value) if value.eq_ignore_ascii_case("title") => ImportMode::Title,
        Some(value) if value.eq_ignore_ascii_case("full") => ImportMode::Full,
        Some(other) => bail!("unknown import mode {other:?} — use \"title\" or \"full\""),
    };
    Ok(GmailConfig {
        account: content.account.filter(|account| !account.trim().is_empty()),
        label: content
            .label
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty())
            .unwrap_or(defaults.label),
        import,
        archive_dir: content
            .archive_dir
            .map(|dir| dir.trim().trim_matches('/').to_string())
            .filter(|dir| !dir.is_empty())
            .unwrap_or(defaults.archive_dir),
        poll_interval: Duration::from_secs(content.poll_seconds.unwrap_or(300).clamp(60, 3600)),
        google: GoogleClientOverride {
            client_id: content.google.client_id,
            client_secret: content.google.client_secret,
        },
    })
}

/// One labeled thread, represented by its most recent message (spec §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedEmail {
    pub thread_id: String,
    pub subject: String,
    pub from: String,
    pub date: DateTime<Local>,
    /// The plain-text body; populated only in full mode, `None` when the
    /// message had no readable text part.
    pub body: Option<String>,
}

/// A provider's answer for one poll.
#[derive(Debug, Clone, PartialEq)]
pub enum MailFetched {
    Emails(Vec<CapturedEmail>),
    /// The configured label doesn't exist in the account — a holding state,
    /// not an error: creating the label is the last step of onboarding.
    LabelNotFound,
}

/// Transport abstraction (spec §4.4): Gmail REST is the V9 implementation;
/// IMAP or Outlook can follow without touching the planner.
pub trait MailProvider: Send + Sync {
    /// Threads currently carrying the capture label, newest message per
    /// thread, skipping threads whose marker digest is in `skip` so
    /// already-captured mail costs no per-message request.
    fn fetch_labeled(
        &self,
        mode: ImportMode,
        skip: &HashSet<String>,
        cx: &AsyncApp,
    ) -> Task<Result<MailFetched>>;
}

/// The id that travels in a captured line's marker: the first 12 hex
/// characters of `sha256(account + "\0" + thread_id)` — same construction and
/// rationale as V8's `event_marker_id` (spec §5.1).
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
/// multi-account browsers open the right mailbox (spec §5.1).
pub fn gmail_thread_url(account: &str, thread_id: &str) -> String {
    format!("https://mail.google.com/mail/u/{account}/#all/{thread_id}")
}

/// A ready-to-write archive file (full mode), vault-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchiveFile {
    pub rel_path: String,
    /// The filename without directory or `.md` — the `[[wikilink]]` target.
    pub stem: String,
    pub digest: String,
    pub content: String,
}

/// What to remember about a capture once it is applied (spec §4.3): appended
/// to `.thock/state/gmail/imported.jsonl` only after the backlog write lands.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRecord {
    pub digest: String,
    pub thread_id: String,
    pub subject: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct CapturePlan {
    /// Written first, create-if-missing; a crash after this leaves harmless
    /// orphans (spec §9).
    pub archives: Vec<ArchiveFile>,
    /// One `append_to_section_edit` into Someday covering every new task.
    pub backlog_edit: Option<Edit>,
    pub newly_imported: Vec<ImportRecord>,
}

impl CapturePlan {
    pub fn is_empty(&self) -> bool {
        self.archives.is_empty() && self.backlog_edit.is_none() && self.newly_imported.is_empty()
    }
}

/// The capture planner (spec §8) — pure, no I/O. `imported` is the loaded
/// dedup state; `taken_stems` maps archive filename stems already on disk to
/// their thread digests, for collision handling (§5.3). `captured_at` is an
/// RFC 3339 timestamp stamped by the caller (this function must stay
/// deterministic).
pub fn plan_capture(
    backlog_text: &str,
    emails: &[CapturedEmail],
    imported: &HashSet<String>,
    taken_stems: &HashMap<String, String>,
    config: &GmailConfig,
    captured_at: &str,
) -> CapturePlan {
    let account = config.account.as_deref().unwrap_or("");
    let markers_in_backlog = scan_backlog_markers(backlog_text);

    let mut emails: Vec<&CapturedEmail> = emails.iter().collect();
    emails.sort_by_key(|email| email.date);

    let mut plan = CapturePlan::default();
    let mut planned: HashSet<String> = HashSet::new();
    let mut claimed_stems: HashMap<String, String> = taken_stems.clone();
    let mut lines = String::new();

    for email in emails {
        let digest = thread_marker_id(account, &email.thread_id);
        if imported.contains(&digest) || !planned.insert(digest.clone()) {
            continue;
        }
        let record = ImportRecord {
            digest: digest.clone(),
            thread_id: email.thread_id.clone(),
            subject: sanitize_subject(&email.subject),
        };
        // Second guard (spec §5.2): a marker already in the backlog means the
        // state lost this thread — repair the state, never duplicate the line.
        if markers_in_backlog.contains(&digest) {
            plan.newly_imported.push(record);
            continue;
        }
        let subject = record.subject.clone();
        let line = match config.import {
            ImportMode::Title => format!(
                "- [ ] [{}]({}) {MARKER_PREFIX}{digest}{MARKER_SUFFIX}",
                escape_link_text(&subject),
                gmail_thread_url(account, &email.thread_id),
            ),
            ImportMode::Full => {
                let stem = claim_stem(&mut claimed_stems, email, &subject, &digest);
                // An existing file for the same thread (state was rebuilt
                // mid-flight) is left untouched — create-if-missing (§5.3).
                if !taken_stems.get(&stem).is_some_and(|owner| *owner == digest) {
                    plan.archives.push(ArchiveFile {
                        rel_path: format!("{}/{stem}.md", config.archive_dir),
                        stem: stem.clone(),
                        digest: digest.clone(),
                        content: render_archive(email, &subject, &digest, account, captured_at),
                    });
                }
                format!(
                    "- [ ] {} [[{stem}]] {MARKER_PREFIX}{digest}{MARKER_SUFFIX}",
                    break_wikilinks(&subject),
                )
            }
        };
        lines.push_str(&line);
        lines.push('\n');
        plan.newly_imported.push(record);
    }

    if !lines.is_empty() {
        plan.backlog_edit = Some(append_to_section_edit(
            backlog_text,
            SectionKind::Someday,
            &lines,
        ));
    }
    plan
}

/// Claims a unique archive filename stem: `<date>-<slug>`, extended with a
/// digest prefix (then the whole digest) when another thread already owns it.
fn claim_stem(
    claimed: &mut HashMap<String, String>,
    email: &CapturedEmail,
    subject: &str,
    digest: &str,
) -> String {
    let base = format!("{}-{}", email.date.format("%Y-%m-%d"), slug(subject, "email"));
    let short = digest.get(..4).unwrap_or(digest);
    for candidate in [
        base.clone(),
        format!("{base}-{short}"),
        format!("{base}-{digest}"),
    ] {
        match claimed.get(&candidate) {
            Some(owner) if *owner != digest => continue,
            _ => {
                claimed.insert(candidate.clone(), digest.to_string());
                return candidate;
            }
        }
    }
    // Unreachable in practice: the full digest is unique per thread.
    format!("{base}-{digest}")
}

fn render_archive(
    email: &CapturedEmail,
    subject: &str,
    digest: &str,
    account: &str,
    captured_at: &str,
) -> String {
    let body = email
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        // Headers still get archived when no text part was readable — a
        // capture is never dropped silently (spec §8.1).
        .unwrap_or("_(no text content)_");
    format!(
        "---\n\
         subject: {subject}\n\
         from: {}\n\
         date: {}\n\
         gmail: {}\n\
         thread: {digest}\n\
         captured: {captured_at}\n\
         ---\n\
         \n\
         # {subject}\n\
         \n\
         {body}\n",
        collapse_whitespace(&email.from),
        email.date.to_rfc3339(),
        gmail_thread_url(account, &email.thread_id),
    )
}

/// Every `<!--gmail:…-->` marker digest in the text, all sections included —
/// the vault-side half of the dedup contract (spec §4.3, §5.2).
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

/// The `thread:` digest from an archive file's frontmatter, for rebuilding
/// state and the stem→digest collision map from the vault (spec §4.3).
pub fn archive_frontmatter_digest(content: &str) -> Option<String> {
    let rest = content.strip_prefix("---\n")?;
    let (frontmatter, _) = rest.split_once("\n---")?;
    frontmatter.lines().find_map(|line| {
        let value = line.strip_prefix("thread:")?.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Subject → task text (spec §5.1 rules 4–5): RFC 2047 decoding already
/// happened in the provider; here reply prefixes are stripped, marker forgery
/// is removed, and whitespace (newlines included) collapses to single spaces.
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

/// Escapes `[` and `]` so a subject cannot break or forge the Markdown link
/// it becomes the text of (title mode).
fn escape_link_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]")
}

/// Breaks apart `[[` / `]]` so a subject cannot forge a wikilink (full mode).
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
/// nothing survives (spec §5.3). Shared with the inbox planner (V13 §6).
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
    use crate::backlog::{DEFAULT_BACKLOG, apply_edits, parse_backlog};
    use chrono::TimeZone as _;

    fn email(thread_id: &str, subject: &str, day: u32, body: Option<&str>) -> CapturedEmail {
        CapturedEmail {
            thread_id: thread_id.to_string(),
            subject: subject.to_string(),
            from: "Acme Billing <billing@acme.com>".to_string(),
            date: Local.with_ymd_and_hms(2026, 8, day, 9, 30, 0).unwrap(),
            body: body.map(str::to_string),
        }
    }

    fn config(import: ImportMode) -> GmailConfig {
        GmailConfig {
            account: Some("diego@example.com".to_string()),
            import,
            ..GmailConfig::default()
        }
    }

    fn apply(text: &str, plan: &CapturePlan) -> String {
        match &plan.backlog_edit {
            Some(edit) => apply_edits(text, vec![edit.clone()]),
            None => text.to_string(),
        }
    }

    #[test]
    fn config_defaults_and_overrides() {
        let config = parse_gmail_config("").unwrap();
        assert_eq!(config, GmailConfig::default());
        assert_eq!(config.label, "backlog");
        assert_eq!(config.import, ImportMode::Title);
        assert_eq!(config.archive_dir, "archives/emails");
        assert_eq!(config.poll_interval, Duration::from_secs(300));

        let config = parse_gmail_config(
            "schema = 1\naccount = \"d@e.com\"\nlabel = \"To Backlog\"\nimport = \"FULL\"\n\
             archive_dir = \"/mail/archive/\"\npoll_seconds = 10\n\
             [google]\nclient_id = \"id\"\nunknown = 1\n",
        )
        .unwrap();
        assert_eq!(config.account.as_deref(), Some("d@e.com"));
        assert_eq!(config.label, "To Backlog");
        assert_eq!(config.import, ImportMode::Full);
        assert_eq!(config.archive_dir, "mail/archive");
        // Clamped to the floor.
        assert_eq!(config.poll_interval, Duration::from_secs(60));
        assert_eq!(config.google.client_id.as_deref(), Some("id"));

        assert!(parse_gmail_config("import = \"everything\"").is_err());
        assert!(parse_gmail_config("account = }").is_err());
    }

    #[test]
    fn subject_sanitization() {
        assert_eq!(sanitize_subject("  Invoice\n #4821   due Friday "), "Invoice #4821 due Friday");
        assert_eq!(sanitize_subject("Re: RE: Fwd:fw: The point"), "The point");
        assert_eq!(sanitize_subject("Sneaky <!--gmail:beef--> subject"), "Sneaky gmail:beef--> subject");
        assert_eq!(sanitize_subject(""), "(no subject)");
        assert_eq!(sanitize_subject("Re: "), "(no subject)");
        // "Real" uses of re: mid-subject survive.
        assert_eq!(sanitize_subject("More re: everything"), "More re: everything");
    }

    #[test]
    fn slugs_are_bounded_and_safe() {
        assert_eq!(slug("Invoice #4821 due Friday!", "email"), "invoice-4821-due-friday");
        assert_eq!(slug("¡Órale! ünïcode", "email"), "rale-n-code");
        assert_eq!(slug("!!!", "email"), "email");
        assert!(slug(&"long word ".repeat(50), "email").len() <= 60);
        assert!(!slug(&"a ".repeat(60), "email").ends_with('-'));
    }

    #[test]
    fn title_mode_appends_linked_tasks_to_someday() {
        let emails = vec![
            email("thread-b", "Second", 19, None),
            email("thread-a", "Re: [urgent] First", 18, None),
        ];
        let plan = plan_capture(
            DEFAULT_BACKLOG,
            &emails,
            &HashSet::new(),
            &HashMap::new(),
            &config(ImportMode::Title),
            "2026-08-19T08:00:00-07:00",
        );
        assert!(plan.archives.is_empty());
        assert_eq!(plan.newly_imported.len(), 2);

        let edited = apply(DEFAULT_BACKLOG, &plan);
        let backlog = parse_backlog(&edited);
        // Oldest first, appended to Someday, subjects escaped and linked.
        assert_eq!(backlog.someday.len(), 2);
        assert!(backlog.someday[0].text.starts_with("[\\[urgent\\] First]("));
        assert!(
            backlog.someday[0]
                .text
                .contains("https://mail.google.com/mail/u/diego@example.com/#all/thread-a"),
            "{}",
            backlog.someday[0].text
        );
        let digest = thread_marker_id("diego@example.com", "thread-a");
        assert!(backlog.someday[0].text.ends_with(&format!("<!--gmail:{digest}-->")));
        assert!(backlog.someday[1].text.starts_with("[Second]("));
        // Soon and Completed untouched.
        assert!(backlog.soon.is_empty());
        assert!(edited.contains("<!-- Soon = tasks for the coming days."));
    }

    #[test]
    fn full_mode_archives_and_wikilinks() {
        let emails = vec![email("thread-a", "Invoice #4821 due Friday", 18, Some("Pay up.\n"))];
        let plan = plan_capture(
            DEFAULT_BACKLOG,
            &emails,
            &HashSet::new(),
            &HashMap::new(),
            &config(ImportMode::Full),
            "2026-08-19T08:00:00-07:00",
        );
        let digest = thread_marker_id("diego@example.com", "thread-a");
        assert_eq!(plan.archives.len(), 1);
        let archive = &plan.archives[0];
        assert_eq!(archive.stem, "2026-08-18-invoice-4821-due-friday");
        assert_eq!(archive.rel_path, "archives/emails/2026-08-18-invoice-4821-due-friday.md");
        assert!(archive.content.starts_with("---\nsubject: Invoice #4821 due Friday\n"));
        for needle in [
            "from: Acme Billing <billing@acme.com>",
            &format!("thread: {digest}"),
            "captured: 2026-08-19T08:00:00-07:00",
            "gmail: https://mail.google.com/mail/u/diego@example.com/#all/thread-a",
            "# Invoice #4821 due Friday\n\nPay up.\n",
        ] {
            assert!(archive.content.contains(needle), "missing {needle} in {}", archive.content);
        }
        assert_eq!(archive_frontmatter_digest(&archive.content).as_deref(), Some(digest.as_str()));

        let edited = apply(DEFAULT_BACKLOG, &plan);
        let backlog = parse_backlog(&edited);
        assert_eq!(
            backlog.someday[0].text,
            format!(
                "Invoice #4821 due Friday [[2026-08-18-invoice-4821-due-friday]] <!--gmail:{digest}-->"
            )
        );
    }

    #[test]
    fn full_mode_without_body_still_captures() {
        let emails = vec![email("thread-a", "Silent", 18, None)];
        let plan = plan_capture(
            DEFAULT_BACKLOG,
            &emails,
            &HashSet::new(),
            &HashMap::new(),
            &config(ImportMode::Full),
            "t",
        );
        assert!(plan.archives[0].content.contains("_(no text content)_"));
        assert!(plan.backlog_edit.is_some());
    }

    #[test]
    fn wikilink_forgery_is_broken_apart() {
        let emails = vec![email("thread-a", "Click [[evil]] now", 18, None)];
        let plan = plan_capture(
            DEFAULT_BACKLOG,
            &emails,
            &HashSet::new(),
            &HashMap::new(),
            &config(ImportMode::Full),
            "t",
        );
        let edited = apply(DEFAULT_BACKLOG, &plan);
        let backlog = parse_backlog(&edited);
        assert!(backlog.someday[0].text.starts_with("Click [ [evil] ] now [["));
    }

    #[test]
    fn stem_collisions_get_digest_suffixes() {
        let emails = vec![
            email("thread-a", "Same subject", 18, Some("a")),
            email("thread-b", "Same subject", 18, Some("b")),
        ];
        let mut taken = HashMap::new();
        taken.insert("2026-08-18-same-subject".to_string(), "someoneelse00".to_string());
        let plan = plan_capture(
            DEFAULT_BACKLOG,
            &emails,
            &HashSet::new(),
            &taken,
            &config(ImportMode::Full),
            "t",
        );
        assert_eq!(plan.archives.len(), 2);
        let digest_a = thread_marker_id("diego@example.com", "thread-a");
        let digest_b = thread_marker_id("diego@example.com", "thread-b");
        // Both dodge the on-disk stem, and each other.
        assert_eq!(plan.archives[0].stem, format!("2026-08-18-same-subject-{}", &digest_a[..4]));
        assert_eq!(plan.archives[1].stem, format!("2026-08-18-same-subject-{}", &digest_b[..4]));
    }

    #[test]
    fn existing_archive_for_same_thread_is_not_rewritten() {
        let emails = vec![email("thread-a", "Same subject", 18, Some("a"))];
        let digest = thread_marker_id("diego@example.com", "thread-a");
        let mut taken = HashMap::new();
        taken.insert("2026-08-18-same-subject".to_string(), digest);
        let plan = plan_capture(
            DEFAULT_BACKLOG,
            &emails,
            &HashSet::new(),
            &taken,
            &config(ImportMode::Full),
            "t",
        );
        // The task line still lands (the state lost it) but the file stays.
        assert!(plan.archives.is_empty());
        assert!(plan.backlog_edit.is_some());
        let edited = apply(DEFAULT_BACKLOG, &plan);
        assert!(edited.contains("[[2026-08-18-same-subject]]"));
    }

    #[test]
    fn imported_state_and_markers_both_dedup() {
        let emails = vec![email("thread-a", "Once", 18, None), email("thread-b", "Twice", 18, None)];
        let config = config(ImportMode::Title);
        let digest_a = thread_marker_id("diego@example.com", "thread-a");
        let digest_b = thread_marker_id("diego@example.com", "thread-b");

        // thread-a is in the state; thread-b's marker survives in Completed
        // (the state lost it).
        let backlog_text = format!(
            "## Soon\n\n## Someday\n\n## Completed\n\n- [x] old [t](u) <!--gmail:{digest_b}--> ✅ 2026-08-10\n"
        );
        let imported: HashSet<String> = [digest_a].into_iter().collect();
        let plan = plan_capture(&backlog_text, &emails, &imported, &HashMap::new(), &config, "t");
        // Nothing to write, but thread-b's state entry gets repaired.
        assert!(plan.backlog_edit.is_none());
        assert_eq!(plan.newly_imported.len(), 1);
        assert_eq!(plan.newly_imported[0].digest, digest_b);
    }

    #[test]
    fn plan_is_idempotent_with_and_without_state() {
        for mode in [ImportMode::Title, ImportMode::Full] {
            let config = config(mode);
            let emails = vec![
                email("thread-a", "First", 18, Some("body a")),
                email("thread-b", "Re: First", 19, Some("body b")),
            ];
            let plan = plan_capture(
                DEFAULT_BACKLOG,
                &emails,
                &HashSet::new(),
                &HashMap::new(),
                &config,
                "t",
            );
            assert!(!plan.is_empty());
            let applied = apply(DEFAULT_BACKLOG, &plan);
            let stems: HashMap<String, String> = plan
                .archives
                .iter()
                .map(|archive| (archive.stem.clone(), archive.digest.clone()))
                .collect();
            let imported: HashSet<String> = plan
                .newly_imported
                .iter()
                .map(|record| record.digest.clone())
                .collect();

            // With the state updated (the normal next poll).
            let replan = plan_capture(&applied, &emails, &imported, &stems, &config, "t2");
            assert!(replan.is_empty(), "{mode:?}: {replan:?}");

            // With the state lost (crash between backlog write and state
            // write): the markers repair the state without duplicating.
            let replan =
                plan_capture(&applied, &emails, &HashSet::new(), &stems, &config, "t2");
            assert!(replan.backlog_edit.is_none(), "{mode:?}: {replan:?}");
            assert!(replan.archives.is_empty());
            assert_eq!(replan.newly_imported.len(), 2);
        }
    }

    #[test]
    fn markers_scan_all_sections_and_ignore_junk() {
        let text = "## Soon\n- [ ] a <!--gmail:aaaa-->\nprose <!--gmail: bbbb -->\n\
                    <!--gmail:-->\n<!--gcal:cccc-->\n## Completed\n- [x] d <!--gmail:dddd--> ✅ 2026-01-01\n";
        let markers = scan_backlog_markers(text);
        assert_eq!(
            markers,
            ["aaaa", "bbbb", "dddd"].into_iter().map(str::to_string).collect()
        );
    }

    #[test]
    fn frontmatter_digest_requires_frontmatter() {
        assert_eq!(archive_frontmatter_digest("# No frontmatter\nthread: nope\n"), None);
        assert_eq!(
            archive_frontmatter_digest("---\nsubject: s\nthread: abc123\n---\n\nbody\n").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn missing_someday_heading_is_created() {
        let plan = plan_capture(
            "# Backlog\n\nprose only\n",
            &[email("thread-a", "New", 18, None)],
            &HashSet::new(),
            &HashMap::new(),
            &config(ImportMode::Title),
            "t",
        );
        let edited = apply("# Backlog\n\nprose only\n", &plan);
        let backlog = parse_backlog(&edited);
        assert_eq!(backlog.someday.len(), 1);
        assert!(edited.contains("prose only"));
    }
}
