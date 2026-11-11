//! Unified Gmail sync (spec `v15-unified-gmail-sync.md`): the `.thock/gmail.toml`
//! label → folder map, the transport trait, marker scanning, and the digest
//! bridge from V9. Everything here is pure string-in/string-out (no network,
//! no I/O); the GPUI service lives in `gmail_service.rs`, the note planner in
//! `inbox.rs`, and the backlog landing integration in `backlog.rs`.

use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use chrono::{DateTime, Local, Utc};
use gpui::{AsyncApp, Task};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Duration;
use thock_sync_core::{DecisionClass, INBOX_ARCHIVE_DIR, decision_class, parse_triage_log_line};

use crate::calendar::GoogleClientOverride;
use crate::inbox::{CapturedItem, parse_inbox_note};
use crate::inbox_email::is_message_heading;

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

    /// The captured threads that changed since they were last looked at
    /// (V56 §6.2), fetching at most `budget` of them. A thread comes back
    /// even with nothing new, so its `historyId` is recorded and it isn't
    /// fetched again. The default is a transport that can't tell: nothing
    /// changed.
    fn refresh(
        &self,
        _known: &HashMap<String, KnownThread>,
        _budget: usize,
        _cx: &AsyncApp,
    ) -> Task<Result<Vec<ThreadUpdate>>> {
        Task::ready(Ok(Vec::new()))
    }

    /// A thread's messages with the headers a reply is addressed from
    /// (V56 §7.2), oldest first, drafts left out.
    fn thread_headers(&self, _thread_id: &str, _cx: &AsyncApp) -> Task<Result<ThreadHeaders>> {
        Task::ready(Err(anyhow!("this mail connection can't read a thread")))
    }

    /// Sends a reply into its thread (V56 §7.4). Fails with
    /// [`MissingPermission`] when the grant lacks `gmail.modify`, without
    /// sending anything when the grant is known.
    fn send_reply(&self, _reply: &OutgoingReply, _cx: &AsyncApp) -> Task<Result<()>> {
        Task::ready(Err(anyhow!("this mail connection can't send")))
    }
}

/// How many changed threads one poll fetches (V56 §6.2), so the first poll
/// after an upgrade doesn't fetch a whole mailbox.
pub const REFRESH_BUDGET: usize = 10;

/// What Thock knows of a captured thread before asking Gmail (V56 §6.3).
#[derive(Debug, Clone, PartialEq)]
pub struct KnownThread {
    /// Gmail's `historyId` when the thread was last fetched, if it ever was.
    pub history: Option<String>,
    /// The newest message the note holds, in milliseconds since the epoch.
    pub through: i64,
}

/// One message of a thread, as a refresh hands it over.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMessage {
    pub from: String,
    pub sent: Option<DateTime<Utc>>,
    pub text: Option<String>,
    /// Sent from the connected account.
    pub own: bool,
}

impl ThreadMessage {
    /// The message as a section of its note.
    pub fn section(&self) -> String {
        let sent = self.sent.map(|sent| sent.with_timezone(&Local));
        message_section(&self.from, sent, self.text.as_deref())
    }

    pub fn heading(&self) -> String {
        let sent = self.sent.map(|sent| sent.with_timezone(&Local));
        section_heading(&self.from, sent)
    }
}

/// A captured thread as Gmail has it now.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadUpdate {
    pub thread: String,
    pub history: String,
    /// The newest message of the thread, in milliseconds since the epoch.
    pub through: i64,
    /// The thread's first message, for the heading a lone message never got.
    pub first: Option<ThreadMessage>,
    /// Messages newer than the note holds, oldest first, drafts left out.
    pub new_messages: Vec<ThreadMessage>,
}

/// `## <from> — <YYYY-MM-DD HH:MM>`: what heads a message in a note, and what
/// tells the reader where one starts.
pub fn section_heading(from: &str, sent: Option<DateTime<Local>>) -> String {
    let from = collapse_whitespace(from);
    let from = if from.is_empty() {
        "(unknown sender)"
    } else {
        from.as_str()
    };
    match sent {
        Some(sent) => format!("## {from} — {}", sent.format("%Y-%m-%d %H:%M")),
        None => format!("## {from}"),
    }
}

