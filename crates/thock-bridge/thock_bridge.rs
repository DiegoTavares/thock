//! The contract between the Thock desk and the agents on a person's other
//! machines (spec `thock/specs/v55-agents-bridge.md`): the MCP framing and
//! the six tools, the rules every write obeys (paths, filters, the inbox
//! line, the daily section), the keyword search, and the client's cache,
//! outbox and mirror formats. No GPUI, no `fs` crate: pure functions over
//! strings plus a `ToolHost` the desk implements over its own file system.

mod http;
mod mirror;
mod offline;
mod rpc;
mod rules;
mod search;

pub use http::{
    HttpRequest, HttpResponse, MAX_HEAD_BYTES, RequestHead, Route, bearer_token, parse_route,
    percent_decode, percent_encode, read_body, read_request_head, request, response_json,
    write_response,
};
pub use mirror::{MIRROR_MAX_BYTES, MirrorFile, MirrorState, is_mirrored_path, plan_mirror};
pub use offline::{
    Cache, CachedText, DrainFailure, DrainReport, DrainStop, Outbox, OutboxEntry, offline_suffix,
};
pub use rpc::{
    CALL_DATE_WINDOW_DAYS, CallMeta, CallToolResponse, HostError, INDEX_RESOURCE_URI, INVALID_PARAMS, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND, NOTE_RESOURCE_PREFIX, PROTOCOL_VERSION,
    QUEUED_SENTENCE, ReadTarget, RecentNote, Resource, ResourceContents, SearchHit, SearchQuery,
    Tool, ToolHost, ToolResponseContent, WriteOutcome, accepted_call_date, dispatch, error_response,
    initialize_result,
    render_hits, render_recent, resources, result_response, tools, validate_tool_arguments,
};
pub use rules::{
    FACT_MAX_CHARS, FilterRefusal, INBOX_PATH, InboxLine, PathError, Refusal, check_fact,
    find_unkeepable,
    forbidden_topics, inbox_line, inbox_write, parse_inbox_line, parse_today_line,
    sanitize_harness, today_line, today_write, validate_vault_path,
};
pub use search::{IndexEntry, LinkResolution, SearchIndex, note_title};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Where the desk serves by default; `bridge.toml` overrides it.
pub const DEFAULT_PORT: u16 = 7557;
pub const DEFAULT_BIND: &str = "0.0.0.0";
/// The daily section `note_today` appends under, unless `bridge.toml` says
/// otherwise (decision 12).
pub const DEFAULT_TODAY_HEADING: &str = "From your agents";
/// How long a pairing code stays valid (§7).
pub const PAIRING_CODE_MINUTES: i64 = 10;
/// How long the desk remembers a call id (§9.5).
pub const SEEN_CALL_HOURS: i64 = 24;
/// The header the client sends so the desk can name the harness behind a
/// paired machine (`via claude-code@robin`).
pub const HARNESS_HEADER: &str = "X-Thock-Harness";
pub const PROTOCOL_HEADER: &str = "MCP-Protocol-Version";
/// The skill the desk serves at `GET /v1/skill`, vault-relative.
pub const SKILL_PATH: &str = "routines/agents/skills/thock-memory/SKILL.md";
/// Where mirrored harness memory lands, vault-relative.
pub const AGENTS_DIR: &str = "memory/agents";

/// Who an agent is to the vault: the harness it runs in and the paired
/// machine it runs on, rendered `claude-code@robin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentity {
    pub harness: String,
    pub host: String,
}

impl ClientIdentity {
    pub fn new(harness: &str, host: &str) -> Self {
        Self {
            harness: sanitize_harness(harness),
            host: host.to_string(),
        }
    }

    pub fn label(&self) -> String {
        format!("{}@{}", self.harness, self.host)
    }
}

/// The desk's `bridge.toml` (§6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_today_heading")]
    pub today_heading: String,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_bind() -> String {
    DEFAULT_BIND.to_string()
}

fn default_today_heading() -> String {
    DEFAULT_TODAY_HEADING.to_string()
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            bind: DEFAULT_BIND.to_string(),
            today_heading: DEFAULT_TODAY_HEADING.to_string(),
        }
    }
}

impl BridgeConfig {
    pub fn parse(toml_text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_text)
    }

    /// The file as the desk writes it on activation.
    pub fn render(&self) -> String {
        format!(
            "# The Agents Routine's server (V55). The desk serves while this vault is open.\n\
             port = {}\nbind = \"{}\"\n# The daily-note section `note_today` appends under.\n\
             today_heading = \"{}\"\n",
            self.port, self.bind, self.today_heading
        )
    }
}

/// One paired machine as `clients.toml` records it (§6.1): the token is
/// kept as its SHA-256 only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedClient {
    pub name: String,
    pub token_sha256: String,
    pub paired_at: String,
    #[serde(default)]
    pub last_seen: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ClientsFile {
    #[serde(default)]
    client: Vec<PairedClient>,
}

pub fn parse_clients(toml_text: &str) -> Result<Vec<PairedClient>, toml::de::Error> {
    toml::from_str::<ClientsFile>(toml_text).map(|file| file.client)
}

pub fn render_clients(clients: &[PairedClient]) -> String {
    let file = ClientsFile {
        client: clients.to_vec(),
    };
    toml::to_string(&file).unwrap_or_default()
}

/// The live pairing code as `pending.toml` records it: hash and expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCode {
    pub code_sha256: String,
    pub expires_at: String,
}

