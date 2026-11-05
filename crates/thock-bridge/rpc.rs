//! JSON-RPC and the MCP methods the bridge serves (spec §7–§8): the framing,
//! the six tool schemas, the two resources, and `dispatch`, which turns a
//! request into a response over whatever `ToolHost` the caller provides —
//! the desk's file system online, the client's cache offline.
//!
//! The types mirror `context_server::types` field for field; that crate
//! pulls GPUI, which the client binary must not.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::rules::Refusal;

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const SERVER_NAME: &str = "thock";
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INDEX_RESOURCE_URI: &str = "thock://memory/index";
pub const NOTE_RESOURCE_PREFIX: &str = "thock://note/";
const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: usize = 50;
const DEFAULT_RECENT_DAYS: u32 = 7;
const MAX_RECENT_DAYS: u32 = 366;
const MAX_TODAY_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

fn jsonrpc_version() -> String {
    "2.0".to_string()
}

impl JsonRpcRequest {
    pub fn new(id: Value, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id: Some(id),
            method: method.to_string(),
            params: Some(params),
        }
    }

    /// Parses one request; a malformed body is the error response to send
    /// back, with a null id as JSON-RPC asks.
    pub fn parse(body: &str) -> Result<Self, Box<JsonRpcResponse>> {
        let value: Value = serde_json::from_str(body).map_err(|error| {
            Box::new(error_response(
                Value::Null,
                PARSE_ERROR,
                &format!("not JSON: {error}"),
            ))
        })?;
        if !value.is_object() {
            return Err(Box::new(error_response(
                Value::Null,
                INVALID_REQUEST,
                "one JSON-RPC request object per POST",
            )));
        }
        serde_json::from_value(value).map_err(|error| {
            Box::new(error_response(
                Value::Null,
                INVALID_REQUEST,
                &error.to_string(),
            ))
        })
    }

    /// A notification carries no id and gets no reply.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    pub fn tool_name(&self) -> Option<&str> {
        self.params.as_ref()?.get("name")?.as_str()
    }

    pub fn arguments(&self) -> Value {
        self.params
            .as_ref()
            .and_then(|params| params.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!({}))
    }

    /// The client's bookkeeping for a write (§9.5): the idempotency id and
    /// the day the agent called, carried in `params._meta`.
    pub fn call_meta(&self) -> CallMeta {
        let meta = self.params.as_ref().and_then(|params| params.get("_meta"));
        CallMeta {
            call_id: meta
                .and_then(|meta| meta.get("call_id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            date: meta
                .and_then(|meta| meta.get("date"))
                .and_then(Value::as_str)
                .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()),
        }
    }

    /// The same request with `_meta` set, as the client forwards it.
    pub fn with_meta(mut self, meta: &CallMeta) -> Self {
        let mut params = self.params.take().unwrap_or_else(|| json!({}));
        if let Value::Object(map) = &mut params {
            let mut entry = serde_json::Map::new();
            if let Some(call_id) = &meta.call_id {
                entry.insert("call_id".into(), Value::String(call_id.clone()));
            }
            if let Some(date) = meta.date {
                entry.insert("date".into(), Value::String(date.to_string()));
            }
            map.insert("_meta".into(), Value::Object(entry));
        }
        self.params = Some(params);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallMeta {
    pub call_id: Option<String>,
    pub date: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn parse(body: &str) -> Result<Self, String> {
        serde_json::from_str(body).map_err(|error| error.to_string())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The tool result inside a `tools/call` reply, if that is what this is.
    pub fn tool_result(&self) -> Option<CallToolResponse> {
        serde_json::from_value(self.result.clone()?).ok()
    }
}

pub fn error_response(id: Value, code: i64, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: jsonrpc_version(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    }
}

pub fn result_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: jsonrpc_version(),
        id,
        result: Some(result),
        error: None,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolResponseContent {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallToolResponse {
    pub content: Vec<ToolResponseContent>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

impl CallToolResponse {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResponseContent::Text { text: text.into() }],
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResponseContent::Text { text: text.into() }],
            is_error: true,
        }
    }

    /// The text of the first content block.
    pub fn first_text(&self) -> &str {
        self.content
            .first()
            .map(|ToolResponseContent::Text { text }| text.as_str())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceContents {
    pub uri: String,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
    pub since: Option<NaiveDate>,
    pub folder: Option<String>,
}

impl SearchQuery {
    /// The key a search is cached under (§9.4): a hash of the query and
    /// its filters.
    pub fn cache_key(&self) -> String {
        crate::sha256_hex(&format!(
            "{}\n{}\n{}\n{}",
            self.query.trim().to_lowercase(),
            self.limit,
            self.since.map(|d| d.to_string()).unwrap_or_default(),
            self.folder.clone().unwrap_or_default()
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub modified: NaiveDate,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentNote {
    pub path: String,
    pub title: String,
    pub modified: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadTarget {
    Path(String),
    Link(String),
}

/// What a write did, in the sync core's words plus the client's `queued`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    Created,
    SectionAdded,
    Applied,
    Noop,
    Queued,
}

impl WriteOutcome {
    pub fn word(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::SectionAdded => "section_added",
            Self::Applied => "applied",
            Self::Noop => "noop",
            Self::Queued => "queued",
        }
    }

    pub fn from_outcome(outcome: thock_sync_core::Outcome) -> Self {
        match outcome {
            thock_sync_core::Outcome::Created => Self::Created,
            thock_sync_core::Outcome::SectionAdded => Self::SectionAdded,
            thock_sync_core::Outcome::Applied | thock_sync_core::Outcome::KeptBoth => Self::Applied,
            thock_sync_core::Outcome::Noop => Self::Noop,
        }
    }
}

/// A sentence the agent can relay (G7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Refusal> for HostError {
    fn from(refusal: Refusal) -> Self {
        Self(refusal.to_string())
    }
}

impl From<anyhow::Error> for HostError {
    fn from(error: anyhow::Error) -> Self {
        Self(format!("{error:#}"))
    }
}

impl From<String> for HostError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

/// What a tool needs from whoever holds the vault: the desk over its file
/// system, or the client over its cache and outbox when the desk is away.
pub trait ToolHost {
    fn context(&mut self) -> impl Future<Output = Result<String, HostError>>;
    fn memory_index(&mut self) -> impl Future<Output = Result<String, HostError>>;
    fn search(
        &mut self,
        query: &SearchQuery,
    ) -> impl Future<Output = Result<Vec<SearchHit>, HostError>>;
    fn read(&mut self, target: &ReadTarget) -> impl Future<Output = Result<String, HostError>>;
    fn recent(
        &mut self,
        days: u32,
        folder: Option<&str>,
    ) -> impl Future<Output = Result<Vec<RecentNote>, HostError>>;
    fn remember(
        &mut self,
        fact: &str,
        meta: &CallMeta,
    ) -> impl Future<Output = Result<WriteOutcome, HostError>>;
    fn note_today(
        &mut self,
        lines: &[String],
        meta: &CallMeta,
    ) -> impl Future<Output = Result<WriteOutcome, HostError>>;
}

/// The six tools and the rules their descriptions carry (§8).
pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "context".into(),
            description: "What this person's Thock vault knows: today's date and week, where \
                          today's and this week's notes live, what past sessions learned about \
                          them (memory/index.md), the Routines they run, their language, the \
                          topics they asked never to keep, and who you are to the vault. Call it \
                          when a task touches the person, their people, their threads or how \
                          they like things done, and at the start of a session about them."
                .into(),
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        },
        Tool {
            name: "search".into(),
            description: "Keyword search over the vault's notes: every word must match (whole \
                          word, any case; `plan*` matches a prefix). Returns the path, title, \
                          last-modified date and a snippet of each hit. Search before assuming."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to find, space-separated."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "default": DEFAULT_SEARCH_LIMIT},
                    "since": {"type": "string", "description": "Only notes modified on or after this day, YYYY-MM-DD."},
                    "folder": {"type": "string", "description": "Only notes under this vault folder, e.g. `daily`."}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        Tool {
            name: "read".into(),
            description: "The text of one note, by vault-relative path (`daily/2026-11-03.md`) \
                          or by `[[wikilink]]` title. Folders starting with a dot are Thock's own \
                          and stay closed."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Vault-relative path of the note."},
                    "link": {"type": "string", "description": "A note title, as a [[wikilink]] names it."}
                },
                "additionalProperties": false
            }),
        },
        Tool {
            name: "recent".into(),
            description: "Notes modified in the last `days` days, newest first, with the first \
                          heading of each. Use it to see what the person has been writing lately."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "days": {"type": "integer", "minimum": 1, "maximum": MAX_RECENT_DAYS, "default": DEFAULT_RECENT_DAYS},
                    "folder": {"type": "string", "description": "Only notes under this vault folder."}
                },
                "additionalProperties": false
            }),
        },
        Tool {
            name: "remember".into(),
            description: "Leave one dated line in the person's memory inbox, for the Reflect \
                          ritual to file. `remember` is for things about the person that will \
                          still be true next month — who someone is to them, how they like things \
                          done, a plan that changed, a correction they gave you. Never for what a \
                          file, an email or a tool said, and never a secret. One fact, one line, \
                          under 300 characters, in your words; the desk adds the date and your name."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "fact": {"type": "string", "description": "The fact, one line, no leading bullet."}
                },
                "required": ["fact"],
                "additionalProperties": false
            }),
        },
        Tool {
            name: "note_today".into(),
            description: "Log what you did to the person's daily note, under its `From your \
                          agents` section, one line per item prefixed with your name. Call it \
                          once, at the end of a session that did something worth a line in their \
                          day. The note is created from their template when it is missing."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "line": {"type": "string", "description": "One line."},
                    "lines": {"type": "array", "items": {"type": "string"}, "maxItems": MAX_TODAY_LINES, "description": "Several lines, in order."}
                },
                "additionalProperties": false
            }),
        },
    ]
}

