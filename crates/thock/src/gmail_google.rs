//! The Gmail REST transport (spec `v15-unified-gmail-sync.md` §7.1):
//! `labels.list` / `messages.list` / `threads.get`, MIME-tree walking,
//! base64url body decoding, RFC 2047 header decoding, and an honest HTML →
//! plain-text reduction. One fetch covers every configured mapping; the
//! claim pass — a thread lands once, first mapping wins — happens here.
//! Read-only toward Google — no label is ever modified.

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use chrono::{DateTime, Local};
use futures::AsyncReadExt as _;
use gpui::{AsyncApp, Task};
use http_client::{AsyncBody, HttpClient, Request, http};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::gmail::{
    GmailFetched, MailTransport, MappingFetched, SyncMapping, gmail_thread_url, sanitize_subject,
    thread_marker_id,
};
use crate::google_auth::{AuthRevoked, GoogleClient, TokenKeeper, Unauthorized};
use crate::inbox::{CapturedItem, capture_digest};

const API_BASE: &str = "https://gmail.googleapis.com/gmail/v1";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GmailLabel {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LabelsResponse {
    labels: Vec<GmailLabel>,
}

pub async fn list_labels(
    http: &Arc<dyn HttpClient>,
    access_token: &str,
) -> Result<Vec<GmailLabel>> {
    let body = get_json(http, &format!("{API_BASE}/users/me/labels"), access_token).await?;
    let response: LabelsResponse =
        serde_json::from_str(&body).context("failed to parse Gmail labels response")?;
    Ok(response.labels)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageRef {
    pub id: String,
    pub thread_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct MessagesPage {
    messages: Vec<MessageRef>,
    next_page_token: Option<String>,
}

/// Every message currently carrying `label_id`, newest first (Gmail's
/// ordering), following `nextPageToken`. Ids only — bodies are fetched per
/// message.
pub async fn list_label_messages(
    http: &Arc<dyn HttpClient>,
    access_token: &str,
    label_id: &str,
) -> Result<Vec<MessageRef>> {
    let mut messages = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("labelIds", label_id)
            .append_pair("maxResults", "100");
        if let Some(token) = &page_token {
            query.append_pair("pageToken", token);
        }
        let url = format!("{API_BASE}/users/me/messages?{}", query.finish());
        let body = get_json(http, &url, access_token).await?;
        let page: MessagesPage =
            serde_json::from_str(&body).context("failed to parse Gmail messages response")?;
        messages.extend(page.messages);
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => return Ok(messages),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct MessageHeader {
    name: String,
    value: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PartBody {
    data: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct MessagePart {
    mime_type: String,
    headers: Vec<MessageHeader>,
    body: PartBody,
    parts: Vec<MessagePart>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct GmailMessage {
    label_ids: Vec<String>,
    /// Milliseconds since the epoch, as a string.
    internal_date: Option<String>,
    payload: MessagePart,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct GmailThread {
    messages: Vec<GmailMessage>,
}

async fn get_thread(
    http: &Arc<dyn HttpClient>,
    access_token: &str,
    thread_id: &str,
) -> Result<GmailThread> {
    let url = format!(
        "{API_BASE}/users/me/threads/{}?format=full",
        url_path_escape(thread_id)
    );
    let body = get_json(http, &url, access_token).await?;
    serde_json::from_str(&body).context("failed to parse Gmail thread response")
}

fn url_path_escape(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect()
}

/// Plain GET returning the body. 401 is typed as [`Unauthorized`] for the
/// retry-once-behind-refresh path; a 403 for a token without the Gmail scope
/// (a legacy V8 grant hand-copied into the unified slot) is [`AuthRevoked`],
/// so it degrades to a reconnect affordance instead of an endless retry.
async fn get_json(http: &Arc<dyn HttpClient>, url: &str, access_token: &str) -> Result<String> {
    let request = Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .body(AsyncBody::default())?;
    let mut response = http.send(request).await?;
    if response.status() == http::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(Unauthorized));
    }
    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    if response.status() == http::StatusCode::FORBIDDEN
        && body.to_lowercase().contains("insufficient")
    {
        return Err(anyhow!(AuthRevoked));
    }
    if !response.status().is_success() {
        bail!(
            "Gmail API request failed with status {}: {body}",
            response.status()
        );
    }
    Ok(body)
}

fn header_value<'a>(payload: &'a MessagePart, name: &str) -> Option<&'a str> {
    payload
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

/// The first `text/plain` leaf of the MIME tree, falling back to the first
/// `text/html` leaf reduced to text (spec §5.4). Attachments and everything
/// else are ignored.
fn extract_text_body(payload: &MessagePart) -> Option<String> {
    fn find_leaf<'a>(part: &'a MessagePart, mime_type: &str) -> Option<&'a str> {
        if part.mime_type.eq_ignore_ascii_case(mime_type)
            && let Some(data) = part.body.data.as_deref()
            && !data.is_empty()
        {
            return Some(data);
        }
        part.parts
            .iter()
            .find_map(|child| find_leaf(child, mime_type))
    }
    if let Some(data) = find_leaf(payload, "text/plain") {
        return decode_body_data(data);
    }
    let html = decode_body_data(find_leaf(payload, "text/html")?)?;
    Some(html_to_text(&html))
}

/// Gmail bodies are base64url, usually unpadded.
fn decode_body_data(data: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data.trim_end_matches('='))
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Decodes RFC 2047 encoded words (`=?charset?B|Q?…?=`) in a header value.
/// UTF-8 and Latin-1 are handled; anything else decodes lossily. Whitespace
/// between two adjacent encoded words is dropped, per the RFC.
fn decode_rfc2047(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    let mut previous_was_encoded = false;
    while let Some(start) = rest.find("=?") {
        let gap = &rest[..start];
        match decode_encoded_word(&rest[start..]) {
            Some((decoded, remainder)) => {
                let whitespace_between_words =
                    previous_was_encoded && gap.chars().all(char::is_whitespace);
                if !whitespace_between_words {
                    out.push_str(gap);
                }
                out.push_str(&decoded);
                previous_was_encoded = true;
                rest = remainder;
            }
            None => {
                // Not a real encoded word: emit up to and past the marker.
                out.push_str(gap);
                out.push_str("=?");
                previous_was_encoded = false;
                rest = &rest[start + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One `=?charset?B|Q?payload?=` at the start of `text`; returns the decoded
/// word and what follows it.
fn decode_encoded_word(text: &str) -> Option<(String, &str)> {
    let inner = text.strip_prefix("=?")?;
    let (charset, inner) = inner.split_once('?')?;
    let (encoding, inner) = inner.split_once('?')?;
    let (payload, rest) = inner.split_once("?=")?;
    let bytes = match encoding {
        "B" | "b" => base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(payload.trim_end_matches('='))
            .ok()?,
        "Q" | "q" => decode_q_encoding(payload),
        _ => return None,
    };
    let decoded = if charset.eq_ignore_ascii_case("iso-8859-1") {
        bytes.iter().map(|&byte| byte as char).collect()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Some((decoded, rest))
}

fn decode_q_encoding(payload: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len());
    let mut chars = payload.bytes();
    while let Some(byte) = chars.next() {
        match byte {
            b'_' => bytes.push(b' '),
            b'=' => {
                let high = chars.next();
                let low = chars.next();
                let decoded = (|| {
                    let high = (high? as char).to_digit(16)?;
                    let low = (low? as char).to_digit(16)?;
                    Some((high * 16 + low) as u8)
                })();
                match decoded {
                    Some(value) => bytes.push(value),
                    None => bytes.push(b'='),
                }
            }
            other => bytes.push(other),
        }
    }
    bytes
}

/// Reduces HTML to honest plain text (spec §5.4): scripts and styles dropped,
/// block-level closings become line breaks, entities decoded, blank runs
/// collapsed. Not a Markdown conversion, deliberately.
fn html_to_text(html: &str) -> String {
    let html = strip_element(html, "script");
    let html = strip_element(&html, "style");
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html.as_str();
    while let Some(open) = rest.find('<') {
        push_entities_decoded(&mut out, &rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[open + 1..open + close];
        let name = tag
            .trim_start_matches('/')
            .split([' ', '\t', '\n', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let closing = tag.starts_with('/');
        match name.as_str() {
            "br" | "hr" => out.push('\n'),
            "p" | "div" | "li" | "tr" | "ul" | "ol" | "blockquote" | "table" | "h1" | "h2"
            | "h3" | "h4" | "h5" | "h6" => {
                if closing {
                    out.push('\n');
                }
            }
            _ => {}
        }
        rest = &rest[open + close + 1..];
    }
    push_entities_decoded(&mut out, rest);

    // Trim every line and collapse runs of blanks to a single blank line.
    let mut result = String::with_capacity(out.len());
    let mut blank_run = 0usize;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            blank_run += 1;
            continue;
        }
        if !result.is_empty() {
            result.push('\n');
            if blank_run > 0 {
                result.push('\n');
            }
        }
        blank_run = 0;
        result.push_str(line);
    }
    result
}

fn strip_element(html: &str, element: &str) -> String {
    let open = format!("<{element}");
    let close = format!("</{element}");
    let lower = html.to_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(start) = lower[cursor..].find(&open) {
        let start = cursor + start;
        out.push_str(&html[cursor..start]);
        match lower[start..].find(&close) {
            Some(end) => {
                let end = start + end;
                cursor = lower[end..]
                    .find('>')
                    .map(|offset| end + offset + 1)
                    .unwrap_or(lower.len());
            }
            None => {
                cursor = lower.len();
            }
        }
    }
    out.push_str(&html[cursor..]);
    out
}

fn push_entities_decoded(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let entity_end = candidate[..candidate.len().min(12)].find(';');
        let Some(end) = entity_end else {
            out.push('&');
            rest = &candidate[1..];
            continue;
        };
        let entity = &candidate[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(character) => {
                out.push(character);
                rest = &candidate[end + 1..];
            }
            None => {
                out.push('&');
                rest = &candidate[1..];
            }
        }
    }
    out.push_str(rest);
}

/// [`MailProvider`] over the REST client: owns the token keeper and the
/// resolved label id. The refresh token stays in the keychain and is read on
/// demand — from the unified slot only, since a legacy calendar token lacks
/// the Gmail scope.
/// The one [`MailTransport`] over Gmail REST. Shared token keeper, one
/// `labels.list` per resolution, and a global claim pass so a thread carrying
/// several mapped labels is captured exactly once, by the first mapping.
pub struct GmailTransport {
    inner: Arc<TransportInner>,
}

struct TransportInner {
    http: Arc<dyn HttpClient>,
    keeper: TokenKeeper,
    account: String,
    mappings: Vec<SyncMapping>,
    /// Index-aligned resolved label ids, cached only when *every* mapping
    /// resolved — a label created later must be picked up on the next poll,
    /// and one `labels.list` per poll is near-free.
    label_ids: Mutex<Option<Vec<String>>>,
}

impl GmailTransport {
    pub fn new(
        http: Arc<dyn HttpClient>,
        client: GoogleClient,
        account: String,
        mappings: Vec<SyncMapping>,
    ) -> Self {
        Self {
            inner: Arc::new(TransportInner {
                http,
                keeper: TokenKeeper::new(client),
                account,
                mappings,
                label_ids: Mutex::new(None),
            }),
        }
    }
}

impl MailTransport for GmailTransport {
    fn fetch(&self, skip: &HashSet<String>, cx: &AsyncApp) -> Task<Result<GmailFetched>> {
        let inner = self.inner.clone();
        let skip = skip.clone();
        cx.spawn(async move |cx| {
            let token = inner.keeper.valid_access_token(&inner.http, cx).await?;
            match inner.fetch_with_token(&skip, &token).await {
                Err(error) if error.is::<Unauthorized>() => {
                    // The token aged out server-side: refresh once and retry.
                    inner.keeper.invalidate_access_token();
                    let token = inner.keeper.valid_access_token(&inner.http, cx).await?;
                    match inner.fetch_with_token(&skip, &token).await {
                        Err(error) if error.is::<Unauthorized>() => Err(anyhow!(AuthRevoked)),
                        other => other,
                    }
                }
                other => other,
            }
        })
    }
}

impl TransportInner {
    async fn fetch_with_token(
        &self,
        skip: &HashSet<String>,
        access_token: &str,
    ) -> Result<GmailFetched> {
        let label_ids = self.resolve_label_ids(access_token).await?;
        let mut seen_threads = HashSet::new();
        let mut mappings = Vec::with_capacity(self.mappings.len());
        for label_id in &label_ids {
            let Some(label_id) = label_id else {
                mappings.push(MappingFetched::LabelNotFound);
                continue;
            };
            let refs = match list_label_messages(&self.http, access_token, label_id).await {
                Ok(refs) => refs,
                Err(error) => {
                    // A cached label may have been deleted; re-resolve next
                    // poll.
                    if let Ok(mut cached) = self.label_ids.lock() {
                        *cached = None;
                    }
                    return Err(error);
                }
            };

            let mut items = Vec::new();
            for reference in refs {
                // Gmail lists newest first, so the first message seen for a
                // thread is the one that represents it (V9 §4.2) — and the
                // first *mapping* to see a thread claims it (spec §7.1).
                if !seen_threads.insert(reference.thread_id.clone()) {
                    continue;
                }
                // Both digest constructions (spec §9): V15's, and V9's for
                // threads the old stack already captured.
                if skip.contains(&capture_digest(
                    &self.account,
                    "gmail",
                    &reference.thread_id,
                )) || skip.contains(&thread_marker_id(&self.account, &reference.thread_id))
                {
                    continue;
                }
                let thread = get_thread(&self.http, access_token, &reference.thread_id).await?;
                items.push(captured_item(&thread, &self.account, &reference.thread_id));
            }
            mappings.push(MappingFetched::Items(items));
        }
        Ok(GmailFetched { mappings })
    }

    /// Resolves every mapped label by its full path name, case-insensitively
    /// — matching the last segment would collide with any other `*/inbox`
    /// label the user keeps. Never resolves on a failed `labels.list`: a
    /// transient error must not read as a missing label.
    async fn resolve_label_ids(&self, access_token: &str) -> Result<Vec<Option<String>>> {
        if let Ok(cached) = self.label_ids.lock()
            && let Some(resolved) = cached.clone()
        {
            return Ok(resolved.into_iter().map(Some).collect());
        }
        let labels = list_labels(&self.http, access_token).await?;
        let resolved: Vec<Option<String>> = self
            .mappings
            .iter()
            .map(|mapping| {
                labels
                    .iter()
                    .find(|label| label.name.eq_ignore_ascii_case(&mapping.label))
                    .map(|label| label.id.clone())
            })
            .collect();
        let complete: Option<Vec<String>> = resolved.iter().cloned().collect();
        if let (Ok(mut cached), Some(complete)) = (self.label_ids.lock(), complete) {
            *cached = Some(complete);
        }
        Ok(resolved)
    }
}

/// One labeled thread as a captured item: sanitized subject and sender from
/// the thread's first message, the latest message's date, every non-draft
/// message's text as the body, and a link back to the thread (V13 §5) —
/// `link`, never `url`. A single-message thread keeps V13's bare-body shape;
/// more than one message renders as one `##` section per message, oldest
/// first, so replies read in order.
fn captured_item(thread: &GmailThread, account: &str, thread_id: &str) -> CapturedItem {
    let mut messages: Vec<&GmailMessage> = thread
        .messages
        .iter()
        .filter(|message| !message.label_ids.iter().any(|label| label == "DRAFT"))
        .collect();
    messages.sort_by_key(|message| message_date(message));

    let first = messages.first();
    let subject = first
        .and_then(|message| header_value(&message.payload, "Subject"))
        .map(decode_rfc2047)
        .unwrap_or_default();
    let from = first
        .and_then(|message| header_value(&message.payload, "From"))
        .map(decode_rfc2047)
        .filter(|from| !from.trim().is_empty());
    let date = messages
        .last()
        .and_then(|message| message_date(message))
        .map(|instant| instant.with_timezone(&Local))
        .unwrap_or_else(Local::now);
    let body = match messages.as_slice() {
        [] => None,
        [only] => extract_text_body(&only.payload),
        many => Some(thread_sections(many)),
    };
    CapturedItem {
        source: "gmail",
        external_id: thread_id.to_string(),
        title: sanitize_subject(&subject),
        from,
        url: None,
        link: Some(gmail_thread_url(account, thread_id)),
        body,
        occurred_at: Some(date.fixed_offset()),
        due: None,
    }
}

fn message_date(message: &GmailMessage) -> Option<DateTime<chrono::Utc>> {
    message
        .internal_date
        .as_deref()
        .and_then(|millis| millis.parse::<i64>().ok())
        .and_then(DateTime::from_timestamp_millis)
}

fn thread_sections(messages: &[&GmailMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        if !out.is_empty() {
            out.push('\n');
        }
        let from = header_value(&message.payload, "From")
            .map(decode_rfc2047)
            .map(|from| crate::gmail::collapse_whitespace(&from))
            .filter(|from| !from.is_empty())
            .unwrap_or_else(|| "(unknown sender)".to_string());
        out.push_str("## ");
        out.push_str(&from);
        if let Some(date) = message_date(message) {
            let local = date.with_timezone(&Local);
            out.push_str(&format!(" — {}", local.format("%Y-%m-%d %H:%M")));
        }
        out.push('\n');
        let text = extract_text_body(&message.payload);
        match text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(text) => {
                out.push('\n');
                out.push_str(text);
                out.push('\n');
            }
            None => out.push_str("\n_(no content)_\n"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use http_client::{FakeHttpClient, Response};

    fn base64url(text: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes())
    }

    #[test]
    fn rfc2047_decoding() {
        assert_eq!(decode_rfc2047("plain subject"), "plain subject");
        assert_eq!(decode_rfc2047("=?UTF-8?B?SW52b2ljZSDinIU=?="), "Invoice ✅");
        assert_eq!(decode_rfc2047("=?utf-8?Q?caf=C3=A9_menu?="), "café menu");
        assert_eq!(decode_rfc2047("=?ISO-8859-1?Q?f=FCr_dich?="), "für dich");
        // Whitespace between adjacent encoded words is dropped…
        assert_eq!(decode_rfc2047("=?UTF-8?B?YWI=?= =?UTF-8?B?Y2Q=?="), "abcd");
        // …but real text between them survives.
        assert_eq!(
            decode_rfc2047("=?UTF-8?B?YWI=?= and =?UTF-8?B?Y2Q=?="),
            "ab and cd"
        );
        // A stray marker that isn't an encoded word passes through.
        assert_eq!(decode_rfc2047("worth =? nothing"), "worth =? nothing");
    }

    #[test]
    fn html_reduction() {
        let html = "<html><head><style>p { color: red }</style></head><body>\
                    <p>Hi &amp; welcome,</p><script>alert(1)</script>\
                    <div>Your invoice<br>is attached.</div>\
                    <ul><li>one</li><li>two &#8212; both</li></ul>\
                    <p>&nbsp;</p><p></p>&copy; Acme</body></html>";
        assert_eq!(
            html_to_text(html),
            "Hi & welcome,\nYour invoice\nis attached.\none\ntwo — both\n\n&copy; Acme"
        );
    }

    #[test]
    fn body_extraction_prefers_plain_text() {
        let multipart = MessagePart {
            mime_type: "multipart/alternative".to_string(),
            parts: vec![
                MessagePart {
                    mime_type: "text/html".to_string(),
                    body: PartBody {
                        data: Some(base64url("<p>rich</p>")),
                    },
                    ..Default::default()
                },
                MessagePart {
                    mime_type: "text/plain".to_string(),
                    body: PartBody {
                        data: Some(base64url("plain wins")),
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(extract_text_body(&multipart).as_deref(), Some("plain wins"));

        let html_only = MessagePart {
            mime_type: "text/html".to_string(),
            body: PartBody {
                data: Some(base64url("<p>only &lt;html&gt;</p>")),
            },
            ..Default::default()
        };
        assert_eq!(
            extract_text_body(&html_only).as_deref(),
            Some("only <html>")
        );

        let attachment_only = MessagePart {
            mime_type: "application/pdf".to_string(),
            ..Default::default()
        };
        assert_eq!(extract_text_body(&attachment_only), None);
    }

    fn plain_message(
        from: &str,
        text: Option<&str>,
        millis: &str,
        labels: &[&str],
    ) -> GmailMessage {
        GmailMessage {
            label_ids: labels.iter().map(|label| label.to_string()).collect(),
            internal_date: Some(millis.to_string()),
            payload: MessagePart {
                mime_type: "text/plain".to_string(),
                headers: vec![
                    MessageHeader {
                        name: "Subject".to_string(),
                        value: "Plans".to_string(),
                    },
                    MessageHeader {
                        name: "From".to_string(),
                        value: from.to_string(),
                    },
                ],
                body: PartBody {
                    data: text.map(base64url),
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn threads_render_every_message_in_order() {
        // Unsorted input, a draft, and a reply with no text.
        let thread = GmailThread {
            messages: vec![
                plain_message("Bea <bea@example.com>", Some("Second"), "2000", &[]),
                plain_message("Draft <me@example.com>", Some("wip"), "3000", &["DRAFT"]),
                plain_message("Ana <ana@example.com>", Some("First"), "1000", &[]),
                plain_message("Cal <cal@example.com>", None, "4000", &[]),
            ],
        };
        let item = captured_item(&thread, "diego@example.com", "t-1");
        assert_eq!(item.from.as_deref(), Some("Ana <ana@example.com>"));
        let body = item.body.as_deref().unwrap();
        let ana = body.find("## Ana").expect("ana section");
        let bea = body.find("## Bea").expect("bea section");
        let cal = body.find("## Cal").expect("cal section");
        assert!(ana < bea && bea < cal, "{body}");
        assert!(body.contains("First") && body.contains("Second"), "{body}");
        assert!(body.contains("_(no content)_"), "{body}");
        assert!(!body.contains("Draft") && !body.contains("wip"), "{body}");

        // A single-message thread keeps the bare V13 body — no section
        // heading.
        let single = GmailThread {
            messages: vec![plain_message(
                "Ana <ana@example.com>",
                Some("Solo"),
                "1000",
                &[],
            )],
        };
        let item = captured_item(&single, "diego@example.com", "t-2");
        assert_eq!(item.body.as_deref(), Some("Solo"));
    }

    #[test]
    fn transport_claims_threads_by_mapping_priority_and_skips_imported() {
        let http = FakeHttpClient::create(|request| async move {
            let uri = request.uri().to_string();
            let body = if uri.contains("/labels") {
                r#"{"labels": [
                    {"id": "Label_1", "name": "thock/Backlog"},
                    {"id": "Label_2", "name": "thock/inbox"},
                    {"id": "INBOX", "name": "INBOX"}
                ]}"#
                .to_string()
            } else if uri.contains("labelIds=Label_1") {
                // Two messages of one thread (newest first), plus an
                // already-imported one.
                r#"{"messages": [
                    {"id": "m3", "threadId": "t-both"},
                    {"id": "m2", "threadId": "t-both"},
                    {"id": "m1", "threadId": "t-old"}
                ]}"#
                .to_string()
            } else if uri.contains("labelIds=Label_2") {
                r#"{"messages": [
                    {"id": "m3", "threadId": "t-both"},
                    {"id": "m4", "threadId": "t-inbox"}
                ]}"#
                .to_string()
            } else if uri.contains("/threads/t-both") {
                assert!(uri.contains("format=full"), "{uri}");
                format!(
                    r#"{{"id": "t-both", "messages": [
                        {{"id": "m3", "internalDate": "1755500000000",
                          "payload": {{"headers": [
                            {{"name": "Subject", "value": "Re: =?UTF-8?Q?Caf=C3=A9?= plans"}},
                            {{"name": "From", "value": "Diego <diego@example.com>"}}
                          ],
                          "mimeType": "text/plain",
                          "body": {{"data": "{}"}}}}}},
                        {{"id": "m2", "internalDate": "1755400000000",
                          "payload": {{"headers": [
                            {{"name": "Subject", "value": "=?UTF-8?Q?Caf=C3=A9?= plans"}},
                            {{"name": "From", "value": "Ana <ana@example.com>"}}
                          ],
                          "mimeType": "text/plain",
                          "body": {{"data": "{}"}}}}}}
                    ]}}"#,
                    base64url("Sure, 9am works."),
                    base64url("Coffee tomorrow?")
                )
            } else if uri.contains("/threads/t-inbox") {
                r#"{"id": "t-inbox", "messages": [
                    {"id": "m4", "internalDate": "1755500000000",
                     "payload": {"headers": [{"name": "Subject", "value": "Read later"}]}}
                ]}"#
                .to_string()
            } else {
                panic!("unexpected request to {uri}");
            };
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body.into_bytes()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;

        let inner = test_inner(
            http,
            &[
                ("thock/backlog", "archives/emails"),
                ("thock/inbox", "inbox"),
            ],
        );
        // The V9 digest still skips (spec §9) — the fake panics on
        // /messages/m1 if it doesn't.
        let skip: HashSet<String> = [thread_marker_id("diego@example.com", "t-old")]
            .into_iter()
            .collect();
        let fetched = block_on(inner.fetch_with_token(&skip, "token")).unwrap();
        assert_eq!(fetched.mappings.len(), 2);
        let MappingFetched::Items(backlog) = &fetched.mappings[0] else {
            panic!("expected items for the backlog mapping");
        };
        // t-both claimed here: whole thread fetched, subject and sender from
        // the first message, replies rendered oldest first…
        assert_eq!(backlog.len(), 1);
        assert_eq!(backlog[0].external_id, "t-both");
        assert_eq!(backlog[0].title, "Café plans");
        assert_eq!(backlog[0].from.as_deref(), Some("Ana <ana@example.com>"));
        assert_eq!(
            backlog[0].link.as_deref(),
            Some("https://mail.google.com/mail/u/diego@example.com/#all/t-both")
        );
        let body = backlog[0].body.as_deref().unwrap();
        let ana = body
            .find("## Ana <ana@example.com>")
            .expect("first message section");
        let diego = body
            .find("## Diego <diego@example.com>")
            .expect("reply section");
        assert!(ana < diego, "{body}");
        assert!(body.contains("Coffee tomorrow?"), "{body}");
        assert!(body.contains("Sure, 9am works."), "{body}");
        let MappingFetched::Items(inbox) = &fetched.mappings[1] else {
            panic!("expected items for the inbox mapping");
        };
        // …so the inbox mapping sees only its own thread.
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].external_id, "t-inbox");
    }

    fn test_inner(http: Arc<dyn HttpClient>, mappings: &[(&str, &str)]) -> TransportInner {
        TransportInner {
            http,
            keeper: TokenKeeper::new(GoogleClient {
                client_id: "id".to_string(),
                client_secret: None,
            }),
            account: "diego@example.com".to_string(),
            mappings: mappings
                .iter()
                .map(|(label, path)| SyncMapping {
                    label: label.to_string(),
                    path: path.to_string(),
                })
                .collect(),
            label_ids: Mutex::new(None),
        }
    }

    /// A missing label holds its own mapping without blocking the others,
    /// and matching is against the full path, never the last segment.
    #[test]
    fn missing_label_holds_only_its_mapping() {
        let http = FakeHttpClient::create(|request| async move {
            let uri = request.uri().to_string();
            let body = if uri.contains("/labels") {
                r#"{"labels": [
                    {"id": "Label_9", "name": "work/backlog"},
                    {"id": "Label_2", "name": "thock/inbox"}
                ]}"#
            } else if uri.contains("labelIds=Label_2") {
                r#"{"messages": []}"#
            } else {
                panic!("unexpected request to {uri}");
            };
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body.as_bytes().to_vec()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let inner = test_inner(
            http,
            &[
                ("thock/backlog", "archives/emails"),
                ("thock/inbox", "inbox"),
            ],
        );
        let fetched = block_on(inner.fetch_with_token(&HashSet::new(), "token")).unwrap();
        assert_eq!(
            fetched.mappings,
            vec![
                MappingFetched::LabelNotFound,
                MappingFetched::Items(Vec::new()),
            ]
        );
        // Incomplete resolutions are never cached, so the label is looked up
        // again next poll.
        assert!(inner.label_ids.lock().unwrap().is_none());
    }

    /// A transient `labels.list` error propagates instead of reading as a
    /// missing label (V13 §11's rule, kept).
    #[test]
    fn transient_labels_error_propagates() {
        let http = FakeHttpClient::create(|_| async move {
            Ok(Response::builder()
                .status(503)
                .body(AsyncBody::from(b"unavailable".to_vec()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let inner = test_inner(http, &[("thock/backlog", "archives/emails")]);
        let error = block_on(inner.fetch_with_token(&HashSet::new(), "token")).unwrap_err();
        assert!(error.to_string().contains("503"), "{error:#}");
    }

    #[test]
    fn insufficient_scope_is_auth_revoked() {
        let http = FakeHttpClient::create(|_| async move {
            Ok(Response::builder()
                .status(403)
                .body(AsyncBody::from(
                    br#"{"error": {"status": "PERMISSION_DENIED",
                        "message": "Request had insufficient authentication scopes."}}"#
                        .to_vec(),
                ))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let error = block_on(list_labels(&http, "token")).unwrap_err();
        assert!(error.is::<AuthRevoked>());
    }
}