impl PendingCode {
    pub fn parse(toml_text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_text)
    }

    pub fn render(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    pub fn is_expired(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|expires| expires.with_timezone(&chrono::Utc) <= now)
            .unwrap_or(true)
    }

    pub fn matches(&self, code: &str) -> bool {
        constant_time_eq(&self.code_sha256, &sha256_hex(code.trim()))
    }
}

/// The client's `bridge.toml` (§6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub desk: String,
    pub token: String,
    #[serde(default)]
    pub vault: String,
    #[serde(default)]
    pub client_name: String,
}

impl ClientConfig {
    pub fn parse(toml_text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_text)
    }

    pub fn render(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }
}

/// What `POST /v1/pair` carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResponse {
    pub token: String,
    pub vault: String,
    pub desk: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub vault: String,
    pub desk: String,
    pub version: String,
    pub serving_since: String,
}

pub fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Equal without an early exit, so a token's hash can't be guessed one
/// byte at a time from the desk's timing.
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// A 32-byte random token, hex (§7).
pub fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

/// A six-word pairing code a person can type on another machine.
pub fn generate_code() -> String {
    use rand::seq::IndexedRandom as _;
    let mut rng = rand::rng();
    (0..6)
        .filter_map(|_| CODE_WORDS.choose(&mut rng))
        .copied()
        .collect::<Vec<_>>()
        .join("-")
}

/// Short, unambiguous, easy to say over a shoulder.
const CODE_WORDS: &[&str] = &[
    "acorn", "amber", "anvil", "apple", "arrow", "aspen", "atlas", "badge", "bagel", "basil",
    "beach", "berry", "birch", "blaze", "bloom", "brick", "brook", "cabin", "candy", "canoe",
    "cedar", "chalk", "cider", "cliff", "cloud", "clover", "cobalt", "comet", "coral", "crane",
    "creek", "crisp", "daisy", "delta", "denim", "dune", "eagle", "ember", "fable", "falcon",
    "fern", "field", "flame", "flint", "forest", "fox", "frost", "garden", "glade", "globe",
    "goose", "grape", "grove", "harbor", "hazel", "heron", "hill", "honey", "iris", "island",
    "ivory", "jade", "juniper", "kettle", "lagoon", "lamp", "lantern", "laurel", "leaf", "lemon",
    "lilac", "linen", "lotus", "lunar", "maple", "marble", "meadow", "mint", "moss", "nectar",
    "north", "oak", "ocean", "olive", "onyx", "orbit", "orchid", "otter", "paper", "pearl",
    "pebble", "pepper", "petal", "pine", "plum", "pond", "poppy", "prairie", "quartz", "quill",
    "raven", "reed", "ridge", "river", "robin", "rose", "saffron", "sage", "sand", "shore",
    "silver", "sky", "slate", "snow", "sparrow", "spring", "spruce", "stone", "storm", "summit",
    "sun", "swan", "thyme", "tide", "timber", "topaz", "trail", "tulip", "valley", "velvet",
    "violet", "walnut", "wave", "wheat", "willow", "wind", "wren", "yarrow", "zephyr", "zinc",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_config_round_trips_with_defaults() {
        let config = BridgeConfig::parse("port = 7557\nbind = \"0.0.0.0\"\n").unwrap();
        assert_eq!(config, BridgeConfig::default());
        let rendered = config.render();
        assert_eq!(BridgeConfig::parse(&rendered).unwrap(), config);
        let custom = BridgeConfig::parse("port = 8000\ntoday_heading = \"Agentes\"\n").unwrap();
        assert_eq!(custom.port, 8000);
        assert_eq!(custom.bind, DEFAULT_BIND);
        assert_eq!(custom.today_heading, "Agentes");
    }

    #[test]
    fn clients_file_round_trips() {
        let clients = vec![PairedClient {
            name: "robin".into(),
            token_sha256: sha256_hex("secret"),
            paired_at: "2026-11-04T08:00:00Z".into(),
            last_seen: String::new(),
        }];
        let rendered = render_clients(&clients);
        assert!(rendered.contains("[[client]]"));
        assert_eq!(parse_clients(&rendered).unwrap(), clients);
        assert!(parse_clients("").unwrap().is_empty());
    }

    #[test]
    fn pending_code_checks_hash_and_expiry() {
        let pending = PendingCode {
            code_sha256: sha256_hex("maple-river"),
            expires_at: "2026-11-04T08:10:00Z".into(),
        };
        assert!(pending.matches(" maple-river\n"));
        assert!(!pending.matches("maple-rivet"));
        let before = chrono::DateTime::parse_from_rfc3339("2026-11-04T08:09:59Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let after = before + chrono::Duration::seconds(1);
        assert!(!pending.is_expired(before));
        assert!(pending.is_expired(after));
        assert_eq!(PendingCode::parse(&pending.render()).unwrap(), pending);
    }

    #[test]
    fn tokens_and_codes_have_the_documented_shape() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        let code = generate_code();
        assert_eq!(code.split('-').count(), 6);
        assert!(code.split('-').all(|word| CODE_WORDS.contains(&word)));
        assert_ne!(generate_token(), token);
    }

    #[test]
    fn constant_time_eq_compares_whole_strings() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }

    #[test]
    fn identity_sanitizes_the_harness() {
        let identity = ClientIdentity::new("Claude Code!", "robin");
        assert_eq!(identity.label(), "claude-code@robin");
    }
}