pub fn resources() -> Vec<Resource> {
    vec![
        Resource {
            uri: INDEX_RESOURCE_URI.into(),
            name: "What Thock has learned".into(),
            description: Some("memory/index.md, capped as the desk agent reads it.".into()),
            mime_type: Some("text/markdown".into()),
        },
        Resource {
            uri: format!("{NOTE_RESOURCE_PREFIX}{{path}}"),
            name: "A note".into(),
            description: Some("Any note by vault-relative path; the same rules as `read`.".into()),
            mime_type: Some("text/markdown".into()),
        },
    ]
}

pub fn initialize_result(version: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}, "resources": {}},
        "serverInfo": {"name": SERVER_NAME, "version": version},
        "instructions": "Read the thock-memory skill if it is installed; its rules apply to every tool here."
    })
}

/// Answers a request over `host`. `None` for a notification, which gets no
/// reply. `initialize`, `tools/list` and `resources/list` need no host
/// work, so a client can answer them offline through the same function.
pub async fn dispatch<H: ToolHost>(
    host: &mut H,
    request: JsonRpcRequest,
    version: &str,
) -> Option<JsonRpcResponse> {
    let id = request.id.clone()?;
    let response = match request.method.as_str() {
        "initialize" => result_response(id, initialize_result(version)),
        "ping" => result_response(id, json!({})),
        "tools/list" => result_response(id, json!({"tools": tools()})),
        "resources/list" => result_response(id, json!({"resources": resources()})),
        "resources/read" => {
            let uri = request
                .params
                .as_ref()
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match read_resource(host, &uri).await {
                Ok(contents) => result_response(id, json!({"contents": [contents]})),
                Err(error) => error_response(id, INVALID_PARAMS, &error.0),
            }
        }
        "tools/call" => {
            let result = call_tool(host, &request).await;
            match result {
                Ok(response) => result_response(id, json!(response)),
                Err(error) => error_response(id, INVALID_PARAMS, &error.0),
            }
        }
        other => error_response(id, METHOD_NOT_FOUND, &format!("{other} is not served here")),
    };
    Some(response)
}