/// One message as capture writes it: the heading, then its text.
pub fn message_section(from: &str, sent: Option<DateTime<Local>>, text: Option<&str>) -> String {
    let mut section = section_heading(from, sent);
    section.push('\n');
    match text.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => {
            section.push('\n');
            section.push_str(text);
            section.push('\n');
        }
        None => section.push_str("\n_(no content)_\n"),
    }
    section
}

/// The last record of `.thock/state/gmail/threads.jsonl` for each thread.
pub fn parse_thread_marks(text: &str) -> HashMap<String, KnownThread> {
    let mut marks = HashMap::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let thread = record.get("thread").and_then(|value| value.as_str());
        let history = record.get("history").and_then(|value| value.as_str());
        let through = record.get("through").and_then(|value| value.as_i64());
        if let (Some(thread), Some(history), Some(through)) = (thread, history, through) {
            marks.insert(
                thread.to_string(),
                KnownThread {
                    history: Some(history.to_string()),
                    through,
                },
            );
        }
    }
    marks
}

/// One line of `threads.jsonl`.
pub fn thread_mark_line(update: &ThreadUpdate, at: &str) -> String {
    serde_json::json!({
        "thread": update.thread,
        "history": update.history,
        "through": update.through,
        "at": at,
    })
    .to_string()
}

/// Every captured thread a refresh may ask about: one with a mark, or one
/// whose capture is dated, which stands in for a mark until the first fetch.
pub fn known_threads(
    imported: &ImportedIndex,
    marks: &HashMap<String, KnownThread>,
) -> HashMap<String, KnownThread> {
    imported
        .values()
        .filter_map(|imported| {
            let known = marks.get(&imported.thread).cloned().or_else(|| {
                Some(KnownThread {
                    history: None,
                    through: imported.captured_at?.timestamp_millis(),
                })
            })?;
            Some((imported.thread.clone(), known))
        })
        .collect()
}

/// One message of a thread, with what a reply to it needs (V56 §7.2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageHeaders {
    pub from: String,
    pub reply_to: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub message_id: Option<String>,
    pub references: Option<String>,
    pub sent: Option<DateTime<Utc>>,
    pub text: Option<String>,
    /// Sent from the connected account.
    pub own: bool,
}

/// A thread as a reply sees it: oldest first, drafts left out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadHeaders {
    pub thread: String,
    pub messages: Vec<MessageHeaders>,
}

/// Who a reply goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyMode {
    /// Whoever wrote the message.
    Author,
    /// Whoever wrote it, and everyone it was sent to.
    All,
    /// One address of the thread, and nobody else.
    One(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipients {
    pub to: Vec<String>,
    pub cc: Vec<String>,
}

/// A reply ready to send. [`plan_reply`] is what builds one, so its
/// recipients are always addresses the thread already named.
#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingReply {
    pub thread: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub body: String,
}

/// The thread id a captured note's `link:` points at (see
/// [`gmail_thread_url`]).
pub fn gmail_thread_id(link: &str) -> Option<&str> {
    let (_, id) = link
        .strip_prefix("https://mail.google.com/mail/")?
        .rsplit_once("/#all/")?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())).then_some(id)
}

/// The bare address of a mailbox (`Ana <ana@example.com>` or
/// `ana@example.com`), lowercased. Anything that isn't plainly one address is
/// refused, so a header can't smuggle a second recipient or a line break.
pub fn mailbox_address(mailbox: &str) -> Option<String> {
    let mailbox = mailbox.trim();
    let address = match mailbox.rsplit_once('<') {
        Some((_, rest)) => rest.strip_suffix('>')?,
        None => mailbox,
    }
    .trim();
    let (local, domain) = address.split_once('@')?;
    let plain = |part: &str| {
        !part.is_empty()
            && part.chars().all(|c| {
                c.is_ascii_graphic()
                    && !matches!(
                        c,
                        '<' | '>' | ',' | ';' | ':' | '"' | '(' | ')' | '[' | ']' | '\\' | '@'
                    )
            })
    };
    (plain(local) && plain(domain) && domain.contains('.')).then(|| address.to_ascii_lowercase())
}

