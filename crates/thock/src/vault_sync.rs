//! Vault sync, the desk end (specs `v34-vault-sync.md` and
//! `v34-vault-sync-api.md`): uploads every allow-listed text file of the
//! vault to the Thock Plus backend as a blob encrypted with a key that exists
//! only on this machine and the paired phone, drains the phone's queued
//! writes, applies them with the shared rules and acks them. The backend sees
//! paths and ciphertext, never a note.
//!
//! Protocol types and the HTTP client live here beside the service so the
//! contract has one home in the desk; the application rules, hashing and the
//! envelope come from `thock_sync_core`, which the phone ships too.

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use chrono::Local;
use fs::{Fs, RemoveOptions};
use futures::{AsyncReadExt as _, StreamExt as _};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, Global, SharedString,
    Task, TaskExt as _,
};
use http_client::{AsyncBody, HttpClient, Request, Response, http};
use project::Project;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thock_sync_core::{Context as SealContext, Operation, Outcome, Write, apply, is_syncable_path};
use ui::IconName;
use workspace::Workspace;

use crate::calendar_service::{ManualSyncFinished, show_sync_toast};
use crate::history;
use crate::notes::{NoteKind, expand_template, parse_date};
use crate::plus;
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

/// Where the desk records what the server has (`v34-vault-sync.md` §6.4).
pub const STATE_FILE: &str = ".thock/sync/state.json";
/// Plaintext files above this never leave the desk (contract §3.5).
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const KEYCHAIN_URL: &str = "https://plus.thethock.com/vault-key";
const UPLOAD_DEBOUNCE: Duration = Duration::from_secs(2);
const RETRY_FLOOR: Duration = Duration::from_secs(5);
const RETRY_CEILING: Duration = Duration::from_secs(10 * 60);
const FEED_RECONNECT_FLOOR: Duration = Duration::from_secs(1);
const FEED_RECONNECT_CEILING: Duration = Duration::from_secs(60);
/// Directories the catch-up scan never enters: the history repository, the
/// caches and this service's own state.
const SKIPPED_DIRS: &[&str] = &[".git", ".thock/history", ".thock/cache", ".thock/sync"];

gpui::actions!(
    thock,
    [
        /// Shows a code to scan with Thock on your phone, so your notes are
        /// there too.
        ConnectPhone,
        /// Disconnects the phone that is paired with this vault. Your notes
        /// stay exactly as they are.
        DisconnectPhone,
        /// Stops keeping a copy of your notes for your phone and deletes that
        /// copy. Your notes on this computer are not affected.
        TurnVaultSyncOff,
        /// Checks for changes from your phone now and sends anything new.
        SyncVaultNow,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let service = cx.new(|cx| VaultSyncService::new(project.clone(), cx));
        cx.subscribe(&service, |workspace, _, event: &ManualSyncFinished, cx| {
            show_sync_toast(workspace, event, cx);
        })
        .detach();
        let project_id = project.entity_id();
        cx.default_global::<GlobalVaultSyncServices>()
            .0
            .insert(project_id, service);
        cx.on_release(move |_, cx| {
            cx.default_global::<GlobalVaultSyncServices>()
                .0
                .remove(&project_id);
        })
        .detach();

        workspace.register_action(|workspace, _: &ConnectPhone, window, cx| {
            let Some(service) = service_for_project(workspace.project(), cx) else {
                return;
            };
            if !service.read(cx).has_vault() {
                workspace.show_error(
                    "This workspace isn't a Thock vault, so there is nothing to send to a phone."
                        .to_string(),
                    cx,
                );
                return;
            }
            let service = service.downgrade();
            workspace.toggle_modal(window, cx, |window, cx| {
                crate::connect_phone::ConnectPhoneModal::new(service, window, cx)
            });
        });
        workspace.register_action(|workspace, _: &DisconnectPhone, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.disconnect_phone(cx));
            }
        });
        workspace.register_action(|workspace, _: &TurnVaultSyncOff, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.turn_off(cx));
            }
        });
        workspace.register_action(|workspace, _: &SyncVaultNow, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.sync_now(cx));
            }
        });
    })
    .detach();
}

#[derive(Default)]
struct GlobalVaultSyncServices(HashMap<EntityId, Entity<VaultSyncService>>);

impl Global for GlobalVaultSyncServices {}