async fn read_resource<H: ToolHost>(
    host: &mut H,
    uri: &str,
) -> Result<ResourceContents, HostError> {
    let text = if uri == INDEX_RESOURCE_URI {
        host.memory_index().await?
    } else if let Some(path) = uri.strip_prefix(NOTE_RESOURCE_PREFIX) {
        host.read(&ReadTarget::Path(path.to_string())).await?
    } else {
        return Err(HostError(format!("{uri} is not a resource here")));
    };
    Ok(ResourceContents {
        uri: uri.to_string(),
        mime_type: Some("text/markdown".into()),
        text,
    })
}

/// A tool call's reply: a refusal or a host failure is a result with
/// `is_error`, so the agent reads the sentence; only an unknown tool or
/// unusable arguments are protocol errors.
async fn call_tool<H: ToolHost>(
    host: &mut H,
    request: &JsonRpcRequest,
) -> Result<CallToolResponse, HostError> {
    let name = request
        .tool_name()
        .ok_or_else(|| HostError("tools/call needs a tool name".into()))?;
    let arguments = request.arguments();
    let meta = request.call_meta();
    let outcome: Result<String, HostError> = match name {
        "context" => host.context().await,
        "search" => {
            let query = parse_search_query(&arguments)?;
            host.search(&query)
                .await
                .map(|hits| render_hits(&query, &hits))
        }
        "read" => {
            let target = parse_read_target(&arguments)?;
            host.read(&target).await
        }
        "recent" => {
            let days = arguments
                .get("days")
                .and_then(Value::as_u64)
                .map(|days| u32::try_from(days).unwrap_or(MAX_RECENT_DAYS))
                .unwrap_or(DEFAULT_RECENT_DAYS)
                .clamp(1, MAX_RECENT_DAYS);
            let folder = arguments.get("folder").and_then(Value::as_str);
            host.recent(days, folder)
                .await
                .map(|notes| render_recent(days, &notes))
        }
        "remember" => {
            let fact = arguments
                .get("fact")
                .and_then(Value::as_str)
                .ok_or_else(|| HostError("remember needs a `fact` string".into()))?;
            host.remember(fact, &meta)
                .await
                .map(|outcome| match outcome {
                    WriteOutcome::Queued => QUEUED_SENTENCE.to_string(),
                    WriteOutcome::Noop => "That line is already in memory/inbox.md (noop).".into(),
                    other => format!(
                        "Noted in memory/inbox.md; the Reflect ritual files it ({}).",
                        other.word()
                    ),
                })
        }
        "note_today" => {
            let lines = parse_lines(&arguments)?;
            host.note_today(&lines, &meta)
                .await
                .map(|outcome| match outcome {
                    WriteOutcome::Queued => QUEUED_SENTENCE.to_string(),
                    other => format!("Added to today's note ({}).", other.word()),
                })
        }
        other => return Err(HostError(format!("no tool named {other}"))),
    };
    Ok(match outcome {
        Ok(text) => CallToolResponse::text(text),
        Err(error) => CallToolResponse::error(error.0),
    })
}