/// The mailboxes of a `To` or `Cc` header: split on the commas between
/// them, not the ones inside a quoted name.
pub fn split_mailboxes(header: &str) -> Vec<String> {
    let mut mailboxes = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut angled) = (false, false);
    for c in header.chars() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angled = true,
            '>' if !quoted => angled = false,
            ',' if !quoted && !angled => {
                mailboxes.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    mailboxes.push(current);
    mailboxes
        .into_iter()
        .map(|mailbox| mailbox.trim().to_string())
        .filter(|mailbox| !mailbox.is_empty())
        .collect()
}

fn addresses<'a>(mailboxes: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut seen = Vec::new();
    for address in mailboxes
        .into_iter()
        .filter_map(|mailbox| mailbox_address(mailbox))
    {
        if !seen.contains(&address) {
            seen.push(address);
        }
    }
    seen
}

/// Every address the thread's headers name, in order of appearance, the
/// account's own left out: who a reply may go to, and nobody else.
pub fn thread_addresses(thread: &ThreadHeaders, account: &str) -> Vec<String> {
    let account = account.to_ascii_lowercase();
    let mailboxes = thread.messages.iter().flat_map(|message| {
        std::iter::once(&message.from)
            .chain(message.reply_to.iter())
            .chain(&message.to)
            .chain(&message.cc)
    });
    addresses(mailboxes)
        .into_iter()
        .filter(|address| *address != account)
        .collect()
}

/// Who a reply to `message` goes to (V56 §7.2). A reply to your own message
/// goes to whoever that one went to.
pub fn reply_recipients(message: &MessageHeaders, mode: &ReplyMode, account: &str) -> Recipients {
    let account = account.to_ascii_lowercase();
    let others = |mailboxes: Vec<String>| -> Vec<String> {
        mailboxes
            .into_iter()
            .filter(|address| *address != account)
            .collect()
    };
    let author = others(addresses(
        message.reply_to.iter().chain([&message.from]).take(1),
    ));
    let sent_to = others(addresses(&message.to));
    let to = if author.is_empty() { sent_to } else { author };
    match mode {
        ReplyMode::One(address) => Recipients {
            to: vec![address.to_ascii_lowercase()],
            cc: Vec::new(),
        },
        ReplyMode::Author => Recipients { to, cc: Vec::new() },
        ReplyMode::All => {
            let cc = others(addresses(message.to.iter().chain(&message.cc)))
                .into_iter()
                .filter(|address| !to.contains(address))
                .collect();
            Recipients { to, cc }
        }
    }
}

/// Which of the thread's messages a card of the note is: by position while
/// the note and the thread agree on how many there are, else by sender and
/// minute, else the newest.
pub fn answered_message(
    thread: &ThreadHeaders,
    card: usize,
    cards: usize,
    card_from: &str,
    card_sent: Option<chrono::NaiveDateTime>,
) -> Option<usize> {
    let last = thread.messages.len().checked_sub(1)?;
    if cards == thread.messages.len() {
        return Some(card.min(last));
    }
    let minute = |sent: DateTime<Utc>| {
        sent.with_timezone(&Local)
            .naive_local()
            .format("%Y-%m-%d %H:%M")
            .to_string()
    };
    let card_minute = card_sent.map(|sent| sent.format("%Y-%m-%d %H:%M").to_string());
    let card_address = mailbox_address(card_from);
    thread
        .messages
        .iter()
        .position(|message| {
            card_minute.is_some()
                && message.sent.map(minute) == card_minute
                && mailbox_address(&message.from) == card_address
        })
        .or(Some(last))
}