/// The sync service for `project`, if one is running.
pub fn service_for_project(
    project: &Entity<Project>,
    cx: &App,
) -> Option<Entity<VaultSyncService>> {
    cx.try_global::<GlobalVaultSyncServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

// --- protocol types (contract §6) ---

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct VaultInfo {
    #[serde(default)]
    pub vault_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub key_check: String,
    #[serde(default)]
    pub quota_bytes: u64,
    #[serde(default)]
    pub used_bytes: u64,
    #[serde(default)]
    pub file_count: u64,
    #[serde(default)]
    pub latest_version: u64,
    #[serde(default)]
    pub writes: WritesInfo,
    #[serde(default)]
    pub devices: Vec<DeviceInfo>,
    #[serde(default)]
    pub lapsed_at: Option<String>,
}

impl VaultInfo {
    pub fn phone(&self) -> Option<&DeviceInfo> {
        self.devices.iter().find(|device| device.role == "phone")
    }

    pub fn is_lapsed(&self) -> bool {
        self.status == "lapsed" || self.lapsed_at.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct WritesInfo {
    #[serde(default)]
    pub pending: u64,
    #[serde(default)]
    pub latest_seq: u64,
    #[serde(default)]
    pub acked_through_seq: u64,
    #[serde(default)]
    pub acked_at_version: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub role: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub paired_at: String,
    #[serde(default)]
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UploadTicket {
    upload: UploadTarget,
}

#[derive(Debug, Clone, Deserialize)]
struct UploadTarget {
    url: String,
    #[serde(default = "default_put")]
    method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
}

fn default_put() -> String {
    "PUT".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct Versioned {
    version: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct WriteRow {
    seq: u64,
    client_id: String,
    path: String,
    payload: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WritesPage {
    #[serde(default)]
    writes: Vec<WriteRow>,
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Acked {
    through_seq: u64,
    #[serde(default)]
    at_version: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PairingCode {
    pub code: String,
    #[serde(default)]
    pub expires_at: String,
}

/// A refusal from the backend, carrying the contract's `code` so callers can
/// branch without parsing the sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncError {
    pub status: u16,
    pub code: String,
    pub message: String,
    /// Set on `stale_version`: the version the server holds now.
    pub current_version: Option<u64>,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SyncError {}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    current: Option<Versioned>,
}

fn sync_error(error: &anyhow::Error) -> Option<&SyncError> {
    error.downcast_ref::<SyncError>()
}

// --- the HTTP client (contract §3, §6) ---

#[derive(Clone)]
pub struct SyncApi {
    http: Arc<dyn HttpClient>,
    base_url: String,
    credential: String,
}

impl SyncApi {
    pub fn new(http: Arc<dyn HttpClient>, base_url: String, credential: String) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            credential,
        }
    }

    async fn send(
        &self,
        method: http::Method,
        route: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(http::StatusCode, String)> {
        let url = format!("{}{route}", self.base_url);
        let builder = Request::builder()
            .method(method.clone())
            .uri(&url)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", self.credential))
            .header(
                "Thock-Client",
                format!("desk/{}", env!("CARGO_PKG_VERSION")),
            );
        // A POST or DELETE without a body goes out as `{}` with an explicit
        // length: Google's front end answers 411 to a bodiless POST whose
        // length it cannot see.
        let request = if method == http::Method::GET {
            builder.body(AsyncBody::default())?
        } else {
            let bytes = body
                .map(|body| body.to_string())
                .unwrap_or_else(|| "{}".to_string())
                .into_bytes();
            builder
                .header("Content-Type", "application/json; charset=utf-8")
                .header("Content-Length", bytes.len().to_string())
                .body(AsyncBody::from(bytes))?
        };
        let mut response = self
            .http
            .send(request)
            .await
            .with_context(|| format!("reaching the Thock Plus service at {url}"))?;
        let mut text = String::new();
        response.body_mut().read_to_string(&mut text).await?;
        Ok((response.status(), text))
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: http::Method,
        route: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        let (status, text) = self.send(method, route, body).await?;
        if !status.is_success() {
            return Err(anyhow!(classify(status, &text)));
        }
        if text.trim().is_empty() {
            return serde_json::from_str("null")
                .with_context(|| format!("{route} answered with an empty body"));
        }
        serde_json::from_str(&text).with_context(|| format!("reading the answer from {route}"))
    }

    async fn call_empty(
        &self,
        method: http::Method,
        route: &str,
        body: Option<serde_json::Value>,
    ) -> Result<()> {
        let (status, text) = self.send(method, route, body).await?;
        if !status.is_success() {
            return Err(anyhow!(classify(status, &text)));
        }
        Ok(())
    }

    pub async fn ensure_vault(&self, device_name: &str, key_check: &str) -> Result<VaultInfo> {
        self.call(
            http::Method::POST,
            "/v1/vault",
            Some(serde_json::json!({ "device_name": device_name, "key_check": key_check })),
        )
        .await
    }

    pub async fn vault(&self) -> Result<VaultInfo> {
        self.call(http::Method::GET, "/v1/vault", None).await
    }

    pub async fn delete_vault(&self) -> Result<()> {
        self.call_empty(http::Method::DELETE, "/v1/vault", None)
            .await
    }

    pub async fn reset_vault(&self, key_check: &str) -> Result<VaultInfo> {
        self.call(
            http::Method::POST,
            "/v1/vault/reset",
            Some(serde_json::json!({ "key_check": key_check })),
        )
        .await
    }

    pub async fn create_pairing(&self) -> Result<PairingCode> {
        self.call(http::Method::POST, "/v1/vault/pairings", None)
            .await
    }

    pub async fn revoke_device(&self, device_id: &str) -> Result<()> {
        self.call_empty(
            http::Method::POST,
            &format!("/v1/vault/devices/{}/revoke", encode_segment(device_id)),
            None,
        )
        .await
    }

    async fn begin_upload(
        &self,
        path: &str,
        expected_version: u64,
        blob_id: &str,
        size_bytes: usize,
        content_hash: &str,
    ) -> Result<UploadTicket> {
        self.call(
            http::Method::POST,
            &format!("/v1/vault/files/{}", encode_path(path)),
            Some(serde_json::json!({
                "expected_version": expected_version,
                "blob_id": blob_id,
                "size_bytes": size_bytes,
                "content_hash": content_hash,
            })),
        )
        .await
    }

    async fn commit(&self, path: &str, expected_version: u64, blob_id: &str) -> Result<u64> {
        let committed: Versioned = self
            .call(
                http::Method::POST,
                &format!("/v1/vault/files/{}/commit", encode_path(path)),
                Some(serde_json::json!({
                    "expected_version": expected_version,
                    "blob_id": blob_id,
                })),
            )
            .await?;
        Ok(committed.version)
    }

    async fn tombstone(&self, path: &str, expected_version: u64) -> Result<u64> {
        let versioned: Versioned = self
            .call(
                http::Method::DELETE,
                &format!("/v1/vault/files/{}", encode_path(path)),
                Some(serde_json::json!({ "expected_version": expected_version })),
            )
            .await?;
        Ok(versioned.version)
    }

    async fn writes_after(&self, after: u64) -> Result<WritesPage> {
        self.call(
            http::Method::GET,
            &format!("/v1/vault/writes?after={after}&limit=1000"),
            None,
        )
        .await
    }

    async fn ack(&self, through_seq: u64) -> Result<Acked> {
        self.call(
            http::Method::POST,
            "/v1/vault/writes/ack",
            Some(serde_json::json!({ "through_seq": through_seq })),
        )
        .await
    }

    async fn put_blob(&self, target: &UploadTarget, bytes: Vec<u8>) -> Result<()> {
        let method = http::Method::from_bytes(target.method.as_bytes())
            .context("the upload method the service named isn't one this app knows")?;
        let mut builder = Request::builder()
            .method(method)
            .uri(&target.url)
            .header("Content-Length", bytes.len().to_string());
        for (name, value) in &target.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let request = builder.body(AsyncBody::from(bytes))?;
        let mut response = self
            .http
            .send(request)
            .await
            .context("sending a note to the Thock Plus service")?;
        if !response.status().is_success() {
            let mut text = String::new();
            response.body_mut().read_to_string(&mut text).await.ok();
            bail!(
                "the upload was refused with status {}: {}",
                response.status(),
                text.trim()
            );
        }
        Ok(())
    }

    async fn open_feed(&self) -> Result<Response<AsyncBody>> {
        let request = Request::builder()
            .method(http::Method::GET)
            .uri(format!("{}/v1/vault/feed", self.base_url))
            .header("Accept", "text/event-stream")
            .header("Authorization", format!("Bearer {}", self.credential))
            .header(
                "Thock-Client",
                format!("desk/{}", env!("CARGO_PKG_VERSION")),
            )
            .body(AsyncBody::default())?;
        let response = self.http.send(request).await?;
        if !response.status().is_success() {
            bail!("the change feed answered with status {}", response.status());
        }
        Ok(response)
    }
}

fn classify(status: http::StatusCode, body: &str) -> SyncError {
    let parsed: Option<ErrorBody> = serde_json::from_str(body).ok();
    let (message, code, current) = match parsed {
        Some(body) => (body.error, body.code, body.current.map(|c| c.version)),
        None => (String::new(), String::new(), None),
    };
    let message = if message.is_empty() {
        format!("The Thock Plus service answered with status {status}.")
    } else {
        message
    };
    let code = if code.is_empty() {
        match status {
            http::StatusCode::UNAUTHORIZED => "unauthorized".to_string(),
            http::StatusCode::FORBIDDEN => "revoked".to_string(),
            http::StatusCode::CONFLICT => "stale_version".to_string(),
            _ => format!("http_{}", status.as_u16()),
        }
    } else {
        code
    };
    SyncError {
        status: status.as_u16(),
        code,
        message,
        current_version: current,
    }
}

/// Percent-encodes one path segment (contract §4.1): unreserved characters
/// pass through, everything else is `%XX` per UTF-8 byte.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

// --- local state (`v34-vault-sync.md` §6.4) ---

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalState {
    #[serde(default = "default_schema")]
    pub schema: u32,
    /// The highest server version this desk has processed.
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub acked_through_seq: u64,
    #[serde(default)]
    pub files: BTreeMap<String, FileRecord>,
}

fn default_schema() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub version: u64,
    pub blob_id: String,
    /// Hex SHA-256 of the plaintext at upload time; what catch-up diffs
    /// against.
    pub plaintext_hash: String,
}

async fn load_state(fs: &Arc<dyn Fs>, root: &Path) -> LocalState {
    let path = root.join(STATE_FILE);
    match fs.load(&path).await {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
            log::warn!(
                "Thock: {} couldn't be read ({error}); rebuilding it from the vault",
                path.display()
            );
            LocalState::default()
        }),
        Err(_) => LocalState::default(),
    }
}

async fn save_state(fs: &Arc<dyn Fs>, root: &Path, state: &LocalState) -> Result<()> {
    let path = root.join(STATE_FILE);
    if let Some(parent) = path.parent() {
        fs.create_dir(parent).await?;
    }
    let text = serde_json::to_string_pretty(state)?;
    fs.atomic_write(path, text).await
}

fn plaintext_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn new_blob_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

// --- the vault key ---

/// The pairing URL the QR encodes (contract §5.3).
pub fn pairing_url(code: &str, key: &[u8; 32], backend_url: &str) -> String {
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
    format!(
        "thock://pair?v=1&code={}&key={key}&backend={}",
        encode_segment(code),
        encode_segment(backend_url)
    )
}

async fn read_vault_key(cx: &AsyncApp) -> Result<Option<[u8; 32]>> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    let Some((_, stored)) = provider.read_credentials(KEYCHAIN_URL, cx).await? else {
        return Ok(None);
    };
    let text = String::from_utf8(stored).context("the stored vault key is not readable")?;
    let bytes = hex::decode(text.trim()).context("the stored vault key is not readable")?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow!("the stored vault key has the wrong length"))?;
    Ok(Some(key))
}

async fn write_vault_key(key: &[u8; 32], cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    provider
        .write_credentials(
            KEYCHAIN_URL,
            &thock_sync_core::key_check(key),
            hex::encode(key).as_bytes(),
            cx,
        )
        .await
}

async fn delete_vault_key(cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    provider.delete_credentials(KEYCHAIN_URL, cx).await
}

// --- the service ---

/// What the status row shows.
#[derive(Debug, Clone, PartialEq)]
pub enum PhoneSyncState {
    /// Not a vault, or no Thock Plus credential on this machine: no row.
    Hidden,
    /// Plus is connected but no phone was ever paired here.
    Off,
    Starting,
    /// A key exists but the server lists no phone device.
    PhoneNotConnected,
    Working,
    UpToDate {
        at: Instant,
    },
    /// The Plus entitlement lapsed; the server keeps the copy for a while.
    Paused,
    Failing {
        error: SharedString,
    },
}

/// Everything a running sync needs, fixed for the session.
#[derive(Clone)]
struct Session {
    api: SyncApi,
    key: [u8; 32],
    key_check: String,
}

/// One pass of the worker: what it must look at.
#[derive(Default)]
struct Job {
    catch_up: bool,
    drain: bool,
    dirty: BTreeSet<String>,
    removed: BTreeSet<String>,
}

impl Job {
    fn is_empty(&self) -> bool {
        !self.catch_up && !self.drain && self.dirty.is_empty() && self.removed.is_empty()
    }
}

struct JobOutcome {
    state: LocalState,
    info: Option<VaultInfo>,
    applied: usize,
    uploaded: usize,
    skipped: Vec<SkippedFile>,
    held_back: Vec<String>,
    /// Phone writes that could not be read and were skipped, for a toast.
    refused: Vec<String>,
    /// The vault could not be read or written while applying a phone write;
    /// the batch stopped before it and is retried.
    blocked: Option<anyhow::Error>,
}

/// Why a phone write was not applied.
enum ApplyFailure {
    /// The write itself is unreadable or not one the desk accepts. Retrying
    /// cannot help, so it is skipped and acked, and the user is told.
    Refused(anyhow::Error),
    /// The vault file could not be read or written. The write is not acked,
    /// so it is applied on a later pass instead of being lost.
    Vault(anyhow::Error),
}

/// A file the desk is not sending, and why, for the status row.
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedFile {
    pub path: String,
    pub reason: SharedString,
}

/// The pairing QR's ingredients, handed to the modal.
#[derive(Debug, Clone, PartialEq)]
pub struct PairingInvite {
    pub url: String,
    pub expires_at: String,
    pub started_at: Instant,
}

pub struct VaultSyncService {
    project: Entity<Project>,
    vault: Option<Vault>,
    session: Option<Session>,
    state: LocalState,
    status: PhoneSyncState,
    info: Option<VaultInfo>,
    skipped: Vec<SkippedFile>,
    held_back: Vec<String>,
    pending: Job,
    worker_running: bool,
    retry_delay: Duration,
    /// Sync-wide announcements requested by `sync_now`.
    announce_next: bool,
    /// The keychain reads a reload starts; the newest wins.
    start_task: Option<Task<()>>,
    debounce_task: Option<Task<()>>,
    feed_task: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<ManualSyncFinished> for VaultSyncService {}

impl VaultSyncService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        let mut this = Self {
            project,
            vault: None,
            session: None,
            state: LocalState::default(),
            status: PhoneSyncState::Hidden,
            info: None,
            skipped: Vec::new(),
            held_back: Vec::new(),
            pending: Job::default(),
            worker_running: false,
            retry_delay: RETRY_FLOOR,
            announce_next: false,
            start_task: None,
            debounce_task: None,
            feed_task: None,
            _subscriptions: vec![project_subscription],
        };
        this.reload(cx);
        this
    }

    pub fn status(&self) -> &PhoneSyncState {
        &self.status
    }

    pub fn has_vault(&self) -> bool {
        self.vault.is_some()
    }

    /// The last vault summary the server gave, for the status row.
    pub fn info(&self) -> Option<&VaultInfo> {
        self.info.as_ref()
    }

    /// Files the desk is not sending (too large, not text).
    pub fn skipped(&self) -> &[SkippedFile] {
        &self.skipped
    }

    /// New `reference/` files held back because the vault is at its quota.
    pub fn held_back(&self) -> &[String] {
        &self.held_back
    }

    /// Whether a paired phone is on record at the server.
    pub fn phone(&self) -> Option<&DeviceInfo> {
        self.info.as_ref().and_then(|info| info.phone())
    }

    fn handle_project_event(
        &mut self,
        _: Entity<Project>,
        event: &project::Event,
        cx: &mut Context<Self>,
    ) {
        match event {
            project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_) => {
                self.reload(cx)
            }
            project::Event::WorktreeUpdatedEntries(_, changes) => {
                let vault_config = format!("{VAULT_MARKER_DIR}/{VAULT_CONFIG_FILE}");
                if changes
                    .iter()
                    .any(|(path, _, _)| path.as_unix_str() == vault_config)
                {
                    self.reload(cx);
                }
                if self.session.is_none() {
                    return;
                }
                let mut touched = false;
                for (path, _, change) in changes.iter() {
                    let path = path.as_unix_str();
                    if !is_syncable_path(path) {
                        continue;
                    }
                    touched = true;
                    match change {
                        project::PathChange::Removed => {
                            self.pending.dirty.remove(path);
                            self.pending.removed.insert(path.to_string());
                        }
                        _ => {
                            self.pending.removed.remove(path);
                            self.pending.dirty.insert(path.to_string());
                        }
                    }
                }
                if touched {
                    self.debounce(cx);
                }
            }
            _ => {}
        }
    }

    /// Re-resolves the vault, then the Plus credential and the vault key; a
    /// complete pair starts the session.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let vault = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .and_then(|root| match Vault::detect(&root) {
                VaultStatus::Valid(vault) => Some(vault),
                _ => None,
            });
        let Some(vault) = vault else {
            self.vault = None;
            self.stop_session(PhoneSyncState::Hidden);
            cx.notify();
            return;
        };
        let changed_root =
            self.vault.as_ref().map(|v| v.root.as_path()) != Some(vault.root.as_path());
        self.vault = Some(vault);
        if changed_root || self.session.is_none() {
            self.stop_session(PhoneSyncState::Starting);
            self.start(cx);
        }
        cx.notify();
    }

    fn stop_session(&mut self, status: PhoneSyncState) {
        self.session = None;
        self.feed_task = None;
        self.debounce_task = None;
        self.start_task = None;
        self.pending = Job::default();
        self.status = status;
    }

    /// Reads the keychain for the Plus credential and the vault key.
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let http = cx.http_client();
        let fs = self.project.read(cx).fs().clone();
        self.status = PhoneSyncState::Starting;
        self.start_task = Some(cx.spawn(async move |this, cx| {
            let credential = plus::read_credential(cx).await.ok().flatten();
            let key = read_vault_key(cx).await;
            let base_url = cx
                .background_spawn(async move { plus::backend_url() })
                .await;
            let state = load_state(&fs, &vault.root).await;
            this.update(cx, |service, cx| {
                service.state = state;
                let Some(credential) = credential else {
                    service.status = PhoneSyncState::Hidden;
                    cx.notify();
                    return;
                };
                match key {
                    Ok(Some(key)) => {
                        let api = SyncApi::new(http, base_url, credential);
                        service.start_session(api, key, cx);
                    }
                    Ok(None) => service.status = PhoneSyncState::Off,
                    Err(error) => {
                        log::warn!("Thock: couldn't read the vault key: {error:#}");
                        service.status = PhoneSyncState::Failing {
                            error: "the keychain couldn't be read".into(),
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn start_session(&mut self, api: SyncApi, key: [u8; 32], cx: &mut Context<Self>) {
        self.session = Some(Session {
            api,
            key_check: thock_sync_core::key_check(&key),
            key,
        });
        self.status = PhoneSyncState::Working;
        self.retry_delay = RETRY_FLOOR;
        self.pending.catch_up = true;
        self.pending.drain = true;
        self.schedule_work(cx);
        self.start_feed(cx);
        cx.notify();
    }

    /// `thock::SyncVaultNow`: a full catch-up and drain, announced.
    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() {
            self.start(cx);
            return;
        }
        self.announce_next = true;
        self.pending.catch_up = true;
        self.pending.drain = true;
        self.schedule_work(cx);
    }

    fn debounce(&mut self, cx: &mut Context<Self>) {
        self.debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UPLOAD_DEBOUNCE).await;
            this.update(cx, |service, cx| {
                service.debounce_task = None;
                service.schedule_work(cx);
            })
            .ok();
        }));
    }

    /// Starts the worker unless one is running; the worker picks the pending
    /// job up itself, so flags set while it runs are not lost.
    fn schedule_work(&mut self, cx: &mut Context<Self>) {
        if self.worker_running || self.session.is_none() {
            return;
        }
        self.worker_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some((job, session, vault, fs, state, info))) =
                    this.update(cx, |service, cx| {
                        if service.pending.is_empty() || service.session.is_none() {
                            service.worker_running = false;
                            cx.notify();
                            return None;
                        }
                        let job = std::mem::take(&mut service.pending);
                        service.status = PhoneSyncState::Working;
                        cx.notify();
                        Some((
                            job,
                            service.session.clone()?,
                            service.vault.clone()?,
                            service.project.read(cx).fs().clone(),
                            service.state.clone(),
                            service.info.clone(),
                        ))
                    })
                else {
                    break;
                };
                let outcome = run_job(&this, job, session, vault, fs, state, info, cx).await;
                let delay = this
                    .update(cx, |service, cx| service.finish_job(outcome, cx))
                    .unwrap_or(None);
                if let Some(delay) = delay {
                    cx.background_executor().timer(delay).await;
                }
            }
        })
        .detach();
    }

    /// Folds a worker pass into the status; returns a delay to wait before
    /// the next pass when the pass failed.
    fn finish_job(
        &mut self,
        outcome: Result<JobOutcome, (Job, anyhow::Error)>,
        cx: &mut Context<Self>,
    ) -> Option<Duration> {
        let announce = std::mem::take(&mut self.announce_next);
        let delay = match outcome {
            Ok(outcome) => {
                self.state = outcome.state;
                if outcome.info.is_some() {
                    self.info = outcome.info;
                }
                self.skipped = outcome.skipped;
                self.held_back = outcome.held_back;
                if outcome.blocked.is_none() {
                    self.retry_delay = RETRY_FLOOR;
                }
                self.status = match &self.info {
                    Some(info) if info.is_lapsed() => PhoneSyncState::Paused,
                    Some(info) if info.phone().is_none() => PhoneSyncState::PhoneNotConnected,
                    _ => PhoneSyncState::UpToDate { at: Instant::now() },
                };
                if !outcome.refused.is_empty() {
                    // Shown even on a background pass: the phone has already
                    // let go of these, so this is the only trace of them.
                    let paths = outcome.refused.join(", ");
                    cx.emit(ManualSyncFinished {
                        message: format!(
                            "A change from your phone couldn't be read and was skipped: {paths}"
                        )
                        .into(),
                        icon: IconName::Warning,
                    });
                }
                if let Some(error) = outcome.blocked {
                    log::warn!("Thock: vault sync stopped at a phone write: {error:#}");
                    self.pending.drain = true;
                    self.status = PhoneSyncState::Failing {
                        error: format!("{error:#}").into(),
                    };
                    if announce {
                        cx.emit(ManualSyncFinished {
                            message: format!("Phone sync stopped: {error:#}").into(),
                            icon: IconName::Warning,
                        });
                    }
                    let delay = self.retry_delay;
                    self.retry_delay = (self.retry_delay * 2).min(RETRY_CEILING);
                    cx.notify();
                    return Some(delay);
                }
                if announce {
                    let message = match (outcome.applied, outcome.uploaded) {
                        (0, 0) => "Phone synced, nothing new".to_string(),
                        (applied, 0) => {
                            format!("Phone synced: {applied} change(s) from your phone")
                        }
                        (0, uploaded) => format!("Phone synced: {uploaded} note(s) sent"),
                        (applied, uploaded) => format!(
                            "Phone synced: {applied} change(s) from your phone, {uploaded} note(s) sent"
                        ),
                    };
                    cx.emit(ManualSyncFinished {
                        message: message.into(),
                        icon: IconName::Check,
                    });
                }
                None
            }
            Err((job, error)) => {
                // Put the work back so the retry sees the same paths.
                self.pending.catch_up |= job.catch_up;
                self.pending.drain |= job.drain;
                self.pending.dirty.extend(job.dirty);
                self.pending.removed.extend(job.removed);
                let delay = self.retry_delay;
                self.retry_delay = (self.retry_delay * 2).min(RETRY_CEILING);
                match sync_error(&error).map(|e| e.code.as_str()) {
                    Some("plus_lapsed") => {
                        self.status = PhoneSyncState::Paused;
                        self.pending = Job::default();
                        None
                    }
                    Some("unauthorized") | Some("revoked") => {
                        log::warn!("Thock: the Plus credential no longer works: {error:#}");
                        self.stop_session(PhoneSyncState::Failing {
                            error: format!("{error:#}").into(),
                        });
                        None
                    }
                    Some("vault_missing") => {
                        // The server copy is gone (lapse sweeper, or turned
                        // off elsewhere); pairing again starts over.
                        self.stop_session(PhoneSyncState::Off);
                        None
                    }
                    _ => {
                        log::warn!("Thock: vault sync failed: {error:#}");
                        self.status = PhoneSyncState::Failing {
                            error: format!("{error:#}").into(),
                        };
                        if announce {
                            cx.emit(ManualSyncFinished {
                                message: format!("Phone sync failed: {error:#}").into(),
                                icon: IconName::Warning,
                            });
                        }
                        Some(delay)
                    }
                }
            }
        };
        cx.notify();
        delay
    }

    /// Holds the change feed open; a `write` event drains, a `vault` event
    /// pauses. Reconnects with backoff; polling on reconnect is what the
    /// worker's `drain` flag does.
    fn start_feed(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.feed_task = Some(cx.spawn(async move |this, cx| {
            let mut delay = FEED_RECONNECT_FLOOR;
            loop {
                let api = session.api.clone();
                let events = cx.background_spawn(async move { api.open_feed().await });
                match events.await {
                    Ok(response) => {
                        delay = FEED_RECONNECT_FLOOR;
                        let mut body = response.into_body();
                        let mut buffer = Vec::new();
                        let mut chunk = [0u8; 4096];
                        loop {
                            let read = match body.read(&mut chunk).await {
                                Ok(0) | Err(_) => break,
                                Ok(read) => read,
                            };
                            buffer.extend_from_slice(&chunk[..read]);
                            for event in drain_sse_events(&mut buffer) {
                                let keep_going = this
                                    .update(cx, |service, cx| service.handle_feed_event(event, cx))
                                    .unwrap_or(false);
                                if !keep_going {
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        log::debug!("Thock: the change feed dropped: {error:#}");
                    }
                }
                // Whatever we missed while disconnected is a drain away.
                if this
                    .update(cx, |service, cx| {
                        service.pending.drain = true;
                        service.schedule_work(cx);
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(delay).await;
                delay = (delay * 2).min(FEED_RECONNECT_CEILING);
            }
        }));
    }

    fn handle_feed_event(&mut self, event: FeedEvent, cx: &mut Context<Self>) -> bool {
        if self.session.is_none() {
            return false;
        }
        match event.kind.as_str() {
            "write" | "ack" => {
                self.pending.drain = true;
                self.schedule_work(cx);
            }
            "vault" => {
                let status = event
                    .data
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default();
                match status {
                    "lapsed" => {
                        self.status = PhoneSyncState::Paused;
                        cx.notify();
                    }
                    "deleted" => {
                        self.stop_session(PhoneSyncState::Off);
                        cx.notify();
                        return false;
                    }
                    _ => {}
                }
            }
            "device" => {
                self.pending.drain = true;
                self.schedule_work(cx);
            }
            _ => {}
        }
        true
    }

    /// `thock::ConnectPhone`: makes sure a key and a server vault exist, then
    /// mints a pairing code. The modal renders the returned URL as a QR.
    pub fn begin_pairing(&mut self, cx: &mut Context<Self>) -> Task<Result<PairingInvite>> {
        let http = cx.http_client();
        let device_name = plus::device_label();
        let existing = self.session.clone();
        cx.spawn(async move |this, cx| {
            let base_url = cx
                .background_spawn(async move { plus::backend_url() })
                .await;
            let (api, key, key_check) = match existing {
                Some(session) => (session.api, session.key, session.key_check),
                None => {
                    let credential = plus::read_credential(cx).await?.ok_or_else(|| {
                        anyhow!("Connect Thock Plus first; your phone's copy lives there.")
                    })?;
                    let key = match read_vault_key(cx).await? {
                        Some(key) => key,
                        None => {
                            let key: [u8; 32] = rand::random();
                            write_vault_key(&key, cx).await?;
                            key
                        }
                    };
                    let key_check = thock_sync_core::key_check(&key);
                    (
                        SyncApi::new(http, base_url.clone(), credential),
                        key,
                        key_check,
                    )
                }
            };
            let ensured = api.ensure_vault(&device_name, &key_check).await;
            match ensured {
                Ok(_) => {}
                Err(error) if sync_error(&error).is_some_and(|e| e.code == "key_mismatch") => {
                    // The server holds files under a key this machine lost;
                    // re-pairing rotates it (`v34-vault-sync.md` §7.1).
                    api.reset_vault(&key_check).await?;
                }
                Err(error) => return Err(error),
            }
            let pairing = api.create_pairing().await?;
            let url = pairing_url(&pairing.code, &key, &base_url);
            this.update(cx, |service, cx| {
                if service.session.is_none() {
                    service.start_session(api, key, cx);
                } else {
                    service.pending.catch_up = true;
                    service.schedule_work(cx);
                }
            })?;
            Ok(PairingInvite {
                url,
                expires_at: pairing.expires_at,
                started_at: Instant::now(),
            })
        })
    }

    /// Re-reads the vault summary (devices, quota); the pairing modal polls
    /// this to notice the phone arriving.
    pub fn refresh_info(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_spawn(async move { session.api.vault().await })
                .await;
            this.update(cx, |service, cx| {
                match fetched {
                    Ok(info) => {
                        let was_unpaired = service.phone().is_none();
                        let paused = info.is_lapsed();
                        service.info = Some(info);
                        if paused {
                            service.status = PhoneSyncState::Paused;
                        } else if was_unpaired && service.phone().is_some() {
                            // A phone just paired: send everything it may be
                            // missing and pick up what it already wrote.
                            service.status = PhoneSyncState::Working;
                            service.pending.catch_up = true;
                            service.pending.drain = true;
                            service.schedule_work(cx);
                        } else if matches!(service.status, PhoneSyncState::PhoneNotConnected)
                            && service.phone().is_some()
                        {
                            service.status = PhoneSyncState::UpToDate { at: Instant::now() };
                        }
                    }
                    Err(error) => log::debug!("Thock: couldn't read the vault summary: {error:#}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `thock::DisconnectPhone`: revokes the phone's credential at the
    /// server. The copy stays for the next pairing.
    fn disconnect_phone(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(phone) = self.phone().cloned() else {
            cx.emit(ManualSyncFinished {
                message: "No phone is connected to this vault".into(),
                icon: IconName::Info,
            });
            return;
        };
        cx.spawn(async move |this, cx| {
            let revoked = cx
                .background_spawn(async move { session.api.revoke_device(&phone.device_id).await })
                .await;
            this.update(cx, |service, cx| {
                match revoked {
                    Ok(()) => {
                        if let Some(info) = &mut service.info {
                            info.devices.retain(|device| device.role != "phone");
                        }
                        service.status = PhoneSyncState::PhoneNotConnected;
                        cx.emit(ManualSyncFinished {
                            message: "Phone disconnected".into(),
                            icon: IconName::Check,
                        });
                    }
                    Err(error) => cx.emit(ManualSyncFinished {
                        message: format!("Couldn't disconnect the phone: {error:#}").into(),
                        icon: IconName::Warning,
                    }),
                }
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
    }

    /// `thock::TurnVaultSyncOff`: deletes the server copy, forgets the key
    /// and this desk's record. Nothing in the vault changes.
    fn turn_off(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.project.read(cx).fs().clone();
        self.stop_session(PhoneSyncState::Off);
        self.info = None;
        self.state = LocalState::default();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result: Result<()> = async {
                session.api.delete_vault().await?;
                delete_vault_key(cx).await?;
                let state_path = vault.root.join(STATE_FILE);
                if fs.is_file(&state_path).await {
                    fs.remove_file(&state_path, RemoveOptions::default())
                        .await?;
                }
                Ok(())
            }
            .await;
            this.update(cx, |_, cx| {
                let event = match result {
                    Ok(()) => ManualSyncFinished {
                        message: "The copy kept for your phone was deleted".into(),
                        icon: IconName::Check,
                    },
                    Err(error) => ManualSyncFinished {
                        message: format!("Couldn't turn phone sync off: {error:#}").into(),
                        icon: IconName::Warning,
                    },
                };
                cx.emit(event);
            })
        })
        .detach_and_log_err(cx);
    }

    #[cfg(test)]
    fn configure_for_test(
        &mut self,
        vault: Vault,
        api: SyncApi,
        key: [u8; 32],
        cx: &mut Context<Self>,
    ) {
        self.vault = Some(vault);
        self.start_task = None;
        self.session = Some(Session {
            api,
            key_check: thock_sync_core::key_check(&key),
            key,
        });
        self.status = PhoneSyncState::Working;
        self.pending.catch_up = true;
        self.pending.drain = true;
        self.schedule_work(cx);
    }
}

// --- server-sent events ---

struct FeedEvent {
    kind: String,
    data: serde_json::Value,
}

/// Pulls every complete event (terminated by a blank line) out of `buffer`,
/// leaving a partial trailing event in place.
fn drain_sse_events(buffer: &mut Vec<u8>) -> Vec<FeedEvent> {
    let mut events = Vec::new();
    loop {
        let text = String::from_utf8_lossy(buffer);
        let Some(end) = text.find("\n\n") else {
            break;
        };
        let block = text[..end].to_string();
        let consumed = text[..end + 2].len();
        buffer.drain(..consumed);
        let mut kind = String::from("message");
        let mut data = String::new();
        for line in block.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(value) = line.strip_prefix("event:") {
                kind = value.trim().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.trim_start());
            }
        }
        if data.is_empty() {
            continue;
        }
        let data = serde_json::from_str(&data).unwrap_or(serde_json::Value::Null);
        events.push(FeedEvent { kind, data });
    }
    events
}

// --- the worker ---

#[allow(clippy::too_many_arguments)]
async fn run_job(
    this: &gpui::WeakEntity<VaultSyncService>,
    job: Job,
    session: Session,
    vault: Vault,
    fs: Arc<dyn Fs>,
    mut state: LocalState,
    info: Option<VaultInfo>,
    cx: &mut AsyncApp,
) -> Result<JobOutcome, (Job, anyhow::Error)> {
    let mut dirty = job.dirty.clone();
    let mut removed = job.removed.clone();
    let mut skipped = Vec::new();

    // Catch up with what changed while Thock was closed (`v34` §10.2).
    if job.catch_up {
        let scan = {
            let fs = fs.clone();
            let root = vault.root.clone();
            let known = state.files.clone();
            cx.background_spawn(async move { scan_vault(&fs, &root, &known).await })
        };
        match scan.await {
            Ok(result) => {
                dirty.extend(result.changed);
                removed.extend(result.removed);
                skipped = result.skipped;
            }
            Err(error) => return Err((job, error)),
        }
    }

    // Drain the phone's queue (`v34` §7.5 / contract §10.4).
    let mut applied = 0;
    let mut last_seq = None;
    let mut refused_writes = Vec::new();
    let mut blocked_write = None;
    if job.drain {
        let fetch = {
            let api = session.api.clone();
            let after = state.acked_through_seq;
            cx.background_spawn(async move { fetch_writes(&api, after).await })
        };
        let writes = match fetch.await {
            Ok(writes) => writes,
            Err(error) => return Err((job, error)),
        };
        if !writes.is_empty() {
            // One checkpoint per batch, so "undo what the phone did" is a
            // restore away.
            this.update(cx, |service, cx| {
                history::checkpoint_before_ai_write(&service.project, cx)
            })
            .ok();
            let apply_all = {
                let fs = fs.clone();
                let vault = vault.clone();
                let key = session.key;
                cx.background_spawn(async move {
                    let mut changed = BTreeSet::new();
                    let mut applied = 0;
                    let mut refused = Vec::new();
                    let mut blocked = None;
                    let mut last = None;
                    // Rows arrive in seq order; the ack covers only the rows
                    // handled, so a blocked write and those after it stay queued.
                    for row in &writes {
                        match apply_write(&fs, &vault, &key, row).await {
                            Ok(Some(path)) => {
                                changed.insert(path);
                                applied += 1;
                            }
                            Ok(None) => {}
                            Err(ApplyFailure::Refused(error)) => {
                                log::warn!(
                                    "Thock: skipping write {} for {}: {error:#}",
                                    row.seq,
                                    row.path
                                );
                                refused.push(row.path.clone());
                            }
                            Err(ApplyFailure::Vault(error)) => {
                                log::warn!(
                                    "Thock: couldn't apply write {} to {}: {error:#}",
                                    row.seq,
                                    row.path
                                );
                                blocked = Some(error.context(format!(
                                    "a change from your phone to {} couldn't be saved",
                                    row.path
                                )));
                                break;
                            }
                        }
                        last = Some(last.map_or(row.seq, |seq: u64| seq.max(row.seq)));
                    }
                    (changed, applied, last, refused, blocked)
                })
            };
            let (changed, count, last, refused, blocked) = apply_all.await;
            dirty.extend(changed);
            applied = count;
            last_seq = last;
            refused_writes = refused;
            blocked_write = blocked;
        }
    }

    // Upload what changed, tombstone what vanished.
    let upload = {
        let api = session.api.clone();
        let key = session.key;
        let fs = fs.clone();
        let root = vault.root.clone();
        let quota = info
            .as_ref()
            .map(|info| (info.quota_bytes, info.used_bytes))
            .unwrap_or((0, 0));
        cx.background_spawn(async move {
            let mut uploaded = 0;
            let mut held_back = Vec::new();
            let mut used = quota.1;
            for path in &dirty {
                let abs = root.join(path);
                let bytes = match fs.load_bytes(&abs).await {
                    Ok(bytes) => bytes,
                    // Changed and gone before we got to it: the Removed event
                    // will follow, or the next catch-up tombstones it.
                    Err(_) => continue,
                };
                if let Some(reason) = unsendable_reason(&bytes) {
                    skipped.push(SkippedFile {
                        path: path.clone(),
                        reason,
                    });
                    continue;
                }
                let hash = plaintext_hash(&bytes);
                let record = state.files.get(path).cloned();
                if record.as_ref().is_some_and(|r| r.plaintext_hash == hash) {
                    continue;
                }
                let is_new = record.is_none();
                if is_new && quota.0 > 0 && path.starts_with("reference/") {
                    let envelope_size = bytes.len() as u64 + 32;
                    if used + envelope_size > quota.0 {
                        held_back.push(path.clone());
                        continue;
                    }
                }
                let expected = record.as_ref().map(|r| r.version).unwrap_or(0);
                let record = upload_file(&api, &key, path, expected, &bytes, hash).await?;
                used += bytes.len() as u64 + 32;
                state.cursor = state.cursor.max(record.version);
                state.files.insert(path.clone(), record);
                uploaded += 1;
            }
            for path in &removed {
                let Some(record) = state.files.get(path).cloned() else {
                    continue;
                };
                let version = match api.tombstone(path, record.version).await {
                    Ok(version) => version,
                    Err(error) => match sync_error(&error) {
                        Some(e) if e.code == "stale_version" => {
                            let current = e.current_version.unwrap_or(record.version);
                            api.tombstone(path, current).await?
                        }
                        Some(e) if e.code == "not_found" => record.version,
                        _ => return Err(error),
                    },
                };
                state.cursor = state.cursor.max(version);
                state.files.remove(path);
            }
            Ok::<_, anyhow::Error>((state, uploaded, skipped, held_back))
        })
    };
    let (mut state, uploaded, skipped, held_back) = match upload.await {
        Ok(result) => result,
        Err(error) => return Err((job, error)),
    };

    // Ack only after the snapshots carrying the writes' effects are up.
    let finish = {
        let api = session.api.clone();
        cx.background_spawn(async move {
            if let Some(seq) = last_seq {
                let acked = api.ack(seq).await?;
                state.acked_through_seq = acked.through_seq;
                state.cursor = state.cursor.max(acked.at_version);
            }
            let info = api.vault().await?;
            Ok::<_, anyhow::Error>((state, info))
        })
    };
    let (state, info) = match finish.await {
        Ok(result) => result,
        Err(error) => return Err((job, error)),
    };
    if let Err(error) = save_state(&fs, &vault.root, &state).await {
        return Err((job, error));
    }
    Ok(JobOutcome {
        state,
        info: Some(info),
        applied,
        uploaded,
        skipped,
        held_back,
        refused: refused_writes,
        blocked: blocked_write,
    })
}

struct ScanResult {
    changed: BTreeSet<String>,
    removed: BTreeSet<String>,
    skipped: Vec<SkippedFile>,
}

/// Hashes every allow-listed file against the local record. Mtime is not
/// trusted (`v34` §10.2).
async fn scan_vault(
    fs: &Arc<dyn Fs>,
    root: &Path,
    known: &BTreeMap<String, FileRecord>,
) -> Result<ScanResult> {
    let mut changed = BTreeSet::new();
    let mut skipped = Vec::new();
    let mut seen = BTreeSet::new();
    // A folder that can't be listed says nothing about its files, so they
    // are not tombstoned on the strength of it.
    let mut unlisted = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(dir) = directories.pop() {
        let mut entries = match fs.read_dir(&dir).await {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!("Thock: couldn't list {}: {error:#}", dir.display());
                // The root, or a folder we can't name, covers everything.
                unlisted.push(match relative_path(root, &dir) {
                    Some(rel) if !rel.is_empty() => format!("{rel}/"),
                    _ => String::new(),
                });
                continue;
            }
        };
        while let Some(entry) = entries.next().await {
            let Ok(abs) = entry else {
                continue;
            };
            let Some(rel) = relative_path(root, &abs) else {
                continue;
            };
            let Some(metadata) = fs.metadata(&abs).await.ok().flatten() else {
                continue;
            };
            if metadata.is_dir {
                if !SKIPPED_DIRS.contains(&rel.as_str()) {
                    directories.push(abs);
                }
                continue;
            }
            if metadata.is_symlink || !is_syncable_path(&rel) {
                continue;
            }
            // Still present, so never tombstoned: a file that grew too large
            // stays on the phone as it was, as "not synced" (`v34` §6.1).
            seen.insert(rel.clone());
            if metadata.len > MAX_FILE_BYTES {
                skipped.push(SkippedFile {
                    path: rel,
                    reason: "too large to send (over 2 MB)".into(),
                });
                continue;
            }
            let Ok(bytes) = fs.load_bytes(&abs).await else {
                continue;
            };
            if let Some(reason) = unsendable_reason(&bytes) {
                skipped.push(SkippedFile { path: rel, reason });
                continue;
            }
            let hash = plaintext_hash(&bytes);
            if known
                .get(&rel)
                .is_none_or(|record| record.plaintext_hash != hash)
            {
                changed.insert(rel);
            }
        }
    }
    let removed = known
        .keys()
        .filter(|path| !seen.contains(*path))
        .filter(|path| !unlisted.iter().any(|prefix| path.starts_with(prefix.as_str())))
        .cloned()
        .collect();
    Ok(ScanResult {
        changed,
        removed,
        skipped,
    })
}

fn relative_path(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in rel.components() {
        parts.push(component.as_os_str().to_str()?.to_string());
    }
    Some(parts.join("/"))
}

fn unsendable_reason(bytes: &[u8]) -> Option<SharedString> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Some("too large to send (over 2 MB)".into());
    }
    if std::str::from_utf8(bytes).is_err() {
        return Some("not a text file".into());
    }
    None
}

async fn fetch_writes(api: &SyncApi, after: u64) -> Result<Vec<WriteRow>> {
    let mut writes = Vec::new();
    let mut after = after;
    loop {
        let page = api.writes_after(after).await?;
        let Some(last) = page.writes.last().map(|row| row.seq) else {
            break;
        };
        after = last;
        writes.extend(page.writes);
        if !page.has_more {
            break;
        }
    }
    writes.sort_by_key(|row| row.seq);
    Ok(writes)
}

/// Encrypts and publishes one file; a `409` means this desk's record was
/// behind, and the content on disk wins (contract §6.3).
async fn upload_file(
    api: &SyncApi,
    key: &[u8; 32],
    path: &str,
    expected_version: u64,
    bytes: &[u8],
    hash: String,
) -> Result<FileRecord> {
    let mut expected = expected_version;
    for attempt in 0..2 {
        let blob_id = new_blob_id();
        let envelope = thock_sync_core::seal(
            key,
            SealContext::File {
                path: path.to_string(),
                blob_id: blob_id.clone(),
            },
            bytes,
        );
        let content_hash = thock_sync_core::content_hash(&envelope);
        let begun = api
            .begin_upload(path, expected, &blob_id, envelope.len(), &content_hash)
            .await;
        let ticket = match begun {
            Ok(ticket) => ticket,
            Err(error) => match sync_error(&error) {
                Some(e) if e.code == "stale_version" && attempt == 0 => {
                    expected = e.current_version.unwrap_or(expected);
                    continue;
                }
                _ => return Err(error),
            },
        };
        api.put_blob(&ticket.upload, envelope).await?;
        match api.commit(path, expected, &blob_id).await {
            Ok(version) => {
                return Ok(FileRecord {
                    version,
                    blob_id,
                    plaintext_hash: hash,
                });
            }
            Err(error) => match sync_error(&error) {
                Some(e) if e.code == "stale_version" && attempt == 0 => {
                    expected = e.current_version.unwrap_or(expected);
                    continue;
                }
                _ => return Err(error),
            },
        }
    }
    bail!("the Thock Plus service kept refusing the version of {path}")
}

/// Applies one queued write to the file on disk. Returns the path when the
/// file changed, `None` for a no-op or a write that had to be skipped.
async fn apply_write(
    fs: &Arc<dyn Fs>,
    vault: &Vault,
    key: &[u8; 32],
    row: &WriteRow,
) -> Result<Option<String>, ApplyFailure> {
    let write = open_write(key, row).map_err(ApplyFailure::Refused)?;
    apply_opened_write(fs, vault, &write)
        .await
        .map_err(ApplyFailure::Vault)
}

fn open_write(key: &[u8; 32], row: &WriteRow) -> Result<Write> {
    let envelope = base64::engine::general_purpose::STANDARD
        .decode(row.payload.trim())
        .context("the write's payload isn't valid base64")?;
    let plaintext = thock_sync_core::open(
        key,
        SealContext::Write {
            client_id: row.client_id.clone(),
        },
        &envelope,
    )
    .map_err(|error| anyhow!("the write couldn't be decrypted: {error}"))?;
    let json = std::str::from_utf8(&plaintext).context("the write isn't UTF-8")?;
    let write = thock_sync_core::parse_write(json)
        .map_err(|error| anyhow!("the write isn't readable: {error}"))?;
    if write.path != row.path || write.client_id != row.client_id {
        bail!("the write names a different path or id than its row");
    }
    if !is_syncable_path(&write.path) {
        bail!("the write targets a path that doesn't sync");
    }
    Ok(write)
}

async fn apply_opened_write(
    fs: &Arc<dyn Fs>,
    vault: &Vault,
    write: &Write,
) -> Result<Option<String>> {
    let abs = vault.root.join(&write.path);
    let existing = if fs.is_file(&abs).await {
        Some(fs.load(&abs).await?)
    } else {
        None
    };
    let seed = if existing.is_none() && write.create_from_template() {
        template_seed(fs, vault, &write.path).await
    } else {
        None
    };
    let applied = apply(existing.as_deref(), write, seed.as_deref());
    if applied.outcome == Outcome::Noop || existing.as_deref() == Some(applied.text.as_str()) {
        return Ok(None);
    }
    if let Some(parent) = abs.parent() {
        fs.create_dir(parent).await?;
    }
    fs.atomic_write(abs, applied.text).await?;
    Ok(Some(write.path.clone()))
}

/// The expanded daily or weekly template for a note the phone asked to
/// create (`v34` §8.2 rule 1), or `None` when `path` is neither.
async fn template_seed(fs: &Arc<dyn Fs>, vault: &Vault, path: &str) -> Option<String> {
    let abs = vault.root.join(path);
    let (kind, date) = note_kind_and_date(vault, &abs)?;
    let template = fs.load(&vault.template_path(kind)).await.ok()?;
    let title = abs.file_stem()?.to_str()?.to_string();
    Some(expand_template(
        &template,
        date,
        Local::now().time(),
        &title,
    ))
}

fn note_kind_and_date(vault: &Vault, abs: &Path) -> Option<(NoteKind, chrono::NaiveDate)> {
    if let Some(date) = vault.daily_note_date(abs) {
        return Some((NoteKind::Daily, date));
    }
    let weekly_dir = vault.root.join(&vault.config.weekly.dir);
    let relative = abs.strip_prefix(&weekly_dir).ok()?.to_str()?;
    let stem = relative.strip_suffix(".md")?;
    parse_date(stem, &vault.config.weekly.filename).map(|date| (NoteKind::Weekly, date))
}

/// What `Write` needs to expose for the desk; kept as a trait so a core
/// rename is one edit here.
trait WriteExt {
    fn create_from_template(&self) -> bool;
}

impl WriteExt for Write {
    fn create_from_template(&self) -> bool {
        matches!(
            &self.operation,
            Operation::Append {
                create_from_template: true,
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use http_client::FakeHttpClient;
    use settings::SettingsStore;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use thock_sync_core::{Heading, Placement};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    fn test_vault() -> Vault {
        Vault {
            root: PathBuf::from("/vault"),
            config: crate::vault::VaultConfig::default(),
        }
    }

    const KEY: [u8; 32] = [7u8; 32];

    #[derive(Default)]
    struct StubFile {
        version: u64,
        blob_id: String,
        deleted: bool,
    }

    /// An in-memory backend implementing contract §6 well enough for the
    /// desk's flows: versions from one counter, `409` on a stale expected
    /// version, blobs behind opaque URLs, a write queue and its ack.
    #[derive(Default)]
    struct StubServer {
        next_version: u64,
        files: BTreeMap<String, StubFile>,
        blobs: HashMap<String, Vec<u8>>,
        pending_uploads: HashMap<String, (String, u64)>,
        writes: Vec<(u64, String, String, String)>,
        acked_through: u64,
        acked_at_version: u64,
        requests: Vec<String>,
        phone_paired: bool,
    }

    impl StubServer {
        fn queue_write(&mut self, path: &str, client_id: &str, write_json: &str) -> u64 {
            let envelope = thock_sync_core::seal(
                &KEY,
                SealContext::Write {
                    client_id: client_id.to_string(),
                },
                write_json.as_bytes(),
            );
            let seq = self.writes.len() as u64 + 1;
            self.writes.push((
                seq,
                client_id.to_string(),
                path.to_string(),
                base64::engine::general_purpose::STANDARD.encode(envelope),
            ));
            seq
        }

        fn plaintext(&self, path: &str) -> Option<String> {
            let file = self.files.get(path)?;
            let envelope = self.blobs.get(&file.blob_id)?;
            let bytes = thock_sync_core::open(
                &KEY,
                SealContext::File {
                    path: path.to_string(),
                    blob_id: file.blob_id.clone(),
                },
                envelope,
            )
            .ok()?;
            String::from_utf8(bytes).ok()
        }

        fn vault_json(&self) -> serde_json::Value {
            let mut devices =
                vec![serde_json::json!({"device_id": "d1", "role": "desk", "name": "desk"})];
            if self.phone_paired {
                devices
                    .push(serde_json::json!({"device_id": "p1", "role": "phone", "name": "phone"}));
            }
            serde_json::json!({
                "vault_id": "v1", "status": "active", "key_check": thock_sync_core::key_check(&KEY),
                "quota_bytes": 209715200, "used_bytes": 0, "file_count": self.files.len(),
                "latest_version": self.next_version,
                "writes": {"pending": self.writes.iter().filter(|w| w.0 > self.acked_through).count(),
                           "latest_seq": self.writes.len(), "acked_through_seq": self.acked_through,
                           "acked_at_version": self.acked_at_version},
                "devices": devices,
            })
        }

        fn handle(&mut self, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
            self.requests.push(format!("{method} {uri}"));
            let path_and_query = uri.trim_start_matches("http://stub");
            let (route, _query) = path_and_query
                .split_once('?')
                .unwrap_or((path_and_query, ""));
            let json = |value: serde_json::Value| (200u16, value.to_string().into_bytes());
            let error = |status: u16, code: &str, extra: serde_json::Value| {
                let mut body = serde_json::json!({"error": code, "code": code});
                if let (Some(obj), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
                    for (k, v) in extra {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                (status, body.to_string().into_bytes())
            };
            let body_json: serde_json::Value =
                serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
            match (method, route) {
                ("GET", "/v1/vault") => json(self.vault_json()),
                ("GET", "/v1/vault/writes") => {
                    let writes: Vec<_> = self
                        .writes
                        .iter()
                        .filter(|w| w.0 > self.acked_through)
                        .map(|(seq, client_id, path, payload)| {
                            serde_json::json!({"seq": seq, "client_id": client_id, "path": path,
                                "base_version": 0, "payload": payload, "created_at": ""})
                        })
                        .collect();
                    json(serde_json::json!({"writes": writes, "has_more": false}))
                }
                ("POST", "/v1/vault/writes/ack") => {
                    let through = body_json["through_seq"].as_u64().unwrap_or(0);
                    self.acked_through = self.acked_through.max(through);
                    self.acked_at_version = self.next_version;
                    json(
                        serde_json::json!({"through_seq": self.acked_through, "at_version": self.acked_at_version}),
                    )
                }
                ("PUT", blob) if blob.starts_with("/blobs/") => {
                    let id = blob.trim_start_matches("/blobs/").to_string();
                    self.blobs.insert(id, body.to_vec());
                    (200, Vec::new())
                }
                (_, route) if route.starts_with("/v1/vault/files/") => {
                    let rest = route.trim_start_matches("/v1/vault/files/");
                    let (path, commit) = match rest.strip_suffix("/commit") {
                        Some(path) => (path, true),
                        None => (rest, false),
                    };
                    let path = path.replace("%20", " ");
                    let expected = body_json["expected_version"].as_u64().unwrap_or(0);
                    let current = self.files.get(&path).map(|f| f.version).unwrap_or(0);
                    if expected != current {
                        return error(
                            409,
                            "stale_version",
                            serde_json::json!({"current": {"version": current}}),
                        );
                    }
                    match (method, commit) {
                        ("POST", false) => {
                            let blob_id = body_json["blob_id"].as_str().unwrap_or("").to_string();
                            let size = body_json["size_bytes"].as_u64().unwrap_or(0);
                            self.pending_uploads
                                .insert(blob_id.clone(), (path.clone(), size));
                            json(
                                serde_json::json!({"upload": {"url": format!("http://stub/blobs/{blob_id}"),
                                "method": "PUT", "headers": {"Content-Type": "application/octet-stream"}}}),
                            )
                        }
                        ("POST", true) => {
                            let blob_id = body_json["blob_id"].as_str().unwrap_or("").to_string();
                            let Some((_, size)) = self.pending_uploads.remove(&blob_id) else {
                                return error(422, "blob_missing", serde_json::Value::Null);
                            };
                            if self.blobs.get(&blob_id).map(|b| b.len() as u64) != Some(size) {
                                return error(422, "blob_missing", serde_json::Value::Null);
                            }
                            self.next_version += 1;
                            self.files.insert(
                                path,
                                StubFile {
                                    version: self.next_version,
                                    blob_id,
                                    deleted: false,
                                },
                            );
                            json(serde_json::json!({"version": self.next_version}))
                        }
                        ("DELETE", _) => {
                            self.next_version += 1;
                            if let Some(file) = self.files.get_mut(&path) {
                                file.version = self.next_version;
                                file.deleted = true;
                                file.blob_id.clear();
                            }
                            json(serde_json::json!({"version": self.next_version}))
                        }
                        _ => error(404, "not_found", serde_json::Value::Null),
                    }
                }
                ("GET", "/v1/vault/feed") => error(503, "unavailable", serde_json::Value::Null),
                _ => error(404, "not_found", serde_json::Value::Null),
            }
        }
    }

    fn stub_api(server: Arc<Mutex<StubServer>>) -> SyncApi {
        let http = FakeHttpClient::create(move |mut request| {
            let server = server.clone();
            async move {
                let method = request.method().to_string();
                let uri = request.uri().to_string();
                let mut body = Vec::new();
                request.body_mut().read_to_end(&mut body).await?;
                let (status, bytes) = server.lock().unwrap().handle(&method, &uri, &body);
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(bytes))
                    .unwrap())
            }
        });
        SyncApi::new(http, "http://stub".to_string(), "tpk_test".to_string())
    }

    async fn start_service(
        fs: &Arc<FakeFs>,
        server: Arc<Mutex<StubServer>>,
        cx: &mut TestAppContext,
    ) -> Entity<VaultSyncService> {
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = cx.new(|cx| VaultSyncService::new(project.clone(), cx));
        service.update(cx, |service, cx| {
            service.configure_for_test(test_vault(), stub_api(server), KEY, cx)
        });
        cx.run_until_parked();
        service
    }

    fn write_json(client_id: &str, path: &str, heading: &str, line: &str) -> String {
        let write = Write {
            v: 1,
            client_id: client_id.to_string(),
            path: path.to_string(),
            made_at: "2026-10-02T13:58:02Z".to_string(),
            device_id: "p1".to_string(),
            operation: Operation::Append {
                heading: Some(Heading {
                    text: heading.to_string(),
                    level: 2,
                    ordinal: 0,
                }),
                lines: vec![line.to_string()],
                placement: Placement::End,
                blank_line_before: false,
                create_from_template: false,
            },
        };
        write.to_json()
    }

    #[gpui::test]
    async fn catch_up_uploads_the_vault_and_skips_what_cannot_be_sent(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            serde_json::json!({
                ".thock": {"config.toml": "", "history": {"HEAD": "ref"}},
                "daily": {"2026-10-02.md": "# Today\n\n## Day planner\n- [ ] Walk\n"},
                "backlog.md": "## Soon\n- [ ] Call Ana\n",
                "photo.png": "not text",
                "notes.txt": "plain\n",
            }),
        )
        .await;
        let server = Arc::new(Mutex::new(StubServer::default()));
        let service = start_service(&fs, server.clone(), cx).await;

        {
            let server = server.lock().unwrap();
            let mut paths: Vec<_> = server.files.keys().cloned().collect();
            paths.sort();
            assert_eq!(
                paths,
                vec![
                    ".thock/config.toml",
                    "backlog.md",
                    "daily/2026-10-02.md",
                    "notes.txt"
                ]
            );
            assert_eq!(
                server.plaintext("daily/2026-10-02.md").as_deref(),
                Some("# Today\n\n## Day planner\n- [ ] Walk\n")
            );
            assert!(!server.requests.iter().any(|r| r.contains("history")));
        }

        service.read_with(cx, |service, _| {
            assert!(matches!(
                service.status(),
                PhoneSyncState::PhoneNotConnected
            ));
            assert_eq!(service.state.files.len(), 4);
        });
        let state: LocalState = serde_json::from_str(
            &fs.load(Path::new("/vault/.thock/sync/state.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(state.files.len(), 4);
    }

    #[gpui::test]
    async fn drain_applies_writes_in_order_uploads_then_acks(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            serde_json::json!({
                ".thock": {"config.toml": ""},
                "daily": {"2026-10-02.md": "# Today\n\n## Day planner\n- [ ] Walk\n"},
            }),
        )
        .await;
        let server = Arc::new(Mutex::new(StubServer::default()));
        {
            let mut server = server.lock().unwrap();
            server.phone_paired = true;
            server.queue_write(
                "daily/2026-10-02.md",
                "c1",
                &write_json(
                    "c1",
                    "daily/2026-10-02.md",
                    "Day planner",
                    "- [ ] Buy a card",
                ),
            );
            server.queue_write(
                "daily/2026-10-02.md",
                "c2",
                &write_json("c2", "daily/2026-10-02.md", "Day planner", "- [ ] Call Ana"),
            );
        }
        let service = start_service(&fs, server.clone(), cx).await;

        let note = fs
            .load(Path::new("/vault/daily/2026-10-02.md"))
            .await
            .unwrap();
        assert_eq!(
            note,
            "# Today\n\n## Day planner\n- [ ] Walk\n- [ ] Buy a card\n- [ ] Call Ana\n"
        );
        let server = server.lock().unwrap();
        assert_eq!(server.acked_through, 2);
        assert_eq!(
            server.plaintext("daily/2026-10-02.md").as_deref(),
            Some(note.as_str())
        );
        let ack_index = server
            .requests
            .iter()
            .position(|r| r.contains("/writes/ack"))
            .expect("acked");
        let last_commit = server
            .requests
            .iter()
            .rposition(|r| r.contains("/commit"))
            .expect("committed");
        assert!(last_commit < ack_index, "{:?}", server.requests);
        drop(server);
        service.read_with(cx, |service, _| {
            assert!(matches!(service.status(), PhoneSyncState::UpToDate { .. }));
            assert_eq!(service.state.acked_through_seq, 2);
        });
    }

    #[gpui::test]
    async fn a_stale_version_is_retried_once_with_the_current_one(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            serde_json::json!({
                ".thock": {"config.toml": ""},
                "backlog.md": "## Soon\n- [ ] Call Ana\n",
            }),
        )
        .await;
        let server = Arc::new(Mutex::new(StubServer::default()));
        {
            // The server already holds a version this desk never recorded.
            let mut server = server.lock().unwrap();
            server.next_version = 5;
            server.files.insert(
                "backlog.md".to_string(),
                StubFile {
                    version: 5,
                    blob_id: "old".to_string(),
                    deleted: false,
                },
            );
        }
        let _service = start_service(&fs, server.clone(), cx).await;
        let server = server.lock().unwrap();
        assert_eq!(
            server.plaintext("backlog.md").as_deref(),
            Some("## Soon\n- [ ] Call Ana\n")
        );
        let conflicts = server
            .requests
            .iter()
            .filter(|r| r.contains("/v1/vault/files/backlog.md"))
            .count();
        assert_eq!(
            conflicts, 3,
            "begin (409), begin, commit: {:?}",
            server.requests
        );
    }

    #[gpui::test]
    async fn an_edit_is_uploaded_after_the_debounce_and_a_delete_is_a_tombstone(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            serde_json::json!({
                ".thock": {"config.toml": ""},
                "backlog.md": "## Soon\n",
                "notes.txt": "gone soon\n",
            }),
        )
        .await;
        let server = Arc::new(Mutex::new(StubServer::default()));
        let service = start_service(&fs, server.clone(), cx).await;

        fs.save(
            Path::new("/vault/backlog.md"),
            &"## Soon\n- [ ] New\n".into(),
            text::LineEnding::Unix,
        )
        .await
        .unwrap();
        fs.remove_file(Path::new("/vault/notes.txt"), RemoveOptions::default())
            .await
            .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(UPLOAD_DEBOUNCE * 2);
        cx.run_until_parked();

        let server = server.lock().unwrap();
        assert_eq!(
            server.plaintext("backlog.md").as_deref(),
            Some("## Soon\n- [ ] New\n")
        );
        assert!(server.files.get("notes.txt").is_some_and(|f| f.deleted));
        drop(server);
        service.read_with(cx, |service, _| {
            assert!(!service.state.files.contains_key("notes.txt"));
        });
    }

    #[gpui::test]
    async fn bodiless_posts_carry_an_empty_json_body_with_a_length(cx: &mut TestAppContext) {
        init_test(cx);
        let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
        let http = FakeHttpClient::create({
            let seen = seen.clone();
            move |mut request| {
                let seen = seen.clone();
                async move {
                    let method = request.method().to_string();
                    let length = request
                        .headers()
                        .get("Content-Length")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let mut body = Vec::new();
                    request.body_mut().read_to_end(&mut body).await?;
                    seen.lock().unwrap().push((
                        method,
                        length,
                        String::from_utf8_lossy(&body).into_owned(),
                    ));
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(
                            r#"{"code": "K7MP-4QZX", "expires_at": ""}"#.as_bytes().to_vec(),
                        ))
                        .unwrap())
                }
            }
        });
        let api = SyncApi::new(http, "http://stub".to_string(), "tpk_test".to_string());
        cx.executor()
            .spawn(async move { api.create_pairing().await })
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            &[("POST".to_string(), "2".to_string(), "{}".to_string())]
        );
    }

    #[test]
    fn sse_events_are_split_on_blank_lines() {
        let mut buffer = b"event: write\ndata: {\"seq\":58}\n\n: ping\n\nevent: ack\ndata: {\"through_seq\":58,\n".to_vec();
        let events = drain_sse_events(&mut buffer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "write");
        assert_eq!(events[0].data["seq"], 58);
        assert_eq!(buffer, b"event: ack\ndata: {\"through_seq\":58,\n".to_vec());
    }

    #[test]
    fn paths_are_percent_encoded_per_segment() {
        assert_eq!(
            encode_path("daily/2026-10-02 notes.md"),
            "daily/2026-10-02%20notes.md"
        );
        assert_eq!(
            encode_path("reference/clips/café.md"),
            "reference/clips/caf%C3%A9.md"
        );
    }

    #[test]
    fn the_pairing_url_carries_code_key_and_backend() {
        let url = pairing_url("K7MP-4QZX", &KEY, "https://plus.thethock.com");
        assert!(url.starts_with("thock://pair?v=1&code=K7MP-4QZX&key="));
        assert!(url.ends_with("&backend=https%3A%2F%2Fplus.thethock.com"));
        let key = url.split("key=").nth(1).unwrap().split('&').next().unwrap();
        assert_eq!(key.len(), 43);
    }
}