pub const QUEUED_SENTENCE: &str = "queued: the desk is unreachable; this will land when it is back";

fn parse_search_query(arguments: &Value) -> Result<SearchQuery, HostError> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| HostError("search needs a `query` string".into()))?;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| usize::try_from(limit).unwrap_or(MAX_SEARCH_LIMIT))
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let since = match arguments.get("since").and_then(Value::as_str) {
        Some(since) => Some(
            NaiveDate::parse_from_str(since.trim(), "%Y-%m-%d")
                .map_err(|_| HostError("`since` must be a day, YYYY-MM-DD".into()))?,
        ),
        None => None,
    };
    let folder = arguments
        .get("folder")
        .and_then(Value::as_str)
        .map(|folder| folder.trim().trim_matches('/').to_string())
        .filter(|folder| !folder.is_empty());
    Ok(SearchQuery {
        query: query.to_string(),
        limit,
        since,
        folder,
    })
}

fn parse_read_target(arguments: &Value) -> Result<ReadTarget, HostError> {
    let path = arguments
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let link = arguments
        .get("link")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|link| !link.is_empty());
    match (path, link) {
        (Some(path), _) => Ok(ReadTarget::Path(path.to_string())),
        (None, Some(link)) => Ok(ReadTarget::Link(link.to_string())),
        (None, None) => Err(HostError("read needs a `path` or a `link`".into())),
    }
}