/// The thread's subject with one `Re: `.
pub fn reply_subject(subject: &str) -> String {
    let subject = collapse_whitespace(subject);
    let already = subject
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("re:"));
    if already {
        subject
    } else {
        format!("Re: {subject}")
    }
}

/// The reply's text, then the answered message quoted under it.
pub fn reply_body(text: &str, answered: &MessageHeaders) -> String {
    let mut body = text.trim_end().to_string();
    body.push('\n');
    let Some(quoted) = answered
        .text
        .as_deref()
        .map(str::trim)
        .filter(|quoted| !quoted.is_empty())
    else {
        return body;
    };
    let from = collapse_whitespace(&answered.from);
    let when = answered.sent.map(|sent| {
        sent.with_timezone(&Local)
            .format("%a, %b %-d, %Y at %H:%M")
            .to_string()
    });
    body.push('\n');
    match when {
        Some(when) => body.push_str(&format!("On {when}, {from} wrote:\n")),
        None => body.push_str(&format!("{from} wrote:\n")),
    }
    for line in quoted.lines() {
        if line.is_empty() {
            body.push_str(">\n");
        } else {
            body.push_str(&format!("> {line}\n"));
        }
    }
    body
}

/// Builds the reply to `thread.messages[answered]`. It refuses empty text, a
/// reply with nobody to go to, and any recipient the thread doesn't name.
pub fn plan_reply(
    thread: &ThreadHeaders,
    answered: usize,
    mode: &ReplyMode,
    account: &str,
    text: &str,
) -> Result<OutgoingReply> {
    let Some(message) = thread.messages.get(answered) else {
        bail!("that message is no longer in the thread");
    };
    if text.trim().is_empty() {
        bail!("there is nothing to send yet");
    }
    let Some(from) = mailbox_address(account) else {
        bail!("Thock doesn't know which Gmail account to send from");
    };
    let recipients = reply_recipients(message, mode, account);
    let allowed = thread_addresses(thread, account);
    if let Some(stranger) = recipients
        .to
        .iter()
        .chain(&recipients.cc)
        .find(|address| !allowed.contains(address))
    {
        bail!("{stranger} isn't on this thread");
    }
    if recipients.to.is_empty() {
        bail!("this message has nobody to reply to");
    }
    let references = match (&message.references, &message.message_id) {
        (Some(references), Some(id)) => Some(format!("{references} {id}")),
        (None, Some(id)) => Some(id.clone()),
        (references, None) => references.clone(),
    };
    Ok(OutgoingReply {
        thread: thread.thread.clone(),
        from,
        to: recipients.to,
        cc: recipients.cc,
        subject: reply_subject(&message.subject),
        in_reply_to: message.message_id.clone(),
        references,
        body: reply_body(text, message),
    })
}

/// A header value on one line, whatever the email it was read from held.
fn header_line(value: &str) -> String {
    collapse_whitespace(&value.replace(['\r', '\n'], " "))
}