fn parse_lines(arguments: &Value) -> Result<Vec<String>, HostError> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(line) = arguments.get("line").and_then(Value::as_str) {
        lines.push(line.to_string());
    }
    if let Some(array) = arguments.get("lines").and_then(Value::as_array) {
        for item in array {
            let line = item
                .as_str()
                .ok_or_else(|| HostError("`lines` must hold strings".into()))?;
            lines.push(line.to_string());
        }
    }
    if lines.is_empty() {
        return Err(HostError("note_today needs a `line` or `lines`".into()));
    }
    if lines.len() > MAX_TODAY_LINES {
        return Err(HostError(format!(
            "note_today takes at most {MAX_TODAY_LINES} lines per call"
        )));
    }
    Ok(lines)
}

pub fn render_hits(query: &SearchQuery, hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return format!("No notes match `{}`.", query.query);
    }
    let mut out = String::new();
    for hit in hits {
        out.push_str(&format!(
            "- `{}` — {} ({})\n  {}\n",
            hit.path, hit.title, hit.modified, hit.snippet
        ));
    }
    out.trim_end().to_string()
}

pub fn render_recent(days: u32, notes: &[RecentNote]) -> String {
    if notes.is_empty() {
        return format!("No notes changed in the last {days} days.");
    }
    notes
        .iter()
        .map(|note| format!("- `{}` — {} ({})", note.path, note.title, note.modified))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeHost {
        notes: BTreeMap<String, String>,
        remembered: Vec<(String, CallMeta)>,
        today: Vec<Vec<String>>,
    }

    impl ToolHost for FakeHost {
        async fn context(&mut self) -> Result<String, HostError> {
            Ok("# Right now\n".into())
        }

        async fn memory_index(&mut self) -> Result<String, HostError> {
            Ok("# Learned\n- one\n".into())
        }

        async fn search(&mut self, query: &SearchQuery) -> Result<Vec<SearchHit>, HostError> {
            Ok(self
                .notes
                .iter()
                .filter(|(_, body)| body.contains(&query.query))
                .map(|(path, body)| SearchHit {
                    path: path.clone(),
                    title: path.clone(),
                    modified: NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
                    snippet: body.clone(),
                })
                .collect())
        }

        async fn read(&mut self, target: &ReadTarget) -> Result<String, HostError> {
            match target {
                ReadTarget::Path(path) => self
                    .notes
                    .get(path)
                    .cloned()
                    .ok_or_else(|| HostError(format!("{path} is not in the vault"))),
                ReadTarget::Link(link) => Err(HostError(format!("no note titled {link}"))),
            }
        }

        async fn recent(&mut self, _: u32, _: Option<&str>) -> Result<Vec<RecentNote>, HostError> {
            Ok(Vec::new())
        }

        async fn remember(
            &mut self,
            fact: &str,
            meta: &CallMeta,
        ) -> Result<WriteOutcome, HostError> {
            if fact.contains("secret") {
                return Err(HostError("that looks like a secret".into()));
            }
            self.remembered.push((fact.to_string(), meta.clone()));
            Ok(WriteOutcome::Applied)
        }

        async fn note_today(
            &mut self,
            lines: &[String],
            _: &CallMeta,
        ) -> Result<WriteOutcome, HostError> {
            self.today.push(lines.to_vec());
            Ok(WriteOutcome::SectionAdded)
        }
    }

    fn call(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest::new(json!(1), method, params)
    }

    #[test]
    fn initialize_and_lists_need_no_host() {
        let mut host = FakeHost::default();
        let response =
            pollster::block_on(dispatch(&mut host, call("initialize", json!({})), "1.0")).unwrap();
        let result = response.result.unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
        let response =
            pollster::block_on(dispatch(&mut host, call("tools/list", json!({})), "1.0")).unwrap();
        let names: Vec<String> = response.result.unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "context",
                "search",
                "read",
                "recent",
                "remember",
                "note_today"
            ]
        );
        let response = pollster::block_on(dispatch(
            &mut host,
            call("resources/list", json!({})),
            "1.0",
        ))
        .unwrap();
        assert_eq!(
            response.result.unwrap()["resources"][0]["uri"],
            INDEX_RESOURCE_URI
        );
    }

    #[test]
    fn unknown_methods_and_notifications() {
        let mut host = FakeHost::default();
        let response =
            pollster::block_on(dispatch(&mut host, call("prompts/list", json!({})), "1.0"))
                .unwrap();
        assert_eq!(response.error.unwrap().code, METHOD_NOT_FOUND);
        let notification = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: None,
        };
        assert!(pollster::block_on(dispatch(&mut host, notification, "1.0")).is_none());
    }

    #[test]
    fn malformed_bodies_are_parse_errors() {
        let error = JsonRpcRequest::parse("{not json").unwrap_err();
        assert_eq!(error.error.unwrap().code, PARSE_ERROR);
        assert_eq!(error.id, Value::Null);
        let error = JsonRpcRequest::parse("[1,2]").unwrap_err();
        assert_eq!(error.error.unwrap().code, INVALID_REQUEST);
        let error = JsonRpcRequest::parse(r#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();
        assert_eq!(error.error.unwrap().code, INVALID_REQUEST);
        let parsed =
            JsonRpcRequest::parse(r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#).unwrap();
        assert_eq!(parsed.id, Some(json!("a")));
    }

    #[test]
    fn tool_calls_carry_refusals_as_results_and_bad_args_as_errors() {
        let mut host = FakeHost::default();
        host.notes.insert("daily/x.md".into(), "hello world".into());
        let response = pollster::block_on(dispatch(
            &mut host,
            call(
                "tools/call",
                json!({"name": "read", "arguments": {"path": "daily/x.md"}}),
            ),
            "1.0",
        ))
        .unwrap();
        let result = response.tool_result().unwrap();
        assert!(!result.is_error);
        assert_eq!(result.first_text(), "hello world");

        let response = pollster::block_on(dispatch(
            &mut host,
            call(
                "tools/call",
                json!({"name": "read", "arguments": {"path": "daily/y.md"}}),
            ),
            "1.0",
        ))
        .unwrap();
        let result = response.tool_result().unwrap();
        assert!(result.is_error);
        assert_eq!(result.first_text(), "daily/y.md is not in the vault");

        let response = pollster::block_on(dispatch(
            &mut host,
            call("tools/call", json!({"name": "read", "arguments": {}})),
            "1.0",
        ))
        .unwrap();
        assert_eq!(response.error.unwrap().code, INVALID_PARAMS);

        let response = pollster::block_on(dispatch(
            &mut host,
            call("tools/call", json!({"name": "nope", "arguments": {}})),
            "1.0",
        ))
        .unwrap();
        assert!(
            response
                .error
                .unwrap()
                .message
                .contains("no tool named nope")
        );
    }

    #[test]
    fn writes_carry_their_meta_and_outcome_words() {
        let mut host = FakeHost::default();
        let request = call(
            "tools/call",
            json!({"name": "remember", "arguments": {"fact": "Likes tea"}}),
        )
        .with_meta(&CallMeta {
            call_id: Some("abc".into()),
            date: NaiveDate::from_ymd_opt(2026, 11, 1),
        });
        let response = pollster::block_on(dispatch(&mut host, request, "1.0")).unwrap();
        assert_eq!(
            response.tool_result().unwrap().first_text(),
            "Noted in memory/inbox.md; the Reflect ritual files it (applied)."
        );
        assert_eq!(host.remembered[0].0, "Likes tea");
        assert_eq!(host.remembered[0].1.call_id.as_deref(), Some("abc"));
        assert_eq!(
            host.remembered[0].1.date,
            NaiveDate::from_ymd_opt(2026, 11, 1)
        );

        let response = pollster::block_on(dispatch(
            &mut host,
            call(
                "tools/call",
                json!({"name": "remember", "arguments": {"fact": "the secret"}}),
            ),
            "1.0",
        ))
        .unwrap();
        assert!(response.tool_result().unwrap().is_error);

        let response = pollster::block_on(dispatch(
            &mut host,
            call(
                "tools/call",
                json!({"name": "note_today", "arguments": {"line": "a", "lines": ["b"]}}),
            ),
            "1.0",
        ))
        .unwrap();
        assert_eq!(
            response.tool_result().unwrap().first_text(),
            "Added to today's note (section_added)."
        );
        assert_eq!(host.today, vec![vec!["a".to_string(), "b".to_string()]]);
    }

    #[test]
    fn search_arguments_are_parsed_and_rendered() {
        let query = parse_search_query(
            &json!({"query": " ana ", "limit": 500, "since": "2026-01-01", "folder": "/daily/"}),
        )
        .unwrap();
        assert_eq!(query.query, "ana");
        assert_eq!(query.limit, MAX_SEARCH_LIMIT);
        assert_eq!(query.since, NaiveDate::from_ymd_opt(2026, 1, 1));
        assert_eq!(query.folder.as_deref(), Some("daily"));
        assert!(parse_search_query(&json!({"query": "x", "since": "yesterday"})).is_err());
        assert_eq!(render_hits(&query, &[]), "No notes match `ana`.");
        let hits = vec![SearchHit {
            path: "daily/x.md".into(),
            title: "X".into(),
            modified: NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
            snippet: "- Met Ana".into(),
        }];
        assert_eq!(
            render_hits(&query, &hits),
            "- `daily/x.md` — X (2026-11-04)\n  - Met Ana"
        );
        assert_eq!(
            render_recent(7, &[]),
            "No notes changed in the last 7 days."
        );
    }

    #[test]
    fn resources_read_through_the_host() {
        let mut host = FakeHost::default();
        host.notes.insert("daily/x.md".into(), "body".into());
        let response = pollster::block_on(dispatch(
            &mut host,
            call("resources/read", json!({"uri": INDEX_RESOURCE_URI})),
            "1.0",
        ))
        .unwrap();
        assert_eq!(
            response.result.unwrap()["contents"][0]["text"],
            "# Learned\n- one\n"
        );
        let response = pollster::block_on(dispatch(
            &mut host,
            call("resources/read", json!({"uri": "thock://note/daily/x.md"})),
            "1.0",
        ))
        .unwrap();
        assert_eq!(response.result.unwrap()["contents"][0]["text"], "body");
        let response = pollster::block_on(dispatch(
            &mut host,
            call("resources/read", json!({"uri": "file:///etc/passwd"})),
            "1.0",
        ))
        .unwrap();
        assert!(response.error.is_some());
    }

    #[test]
    fn the_cache_key_folds_case_and_filters() {
        let a = parse_search_query(&json!({"query": "Ana"})).unwrap();
        let b = parse_search_query(&json!({"query": "ana "})).unwrap();
        let c = parse_search_query(&json!({"query": "ana", "folder": "daily"})).unwrap();
        assert_eq!(a.cache_key(), b.cache_key());
        assert_ne!(a.cache_key(), c.cache_key());
    }
}