/// An unstructured header's text, RFC 2047 encoded when it isn't ASCII.
fn encoded_word(value: &str) -> String {
    let value = header_line(value);
    if value.is_ascii() {
        return value;
    }
    // An encoded word holds at most 75 characters; 30 bytes of UTF-8, cut
    // between characters, keeps each well under.
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in value.chars() {
        if chunk.len() + c.len_utf8() > 30 {
            words.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    words.push(chunk);
    words
        .iter()
        .map(|word| {
            format!(
                "=?UTF-8?B?{}?=",
                base64::engine::general_purpose::STANDARD.encode(word)
            )
        })
        .collect::<Vec<_>>()
        .join("\r\n ")
}

/// The reply as an RFC 5322 message: plain UTF-8 text, base64 encoded.
pub fn reply_mime(reply: &OutgoingReply) -> String {
    let mut mime = String::new();
    let mut header = |name: &str, value: &str| {
        mime.push_str(name);
        mime.push_str(": ");
        mime.push_str(value);
        mime.push_str("\r\n");
    };
    header("From", &header_line(&reply.from));
    header("To", &header_line(&reply.to.join(", ")));
    if !reply.cc.is_empty() {
        header("Cc", &header_line(&reply.cc.join(", ")));
    }
    header("Subject", &encoded_word(&reply.subject));
    if let Some(id) = &reply.in_reply_to {
        header("In-Reply-To", &header_line(id));
    }
    if let Some(references) = &reply.references {
        header("References", &header_line(references));
    }
    header("MIME-Version", "1.0");
    header("Content-Type", "text/plain; charset=\"UTF-8\"");
    header("Content-Transfer-Encoding", "base64");
    mime.push_str("\r\n");
    let encoded = base64::engine::general_purpose::STANDARD.encode(reply.body.as_bytes());
    for line in encoded.as_bytes().chunks(76) {
        mime.push_str(&String::from_utf8_lossy(line));
        mime.push_str("\r\n");
    }
    mime
}

/// What a refresh adds to a note (V56 §6.4): the first message's heading
/// where capture wrote a lone message bare, then the new sections at the end.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteRefresh {
    /// Where the first message's heading goes, and its text.
    pub heading: Option<(usize, String)>,
    pub appended: String,
}

impl NoteRefresh {
    pub fn apply(&self, content: &str) -> String {
        let mut out = String::with_capacity(content.len() + self.appended.len());
        match &self.heading {
            Some((offset, heading)) => {
                let offset = (*offset).min(content.len());
                out.push_str(&content[..offset]);
                out.push_str(heading);
                out.push_str(&content[offset..]);
            }
            None => out.push_str(content),
        }
        out.push_str(&self.appended);
        out
    }
}

/// Plans the append of `sections` to a note. A section the note already
/// holds, byte for byte, is left out, so a poll repeated after a crash adds
/// nothing; `None` when that leaves nothing to add.
pub fn plan_note_refresh(
    content: &str,
    first_heading: Option<&str>,
    sections: &[String],
) -> Option<NoteRefresh> {
    let missing: Vec<&String> = sections
        .iter()
        .filter(|section| !content.contains(section.trim_end()))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let heading = first_heading
        .filter(|_| !content.lines().any(is_message_heading))
        .map(|heading| (first_message_offset(content), format!("{heading}\n\n")));
    let mut appended = String::new();
    if !content.ends_with('\n') {
        appended.push('\n');
    }
    for section in missing {
        appended.push('\n');
        appended.push_str(section);
    }
    Some(NoteRefresh { heading, appended })
}

/// Where a note's first message starts: past the frontmatter and the title.
fn first_message_offset(content: &str) -> usize {
    let item = parse_inbox_note("", content);
    let body = item.body.as_str();
    let mut offset = content.len() - body.len();
    let mut past_title = false;
    for line in body.split_inclusive('\n') {
        if line.trim().is_empty() {
            offset += line.len();
        } else if !past_title && line.starts_with("# ") {
            past_title = true;
            offset += line.len();
        } else {
            break;
        }
    }
    offset
}

/// Whether a reply brings a triaged note back to the inbox (V56 §6.5): it
/// came from someone else, the note sits where triage put it, and the last
/// decision wasn't to archive or discard it.
pub fn returns_to_inbox(
    note_dir: &str,
    last_decision: Option<DecisionClass>,
    from_someone_else: bool,
) -> bool {
    from_someone_else
        && note_dir.trim_matches('/') == INBOX_ARCHIVE_DIR
        && matches!(
            last_decision,
            Some(
                DecisionClass::Today
                    | DecisionClass::Soon
                    | DecisionClass::Someday
                    | DecisionClass::Other
            )
        )
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
    /// When it was captured; absent in records written before V15.
    pub captured_at: Option<DateTime<Utc>>,
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
                    captured_at: field("at")
                        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                        .map(|at| at.with_timezone(&Utc)),
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

    #[test]
    fn a_lone_message_gets_its_heading_before_the_first_reply() {
        let note = "---\nsource:   gmail\ncapture:  abc\n---\n\n# Subject\n\nHello there.\n";
        let reply = message_section("Bea <bea@example.com>", None, Some("Hi back."));
        let refresh = plan_note_refresh(
            note,
            Some("## Ana — 2026-08-18 09:30"),
            std::slice::from_ref(&reply),
        )
        .unwrap();
        assert_eq!(
            refresh.apply(note),
            "---\nsource:   gmail\ncapture:  abc\n---\n\n# Subject\n\n\
             ## Ana — 2026-08-18 09:30\n\nHello there.\n\n\
             ## Bea <bea@example.com>\n\nHi back.\n"
        );
        // Applied, the same plan has nothing left to add.
        let applied = refresh.apply(note);
        assert_eq!(
            plan_note_refresh(&applied, Some("## Ana — 2026-08-18 09:30"), &[reply]),
            None
        );
    }

    #[test]
    fn a_thread_only_grows_at_its_end() {
        let note =
            "# Subject\n\n## Ana — 2026-08-18 09:30\n\nHello.\n\n## Bea — 2026-08-18 09:40\n\nHi.";
        let held = "## Bea — 2026-08-18 09:40\n\nHi.\n".to_string();
        let new = "## Ana — 2026-08-18 09:50\n\nBye.\n".to_string();
        let refresh =
            plan_note_refresh(note, Some("## Ana — 2026-08-18 09:30"), &[held, new]).unwrap();
        assert_eq!(refresh.heading, None, "it has its headings");
        assert_eq!(
            refresh.apply(note),
            format!("{note}\n\n## Ana — 2026-08-18 09:50\n\nBye.\n")
        );
    }

    #[test]
    fn a_note_without_a_title_gets_the_heading_at_its_top() {
        let note = "Just the words.\n";
        let new = "## Bea\n\nHi.\n".to_string();
        let refresh = plan_note_refresh(note, Some("## Ana"), &[new]).unwrap();
        assert_eq!(
            refresh.apply(note),
            "## Ana\n\nJust the words.\n\n## Bea\n\nHi.\n"
        );
    }

    #[test]
    fn only_a_decided_unarchived_note_returns_on_someone_elses_reply() {
        use DecisionClass::*;
        for (dir, decision, foreign, back) in [
            ("archives/inbox", Some(Today), true, true),
            ("archives/inbox", Some(Soon), true, true),
            ("archives/inbox", Some(Someday), true, true),
            ("archives/inbox", Some(Other), true, true),
            ("archives/inbox", Some(Archive), true, false),
            ("archives/inbox", Some(Discard), true, false),
            ("archives/inbox", None, true, false),
            ("archives/inbox", Some(Today), false, false),
            ("inbox", Some(Today), true, false),
            ("projects/road", Some(Today), true, false),
        ] {
            assert_eq!(
                returns_to_inbox(dir, decision, foreign),
                back,
                "{dir} {decision:?} {foreign}"
            );
        }
    }

    #[test]
    fn a_thread_is_known_by_its_mark_or_by_when_it_was_captured() {
        let imported = parse_imported_index(concat!(
            r#"{"digest":"d1","thread":"t1","path":"inbox","at":"2026-11-10T09:00:00-03:00"}"#,
            "\n",
            r#"{"digest":"d2","thread":"t2","path":"inbox","at":"2026-11-10T09:00:00-03:00"}"#,
            "\n",
            r#"{"digest":"d3","thread":"t3","path":"inbox"}"#,
            "\n",
        ));
        let update = ThreadUpdate {
            thread: "t2".to_string(),
            history: "77".to_string(),
            through: 5,
            first: None,
            new_messages: Vec::new(),
        };
        let marks = parse_thread_marks(&format!(
            "not json\n{}\n",
            thread_mark_line(&update, "2026-11-10T10:00:00Z")
        ));
        let known = known_threads(&imported, &marks);
        let captured = DateTime::parse_from_rfc3339("2026-11-10T09:00:00-03:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            known.get("t1"),
            Some(&KnownThread {
                history: None,
                through: captured
            })
        );
        assert_eq!(
            known.get("t2"),
            Some(&KnownThread {
                history: Some("77".to_string()),
                through: 5
            })
        );
        assert_eq!(known.get("t3"), None, "undated and never fetched");
    }

    fn message(from: &str, to: &[&str], cc: &[&str]) -> MessageHeaders {
        MessageHeaders {
            from: from.to_string(),
            to: to.iter().map(|to| to.to_string()).collect(),
            cc: cc.iter().map(|cc| cc.to_string()).collect(),
            subject: "Farm is down".to_string(),
            message_id: Some("<m2@mail.example.com>".to_string()),
            references: Some("<m1@mail.example.com>".to_string()),
            sent: Some(DateTime::from_timestamp(1_760_000_000, 0).unwrap()),
            text: Some("Is it up?\n\nThanks.".to_string()),
            ..MessageHeaders::default()
        }
    }

    fn thread() -> ThreadHeaders {
        ThreadHeaders {
            thread: "t1".to_string(),
            messages: vec![
                message("Diego <diego@example.com>", &["helpdesk@example.com"], &[]),
                message(
                    "Ana Reyes <Ana@Example.com>",
                    &["\"Tavares, Diego\" <diego@example.com>", "bea@example.com"],
                    &["Rui <rui@example.com>", "ana@example.com"],
                ),
            ],
        }
    }

    #[test]
    fn a_mailbox_is_one_plain_address_or_nothing() {
        assert_eq!(
            mailbox_address("Ana Reyes <Ana@Example.com>").as_deref(),
            Some("ana@example.com")
        );
        assert_eq!(
            mailbox_address(" bea@example.com ").as_deref(),
            Some("bea@example.com")
        );
        for bad in [
            "",
            "Ana",
            "ana@localhost",
            "ana@example.com, eve@evil.example",
            "ana@example.com\r\nBcc: eve@evil.example",
            "<ana@example.com> <eve@evil.example",
            "a b@example.com",
        ] {
            assert_eq!(mailbox_address(bad), None, "{bad:?}");
        }
        assert_eq!(
            split_mailboxes("\"Tavares, Diego\" <diego@example.com>, bea@example.com,"),
            ["\"Tavares, Diego\" <diego@example.com>", "bea@example.com"]
        );
    }

    #[test]
    fn a_reply_goes_to_the_author_everyone_or_one_address() {
        let thread = thread();
        let me = "Diego@example.com";
        let theirs = &thread.messages[1];
        assert_eq!(
            reply_recipients(theirs, &ReplyMode::Author, me),
            Recipients {
                to: vec!["ana@example.com".to_string()],
                cc: Vec::new()
            }
        );
        // Everyone once: not me, and not the author a second time.
        assert_eq!(
            reply_recipients(theirs, &ReplyMode::All, me),
            Recipients {
                to: vec!["ana@example.com".to_string()],
                cc: vec!["bea@example.com".to_string(), "rui@example.com".to_string()]
            }
        );
        assert_eq!(
            reply_recipients(theirs, &ReplyMode::One("Rui@example.com".to_string()), me).to,
            ["rui@example.com"]
        );
        // My own message is answered to whoever it went to.
        assert_eq!(
            reply_recipients(&thread.messages[0], &ReplyMode::Author, me).to,
            ["helpdesk@example.com"]
        );
        // Reply-To wins over From.
        let mut listed = theirs.clone();
        listed.reply_to = Some("list@example.com".to_string());
        assert_eq!(
            reply_recipients(&listed, &ReplyMode::Author, me).to,
            ["list@example.com"]
        );
        assert_eq!(
            thread_addresses(&thread, me),
            [
                "helpdesk@example.com",
                "ana@example.com",
                "bea@example.com",
                "rui@example.com"
            ]
        );
    }

    #[test]
    fn a_reply_only_ever_addresses_the_thread() {
        let thread = thread();
        let me = "diego@example.com";
        let reply = plan_reply(&thread, 1, &ReplyMode::All, me, "On it.\n\n").unwrap();
        assert_eq!(reply.thread, "t1");
        assert_eq!(reply.from, me);
        assert_eq!(reply.to, ["ana@example.com"]);
        assert_eq!(reply.cc, ["bea@example.com", "rui@example.com"]);
        assert_eq!(reply.subject, "Re: Farm is down");
        assert_eq!(reply.in_reply_to.as_deref(), Some("<m2@mail.example.com>"));
        assert_eq!(
            reply.references.as_deref(),
            Some("<m1@mail.example.com> <m2@mail.example.com>")
        );
        assert!(reply.body.starts_with("On it.\n\nOn "), "{}", reply.body);
        assert!(
            reply
                .body
                .ends_with(", Ana Reyes <Ana@Example.com> wrote:\n> Is it up?\n>\n> Thanks.\n"),
            "{}",
            reply.body
        );

        let stranger = ReplyMode::One("eve@evil.example".to_string());
        let error = plan_reply(&thread, 1, &stranger, me, "Hi").unwrap_err();
        assert_eq!(error.to_string(), "eve@evil.example isn't on this thread");
        assert!(plan_reply(&thread, 1, &ReplyMode::Author, me, "  \n").is_err());
        assert!(plan_reply(&thread, 7, &ReplyMode::Author, me, "Hi").is_err());
        assert!(plan_reply(&thread, 1, &ReplyMode::Author, "", "Hi").is_err());
    }

    #[test]
    fn the_subject_takes_one_re_and_a_card_finds_its_message() {
        assert_eq!(reply_subject("Farm is down"), "Re: Farm is down");
        assert_eq!(reply_subject("RE: Farm\r\n is down"), "RE: Farm is down");

        let thread = thread();
        // The note and the thread agree: by position.
        assert_eq!(answered_message(&thread, 0, 2, "", None), Some(0));
        // The thread grew past the note: by sender and minute, else the newest.
        let sent = thread.messages[1]
            .sent
            .unwrap()
            .with_timezone(&Local)
            .naive_local();
        assert_eq!(
            answered_message(&thread, 0, 1, "Diego <diego@example.com>", Some(sent)),
            Some(0)
        );
        assert_eq!(
            answered_message(&thread, 0, 1, "Someone <else@example.com>", Some(sent)),
            Some(1)
        );
        assert_eq!(
            answered_message(&ThreadHeaders::default(), 0, 1, "", None),
            None
        );
        assert_eq!(
            gmail_thread_id(&gmail_thread_url("diego@example.com", "18c2f0a")),
            Some("18c2f0a")
        );
        assert_eq!(gmail_thread_id("https://example.com/#all/18c2f0a"), None);
        assert_eq!(
            gmail_thread_id("https://mail.google.com/mail/u/0/#all/a/b?c"),
            None
        );
    }

    #[test]
    fn the_message_keeps_every_header_on_its_line() {
        let mut reply =
            plan_reply(&thread(), 1, &ReplyMode::All, "diego@example.com", "Olá.").unwrap();
        reply.subject = "Re: Café\r\nBcc: eve@evil.example".to_string();
        reply.in_reply_to = Some("<m2@mail.example.com>\r\nBcc: eve@evil.example".to_string());
        let mime = reply_mime(&reply);
        let (headers, body) = mime.split_once("\r\n\r\n").unwrap();
        let lines: Vec<&str> = headers.split("\r\n").collect();
        assert_eq!(lines[0], "From: diego@example.com");
        assert_eq!(lines[1], "To: ana@example.com");
        assert_eq!(lines[2], "Cc: bea@example.com, rui@example.com");
        assert!(lines[3].starts_with("Subject: =?UTF-8?B?"), "{}", lines[3]);
        assert!(
            !lines.iter().any(|line| line.starts_with("Bcc")),
            "no header was smuggled in: {headers}"
        );
        assert!(headers.contains("In-Reply-To: <m2@mail.example.com> Bcc: eve@evil.example"));
        assert!(headers.ends_with("Content-Transfer-Encoding: base64"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body.replace("\r\n", ""))
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), reply.body);
        assert!(body.split("\r\n").all(|line| line.len() <= 76));
    }
}
