//! The Agents Routine's server inside the app (spec `v55-agents-bridge.md`
//! §11): while a vault with the Routine is open, the desk serves the vault's
//! memory over a small local HTTP endpoint that the `thock-bridge` client on
//! the person's other machines speaks to. The contract — routes, tools,
//! filters, lines — is `thock_bridge`; this module binds the port, checks
//! tokens, and implements the tools over the project `Fs`.

use anyhow::{Context as _, Result, anyhow};
use chrono::{Local, NaiveDate, Utc};
use fs::Fs;
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EntityId, Global, Task, TaskExt as _};
use project::Project;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thock_bridge::{
    BridgeConfig, CallMeta, ClientIdentity, HostError, JsonRpcRequest, PairRequest, PairResponse,
    PairedClient, PendingCode, ReadTarget, RecentNote, Route, SearchHit, SearchIndex, SearchQuery,
    ToolHost, WriteOutcome,
};
use thock_sync_core::{Outcome, Write, apply};
use util::ResultExt as _;
use workspace::Workspace;

use crate::calendar_service::{ManualSyncFinished, show_sync_toast};
use crate::history;
use crate::notes::{NoteKind, expand_template};
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

pub const AGENTS_ROUTINE_ID: &str = "agents";
/// Port and bind address, created on activation (§6.1).
pub const BRIDGE_CONFIG_PATH: &str = ".thock/routines/agents/bridge.toml";
pub const CLIENTS_PATH: &str = ".thock/state/bridge/clients.toml";
pub const PENDING_PATH: &str = ".thock/state/bridge/pending.toml";
pub const SEEN_PATH: &str = ".thock/state/bridge/seen.jsonl";
const PROFILE_PATH: &str = "profile.md";
/// What one request may carry; a mirrored file is capped lower (§9.6).
const BODY_LIMIT: usize = 2 * 1024 * 1024;
const RECV_TIMEOUT: Duration = Duration::from_millis(250);
/// The client id the bridge's sync-core writes carry.
const WRITE_CLIENT_ID: &str = "thock-bridge";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

gpui::actions!(
    thock,
    [
        /// Shows a pairing code so an agent on another computer can read what
        /// this vault knows and leave notes in it.
        ConnectOtherAgent,
        /// Disconnects one of the computers paired with this vault. It can
        /// pair again any time.
        DisconnectAgent,
        /// Pauses or resumes sharing this vault's memory with your other
        /// agents, without removing the Agents Routine.
        ToggleBridge,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let service = cx.new(|cx| BridgeService::new(project.clone(), cx));
        cx.subscribe(&service, |workspace, _, event: &ManualSyncFinished, cx| {
            show_sync_toast(workspace, event, cx);
        })
        .detach();
        let project_id = project.entity_id();
        cx.default_global::<GlobalBridgeServices>()
            .0
            .insert(project_id, service);
        cx.on_release(move |_, cx| {
            cx.default_global::<GlobalBridgeServices>()
                .0
                .remove(&project_id);
        })
        .detach();

        workspace.register_action(|workspace, _: &ConnectOtherAgent, window, cx| {
            let Some(service) = service_for_project(workspace.project(), cx) else {
                return;
            };
            if !service.read(cx).is_enabled() {
                workspace.show_error(
                    "Add the Agents Routine first; it is what lets other agents reach this vault."
                        .to_string(),
                    cx,
                );
                return;
            }
            let service = service.downgrade();
            workspace.toggle_modal(window, cx, |window, cx| {
                crate::connect_agent::ConnectAgentModal::new(service, window, cx)
            });
        });
        workspace.register_action(|workspace, _: &DisconnectAgent, window, cx| {
            let Some(service) = service_for_project(workspace.project(), cx) else {
                return;
            };
            let clients = service.read(cx).clients();
            if clients.is_empty() {
                workspace.show_error("No agent is connected to this vault.".to_string(), cx);
                return;
            }
            let service = service.downgrade();
            workspace.toggle_modal(window, cx, |window, cx| {
                crate::connect_agent::DisconnectAgentPicker::new(service, clients, window, cx)
            });
        });
        workspace.register_action(|workspace, _: &ToggleBridge, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.toggle(cx));
            }
        });
    })
    .detach();
}

#[derive(Default)]
struct GlobalBridgeServices(HashMap<EntityId, Entity<BridgeService>>);

impl Global for GlobalBridgeServices {}

/// The bridge service for `project`, if one is running.
pub fn service_for_project(project: &Entity<Project>, cx: &App) -> Option<Entity<BridgeService>> {
    cx.try_global::<GlobalBridgeServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

/// What the Routines panel's status row shows (§11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeState {
    /// The Routine is not installed, or the workspace is not a vault.
    Off,
    /// `thock::ToggleBridge` stopped it; the Routine stays.
    Paused,
    Starting,
    Serving { address: String, port: u16 },
    Failed { port: u16, error: String },
}

impl BridgeState {
    /// The row's words.
    pub fn label(&self, client_count: usize) -> String {
        match self {
            Self::Off => "Off".to_string(),
            Self::Paused => "Paused".to_string(),
            Self::Starting => "Starting…".to_string(),
            Self::Serving { address, port } => {
                let agents = match client_count {
                    0 => "no agents yet".to_string(),
                    1 => "1 agent".to_string(),
                    n => format!("{n} agents"),
                };
                format!("Serving on {address}:{port} · {agents}")
            }
            Self::Failed { port, error } => format!("Not serving: {error} (port {port})"),
        }
    }
}

/// A paired machine as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientView {
    pub name: String,
    pub last_seen: String,
}

impl ClientView {
    /// `robin · last seen 3 min ago`, or just the name before the first call.
    pub fn label(&self, now: chrono::DateTime<Utc>) -> String {
        match chrono::DateTime::parse_from_rfc3339(&self.last_seen) {
            Ok(seen) => {
                let minutes = (now - seen.with_timezone(&Utc)).num_minutes();
                let ago = if minutes < 1 {
                    "just now".to_string()
                } else if minutes < 60 {
                    format!("{minutes} min ago")
                } else if minutes < 60 * 24 {
                    format!("{} h ago", minutes / 60)
                } else {
                    format!("{} days ago", minutes / (60 * 24))
                };
                format!("{} · last seen {ago}", self.name)
            }
            Err(_) => self.name.clone(),
        }
    }
}

/// What *Connect an agent* shows (§7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingInvite {
    pub code: String,
    /// The desk's addresses as the modal can see them: `.local` name first.
    pub addresses: Vec<String>,
    pub port: u16,
    pub expires_at: String,
}

impl PairingInvite {
    pub fn command(&self, address: &str) -> String {
        format!("thock-bridge pair {address}:{} {}", self.port, self.code)
    }
}

/// What the listener thread and the service both read: the paired clients
/// (checked on every request) and the live pairing code.
#[derive(Default)]
struct Shared {
    clients: Vec<PairedClient>,
    pending: Option<PendingCode>,
}

struct Listener {
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::warn!("Thock: the agents bridge listener thread panicked");
        }
    }
}

/// One request the thread has authenticated, for the foreground to answer.
struct Incoming {
    route: Route,
    /// The paired client's name, once the token checked out; `None` on
    /// `/v1/pair`.
    client: Option<String>,
    harness: String,
    body: Vec<u8>,
}

struct Outgoing {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Outgoing {
    fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.into_bytes(),
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, thock_bridge::response_json(message))
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: Vec::new(),
        }
    }
}

/// Call ids the desk already applied (§9.5), so a drain that lost its
/// reply never writes twice.
#[derive(Default)]
struct SeenCalls {
    loaded: bool,
    entries: HashMap<String, (chrono::DateTime<Utc>, WriteOutcome)>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SeenLine {
    id: String,
    at: String,
    outcome: WriteOutcome,
}

impl gpui::EventEmitter<ManualSyncFinished> for BridgeService {}

pub struct BridgeService {
    project: Entity<Project>,
    vault: Option<Vault>,
    enabled: bool,
    paused: bool,
    state: BridgeState,
    config: BridgeConfig,
    shared: Arc<Mutex<Shared>>,
    listener: Option<Listener>,
    receiver_task: Option<Task<()>>,
    start_task: Option<Task<()>>,
    index: Rc<RefCell<Option<SearchIndex>>>,
    seen: Rc<RefCell<SeenCalls>>,
    serving_since: String,
    _subscriptions: Vec<gpui::Subscription>,
}

impl BridgeService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        let mut this = Self::bare(project, vec![project_subscription]);
        this.reload(cx);
        this
    }

    fn bare(project: Entity<Project>, subscriptions: Vec<gpui::Subscription>) -> Self {
        Self {
            project,
            vault: None,
            enabled: false,
            paused: false,
            state: BridgeState::Off,
            config: BridgeConfig::default(),
            shared: Arc::new(Mutex::new(Shared::default())),
            listener: None,
            receiver_task: None,
            start_task: None,
            index: Rc::new(RefCell::new(None)),
            seen: Rc::new(RefCell::new(SeenCalls::default())),
            serving_since: String::new(),
            _subscriptions: subscriptions,
        }
    }

    /// A service over a known vault, for tests that run on a fake file
    /// system where `Vault::detect` cannot look.
    #[cfg(test)]
    pub(crate) fn attach_for_test(
        vault: Vault,
        project: &Entity<Project>,
        cx: &mut App,
    ) -> Entity<Self> {
        let service = cx.new(|cx| {
            let project_subscription = cx.subscribe(project, Self::handle_project_event);
            let mut this = Self::bare(project.clone(), vec![project_subscription]);
            this.enabled = routine_enabled(&vault);
            this.vault = Some(vault);
            this.apply_state(cx);
            this
        });
        cx.default_global::<GlobalBridgeServices>()
            .0
            .insert(project.entity_id(), service.clone());
        service
    }

    pub fn state(&self) -> &BridgeState {
        &self.state
    }

    /// Whether the Agents Routine is installed in this vault.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn clients(&self) -> Vec<ClientView> {
        self.shared
            .lock()
            .map(|shared| {
                shared
                    .clients
                    .iter()
                    .map(|client| ClientView {
                        name: client.name.clone(),
                        last_seen: client.last_seen.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn fs(&self, cx: &App) -> Arc<dyn Fs> {
        self.project.read(cx).fs().clone()
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
                let mut reload = false;
                for (path, _, change) in changes.iter() {
                    let path = path.as_unix_str();
                    if path == vault_config || path == BRIDGE_CONFIG_PATH {
                        reload = true;
                    } else if SearchIndex::indexes(path) {
                        self.index_changed(path, matches!(change, project::PathChange::Removed), cx);
                    }
                }
                if reload {
                    self.reload(cx);
                }
            }
            _ => {}
        }
    }

    /// Keeps the search index current from the worktree's events (§9.3).
    /// Nothing happens before a first `search` builds it.
    fn index_changed(&mut self, path: &str, removed: bool, cx: &mut Context<Self>) {
        if self.index.borrow().is_none() {
            return;
        }
        if removed {
            if let Some(index) = self.index.borrow_mut().as_mut() {
                index.remove(path);
            }
            return;
        }
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.fs(cx);
        let index = self.index.clone();
        let path = path.to_string();
        cx.spawn(async move |_, _| {
            if let Some(entry) = read_index_entry(&fs, &vault.root, &path).await
                && let Some(index) = index.borrow_mut().as_mut()
            {
                index.upsert(&path, entry.0, entry.1);
            }
        })
        .detach();
    }

    /// Re-resolves the vault and whether the Routine is on; starts or stops
    /// the server to match.
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
        let changed_root =
            self.vault.as_ref().map(|v| v.root.as_path()) != vault.as_ref().map(|v| v.root.as_path());
        self.enabled = vault.as_ref().is_some_and(routine_enabled);
        self.vault = vault;
        if changed_root {
            self.stop(BridgeState::Off);
            *self.index.borrow_mut() = None;
            *self.seen.borrow_mut() = SeenCalls::default();
        }
        self.apply_state(cx);
    }

    fn apply_state(&mut self, cx: &mut Context<Self>) {
        let should_serve = self.vault.is_some() && self.enabled && !self.paused;
        if !should_serve {
            let state = if self.enabled && self.paused {
                BridgeState::Paused
            } else {
                BridgeState::Off
            };
            self.stop(state);
            cx.notify();
            return;
        }
        if self.listener.is_some() || self.start_task.is_some() {
            return;
        }
        self.start(cx);
    }

    fn stop(&mut self, state: BridgeState) {
        self.listener = None;
        self.receiver_task = None;
        self.start_task = None;
        self.state = state;
    }

    /// `thock::ToggleBridge`.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        self.apply_state(cx);
    }

    /// Reads (or writes) `bridge.toml` and the paired clients, then binds.
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.fs(cx);
        self.state = BridgeState::Starting;
        cx.notify();
        self.start_task = Some(cx.spawn(async move |this, cx| {
            let loaded = load_server_files(&fs, &vault.root).await;
            this.update(cx, |service, cx| {
                service.start_task = None;
                match loaded {
                    Ok((config, clients, pending)) => {
                        service.config = config;
                        if let Ok(mut shared) = service.shared.lock() {
                            shared.clients = clients;
                            shared.pending = pending;
                        }
                        service.bind(cx);
                    }
                    Err(error) => {
                        service.state = BridgeState::Failed {
                            port: service.config.port,
                            error: format!("couldn't read its settings: {error:#}"),
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn bind(&mut self, cx: &mut Context<Self>) {
        let address = format!("{}:{}", self.config.bind, self.config.port);
        let server = match tiny_http::Server::http(&address) {
            Ok(server) => server,
            Err(error) => {
                let message = error.to_string();
                let error = if message.contains("in use") {
                    format!("port {} is in use", self.config.port)
                } else {
                    message
                };
                log::warn!("Thock: the agents bridge couldn't bind {address}: {error}");
                self.state = BridgeState::Failed {
                    port: self.config.port,
                    error,
                };
                return;
            }
        };
        let port = server
            .server_addr()
            .to_ip()
            .map(|addr| addr.port())
            .unwrap_or(self.config.port);
        let (sender, mut receiver) = mpsc::unbounded::<(Incoming, oneshot::Sender<Outgoing>)>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("thock-bridge-listener".into())
            .spawn({
                let shutdown = shutdown.clone();
                let shared = self.shared.clone();
                move || listen(server, shutdown, shared, sender)
            });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                self.state = BridgeState::Failed {
                    port,
                    error: format!("couldn't start its thread: {error}"),
                };
                return;
            }
        };
        self.listener = Some(Listener {
            shutdown,
            thread: Some(thread),
        });
        self.serving_since = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.state = BridgeState::Serving {
            address: desk_name(),
            port,
        };
        self.receiver_task = Some(cx.spawn(async move |this, cx| {
            while let Some((incoming, reply)) = receiver.next().await {
                let Some(service) = this.upgrade() else {
                    break;
                };
                let outgoing = handle_incoming(service, incoming, cx).await;
                reply.send(outgoing).ok();
            }
        }));
        log::info!("Thock: the agents bridge is serving on port {port}");
    }

    /// `thock::ConnectOtherAgent`: mints a pairing code good for ten minutes.
    pub fn begin_pairing(&mut self, cx: &mut Context<Self>) -> Task<Result<PairingInvite>> {
        let Some(vault) = self.vault.clone() else {
            return Task::ready(Err(anyhow!("this workspace isn't a vault")));
        };
        let port = match &self.state {
            BridgeState::Serving { port, .. } => *port,
            BridgeState::Failed { error, .. } => {
                return Task::ready(Err(anyhow!("the desk isn't serving: {error}")));
            }
            _ => return Task::ready(Err(anyhow!("the desk isn't serving yet; try again in a moment"))),
        };
        let code = thock_bridge::generate_code();
        let pending = PendingCode {
            code_sha256: thock_bridge::sha256_hex(&code),
            expires_at: (Utc::now() + chrono::Duration::minutes(thock_bridge::PAIRING_CODE_MINUTES))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        };
        if let Ok(mut shared) = self.shared.lock() {
            shared.pending = Some(pending.clone());
        }
        let fs = self.fs(cx);
        let addresses = desk_addresses();
        cx.background_spawn(async move {
            write_state_file(&fs, &vault.root, PENDING_PATH, &pending.render()).await?;
            Ok(PairingInvite {
                code,
                addresses,
                port,
                expires_at: pending.expires_at,
            })
        })
    }

    /// Forgets a paired machine; its token stops working at once.
    pub fn disconnect(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let clients = match self.shared.lock() {
            Ok(mut shared) => {
                shared.clients.retain(|client| client.name != name);
                shared.clients.clone()
            }
            Err(_) => return,
        };
        let fs = self.fs(cx);
        cx.background_spawn(async move {
            write_state_file(
                &fs,
                &vault.root,
                CLIENTS_PATH,
                &thock_bridge::render_clients(&clients),
            )
            .await
        })
        .detach_and_log_err(cx);
        cx.notify();
    }
}

fn routine_enabled(vault: &Vault) -> bool {
    vault
        .config
        .routines
        .installed
        .iter()
        .any(|entry| entry.id == AGENTS_ROUTINE_ID && entry.enabled)
}

/// The machine's short host name, for `via` labels and the pairing command.
fn desk_name() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .split('.')
        .next()
        .unwrap_or("this-computer")
        .to_string()
}

/// `.local` name first (mDNS reaches it on most home networks), then the
/// first non-loopback IPv4 the machine would route out through.
fn desk_addresses() -> Vec<String> {
    let mut addresses = vec![format!("{}.local", desk_name())];
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
        && socket.connect("10.255.255.255:1").is_ok()
        && let Ok(local) = socket.local_addr()
        && !local.ip().is_loopback()
    {
        addresses.push(local.ip().to_string());
    }
    addresses
}

async fn load_server_files(
    fs: &Arc<dyn Fs>,
    root: &Path,
) -> Result<(BridgeConfig, Vec<PairedClient>, Option<PendingCode>)> {
    let config_path = root.join(BRIDGE_CONFIG_PATH);
    let config = if fs.is_file(&config_path).await {
        let raw = fs.load(&config_path).await?;
        BridgeConfig::parse(&raw).with_context(|| format!("reading {BRIDGE_CONFIG_PATH}"))?
    } else {
        let config = BridgeConfig::default();
        write_state_file(fs, root, BRIDGE_CONFIG_PATH, &config.render()).await?;
        config
    };
    let clients_path = root.join(CLIENTS_PATH);
    let clients = if fs.is_file(&clients_path).await {
        thock_bridge::parse_clients(&fs.load(&clients_path).await?)
            .with_context(|| format!("reading {CLIENTS_PATH}"))?
    } else {
        Vec::new()
    };
    let pending_path = root.join(PENDING_PATH);
    let pending = if fs.is_file(&pending_path).await {
        PendingCode::parse(&fs.load(&pending_path).await?)
            .ok()
            .filter(|pending| !pending.is_expired(Utc::now()))
    } else {
        None
    };
    Ok((config, clients, pending))
}

async fn write_state_file(fs: &Arc<dyn Fs>, root: &Path, relative: &str, text: &str) -> Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs.create_dir(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    fs.atomic_write(path, text.to_string())
        .await
        .with_context(|| format!("writing {relative}"))
}

/// The listener thread: parses each request, checks the token against the
/// paired clients, and hands the rest to the foreground.
fn listen(
    server: tiny_http::Server,
    shutdown: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    sender: mpsc::UnboundedSender<(Incoming, oneshot::Sender<Outgoing>)>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        let request = match server.recv_timeout(RECV_TIMEOUT) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(error) => {
                log::warn!("Thock: the agents bridge stopped listening: {error}");
                break;
            }
        };
        let outgoing = answer(request_parts(request), &shared, &sender);
        let (request, outgoing) = outgoing;
        let mut response =
            tiny_http::Response::from_data(outgoing.body).with_status_code(outgoing.status);
        for (name, value) in [
            ("Content-Type", outgoing.content_type),
            ("Connection", "close"),
        ] {
            if let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()) {
                response = response.with_header(header);
            }
        }
        if let Err(error) = request.respond(response) {
            log::warn!("Thock: couldn't answer an agents bridge request: {error}");
        }
    }
}

struct RequestParts {
    request: tiny_http::Request,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    oversized: bool,
}

fn request_parts(mut request: tiny_http::Request) -> RequestParts {
    let method = request.method().as_str().to_string();
    let url = request.url().to_string();
    let headers = request
        .headers()
        .iter()
        .map(|header| {
            (
                header.field.as_str().to_string(),
                header.value.as_str().to_string(),
            )
        })
        .collect();
    let mut body = Vec::new();
    let oversized = request.body_length().is_some_and(|length| length > BODY_LIMIT);
    if !oversized {
        request
            .as_reader()
            .take(BODY_LIMIT as u64 + 1)
            .read_to_end(&mut body)
            .log_err();
    }
    RequestParts {
        request,
        method,
        url,
        headers,
        body,
        oversized,
    }
}

fn answer(
    parts: RequestParts,
    shared: &Arc<Mutex<Shared>>,
    sender: &mpsc::UnboundedSender<(Incoming, oneshot::Sender<Outgoing>)>,
) -> (tiny_http::Request, Outgoing) {
    let RequestParts {
        request,
        method,
        url,
        headers,
        body,
        oversized,
    } = parts;
    if oversized || body.len() > BODY_LIMIT {
        return (request, Outgoing::error(413, "that is more than the desk takes in one request"));
    }
    let route = thock_bridge::parse_route(&method, &url);
    let client = match route {
        Route::Pair | Route::NotFound | Route::McpNotPost => None,
        _ => {
            let token = thock_bridge::bearer_token(
                headers
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            let hash = token.map(thock_bridge::sha256_hex);
            let known = shared.lock().ok().and_then(|shared| {
                shared
                    .clients
                    .iter()
                    .find(|client| {
                        hash.as_deref()
                            .is_some_and(|hash| thock_bridge::constant_time_eq(hash, &client.token_sha256))
                    })
                    .map(|client| client.name.clone())
            });
            match known {
                Some(name) => Some(name),
                None => {
                    return (
                        request,
                        Outgoing::error(401, "unknown client; pair again from the desk"),
                    );
                }
            }
        }
    };
    let harness = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(thock_bridge::HARNESS_HEADER))
        .map(|(_, value)| value.as_str())
        .unwrap_or("agent");
    let incoming = Incoming {
        route,
        client,
        harness: thock_bridge::sanitize_harness(harness),
        body,
    };
    let (reply_sender, reply_receiver) = oneshot::channel();
    if sender.unbounded_send((incoming, reply_sender)).is_err() {
        return (request, Outgoing::error(503, "the desk is closing"));
    }
    match futures::executor::block_on(reply_receiver) {
        Ok(outgoing) => (request, outgoing),
        Err(_) => (request, Outgoing::error(503, "the desk is closing")),
    }
}

/// What the foreground needs per request, read in one entity update.
struct RequestContext {
    fs: Arc<dyn Fs>,
    vault: Vault,
    config: BridgeConfig,
    shared: Arc<Mutex<Shared>>,
    index: Rc<RefCell<Option<SearchIndex>>>,
    seen: Rc<RefCell<SeenCalls>>,
    project: Entity<Project>,
    serving_since: String,
}

async fn handle_incoming(
    service: Entity<BridgeService>,
    incoming: Incoming,
    cx: &mut AsyncApp,
) -> Outgoing {
    let context = service.update(cx, |service, cx| {
        service.vault.clone().map(|vault| RequestContext {
            fs: service.fs(cx),
            vault,
            config: service.config.clone(),
            shared: service.shared.clone(),
            index: service.index.clone(),
            seen: service.seen.clone(),
            project: service.project.clone(),
            serving_since: service.serving_since.clone(),
        })
    });
    let Some(context) = context else {
        return Outgoing::error(503, "the vault is closed");
    };
    let outgoing = match incoming.route {
        Route::Pair => handle_pair(&context, &incoming.body, &service, cx).await,
        Route::Health => Outgoing::json(
            200,
            serde_json::to_string(&thock_bridge::HealthResponse {
                vault: vault_name(&context.vault),
                desk: desk_name(),
                version: APP_VERSION.to_string(),
                serving_since: context.serving_since.clone(),
            })
            .unwrap_or_default(),
        ),
        Route::Mcp => handle_mcp(&context, &incoming, cx).await,
        Route::McpNotPost => Outgoing::error(405, "POST one JSON-RPC request to /mcp"),
        Route::Skill => {
            let path = context.vault.root.join(thock_bridge::SKILL_PATH);
            match context.fs.load(&path).await {
                Ok(text) => Outgoing {
                    status: 200,
                    content_type: "text/markdown",
                    body: text.into_bytes(),
                },
                Err(_) => Outgoing::error(404, "the vault has no thock-memory skill"),
            }
        }
        Route::AgentFile { harness, path } => {
            handle_agent_file(&context, incoming.client.as_deref(), &harness, &path, &incoming.body)
                .await
        }
        Route::NotFound => Outgoing::error(404, "nothing here"),
    };
    if let Some(name) = incoming.client {
        touch_client(&context, &name).await;
        service.update(cx, |_, cx| cx.notify());
    }
    outgoing
}

/// Records when a paired machine last called, persisted once a minute.
async fn touch_client(context: &RequestContext, name: &str) {
    let now = Utc::now();
    let clients = {
        let Ok(mut shared) = context.shared.lock() else {
            return;
        };
        let Some(client) = shared.clients.iter_mut().find(|client| client.name == name) else {
            return;
        };
        let stale = chrono::DateTime::parse_from_rfc3339(&client.last_seen)
            .map(|seen| (now - seen.with_timezone(&Utc)).num_seconds() >= 60)
            .unwrap_or(true);
        client.last_seen = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        if !stale {
            return;
        }
        shared.clients.clone()
    };
    write_state_file(
        &context.fs,
        &context.vault.root,
        CLIENTS_PATH,
        &thock_bridge::render_clients(&clients),
    )
    .await
    .log_err();
}

fn vault_name(vault: &Vault) -> String {
    vault
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vault".to_string())
}

async fn handle_pair(
    context: &RequestContext,
    body: &[u8],
    service: &Entity<BridgeService>,
    cx: &mut AsyncApp,
) -> Outgoing {
    let Ok(request) = serde_json::from_slice::<PairRequest>(body) else {
        return Outgoing::error(400, "pair needs {\"code\", \"name\"}");
    };
    let name = request.name.trim();
    if name.is_empty() || name.len() > 64 || name.contains(['/', '\\', '@']) {
        return Outgoing::error(400, "the client's name must be a plain host name");
    }
    let accepted = match context.shared.lock() {
        Ok(mut shared) => match shared.pending.take() {
            Some(pending) if !pending.is_expired(Utc::now()) && pending.matches(&request.code) => {
                true
            }
            Some(pending) if pending.is_expired(Utc::now()) => false,
            Some(pending) => {
                // A wrong guess keeps the code alive; only a match or the
                // clock spends it.
                shared.pending = Some(pending);
                return Outgoing::error(403, "that is not the code on the desk");
            }
            None => false,
        },
        Err(_) => false,
    };
    if !accepted {
        return Outgoing::error(410, "that code was already used or has expired");
    }
    let token = thock_bridge::generate_token();
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let clients = match context.shared.lock() {
        Ok(mut shared) => {
            shared.clients.retain(|client| client.name != name);
            shared.clients.push(PairedClient {
                name: name.to_string(),
                token_sha256: thock_bridge::sha256_hex(&token),
                paired_at: now.clone(),
                last_seen: now,
            });
            shared.clients.clone()
        }
        Err(_) => return Outgoing::error(500, "the desk couldn't record the pairing"),
    };
    let persisted = write_state_file(
        &context.fs,
        &context.vault.root,
        CLIENTS_PATH,
        &thock_bridge::render_clients(&clients),
    )
    .await;
    if let Err(error) = persisted {
        log::error!("Thock: couldn't write {CLIENTS_PATH}: {error:#}");
        return Outgoing::error(500, "the desk couldn't record the pairing");
    }
    context
        .fs
        .remove_file(&context.vault.root.join(PENDING_PATH), Default::default())
        .await
        .log_err();
    service.update(cx, |_, cx| cx.notify());
    Outgoing::json(
        200,
        serde_json::to_string(&PairResponse {
            token,
            vault: vault_name(&context.vault),
            desk: desk_name(),
        })
        .unwrap_or_default(),
    )
}

async fn handle_mcp(context: &RequestContext, incoming: &Incoming, cx: &mut AsyncApp) -> Outgoing {
    let body = String::from_utf8_lossy(&incoming.body);
    let request = match JsonRpcRequest::parse(&body) {
        Ok(request) => request,
        Err(error) => return Outgoing::json(400, error.to_json()),
    };
    if request.is_notification() {
        return Outgoing::empty(202);
    }
    let Some(client) = incoming.client.as_deref() else {
        return Outgoing::error(401, "unknown client; pair again from the desk");
    };
    let mut host = DeskHost {
        context,
        identity: ClientIdentity::new(&incoming.harness, client),
        cx: cx.clone(),
    };
    match thock_bridge::dispatch(&mut host, request, APP_VERSION).await {
        Some(response) => Outgoing::json(200, response.to_json()),
        None => Outgoing::empty(202),
    }
}

/// `PUT /v1/agents/{harness}/{path}`: a harness-owned whole file lands
/// under `memory/agents/{harness}/{client}/{path}` (§9.6).
async fn handle_agent_file(
    context: &RequestContext,
    client: Option<&str>,
    harness: &str,
    path: &str,
    body: &[u8],
) -> Outgoing {
    let Some(client) = client else {
        return Outgoing::error(401, "unknown client; pair again from the desk");
    };
    if body.len() as u64 > thock_bridge::MIRROR_MAX_BYTES {
        return Outgoing::error(413, "mirrored files stay under 1 MiB");
    }
    if !thock_bridge::is_mirrored_path(path) {
        return Outgoing::error(400, "only .md, .txt and .json files at plain paths are mirrored");
    }
    let harness = thock_bridge::sanitize_harness(harness);
    let relative = format!("{}/{harness}/{client}/{path}", thock_bridge::AGENTS_DIR);
    if thock_bridge::validate_vault_path(&relative).is_err() {
        return Outgoing::error(400, "that path can't live in the vault");
    }
    let absolute = context.vault.root.join(&relative);
    let Some(parent) = absolute.parent().map(Path::to_path_buf) else {
        return Outgoing::error(400, "that path can't live in the vault");
    };
    let Ok(text) = String::from_utf8(body.to_vec()) else {
        return Outgoing::error(400, "mirrored files must be UTF-8 text");
    };
    let written = async {
        context.fs.create_dir(&parent).await?;
        context.fs.atomic_write(absolute, text).await
    }
    .await;
    match written {
        Ok(()) => Outgoing::empty(204),
        Err(error) => {
            log::error!("Thock: couldn't mirror {relative}: {error:#}");
            Outgoing::error(500, "the desk couldn't write the file")
        }
    }
}

/// The six tools over the project `Fs` (§11).
struct DeskHost<'a> {
    context: &'a RequestContext,
    identity: ClientIdentity,
    cx: AsyncApp,
}

impl DeskHost<'_> {
    fn root(&self) -> &Path {
        &self.context.vault.root
    }

    async fn forbidden_topics(&self) -> Vec<String> {
        let path = self.root().join(PROFILE_PATH);
        match self.context.fs.load(&path).await {
            Ok(profile) => thock_bridge::forbidden_topics(&profile),
            Err(_) => Vec::new(),
        }
    }

    async fn ensure_index(&self) -> Result<(), HostError> {
        if self.context.index.borrow().is_some() {
            return Ok(());
        }
        let index = build_index(&self.context.fs, self.root()).await?;
        *self.context.index.borrow_mut() = Some(index);
        Ok(())
    }

    /// The absolute path of a vault-relative note, refused when it leaves
    /// the vault (§9.1).
    async fn note_path(&self, relative: &str) -> Result<PathBuf, HostError> {
        thock_bridge::validate_vault_path(relative).map_err(|error| HostError(error.to_string()))?;
        let absolute = self.root().join(relative.trim());
        if let Ok(canonical) = self.context.fs.canonicalize(&absolute).await {
            let root = self
                .context
                .fs
                .canonicalize(self.root())
                .await
                .unwrap_or_else(|_| self.root().to_path_buf());
            if !canonical.starts_with(&root) {
                return Err(HostError(format!("{relative} points outside the vault")));
            }
        }
        Ok(absolute)
    }

    async fn seen(&self, meta: &CallMeta) -> Option<WriteOutcome> {
        let id = meta.call_id.as_deref()?;
        self.load_seen().await;
        let seen = self.context.seen.borrow();
        seen.entries.get(id).map(|(_, outcome)| *outcome)
    }

    async fn load_seen(&self) {
        if self.context.seen.borrow().loaded {
            return;
        }
        let path = self.root().join(SEEN_PATH);
        let raw = self.context.fs.load(&path).await.unwrap_or_default();
        let cutoff = Utc::now() - chrono::Duration::hours(thock_bridge::SEEN_CALL_HOURS);
        let mut seen = self.context.seen.borrow_mut();
        seen.loaded = true;
        for line in raw.lines() {
            let Ok(entry) = serde_json::from_str::<SeenLine>(line) else {
                continue;
            };
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(&entry.at) else {
                continue;
            };
            let at = at.with_timezone(&Utc);
            if at >= cutoff {
                seen.entries.insert(entry.id, (at, entry.outcome));
            }
        }
    }

    async fn record_seen(&self, meta: &CallMeta, outcome: WriteOutcome) {
        let Some(id) = meta.call_id.clone() else {
            return;
        };
        let now = Utc::now();
        let lines = {
            let mut seen = self.context.seen.borrow_mut();
            seen.entries.insert(id, (now, outcome));
            seen.entries
                .iter()
                .map(|(id, (at, outcome))| SeenLine {
                    id: id.clone(),
                    at: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    outcome: *outcome,
                })
                .map(|line| serde_json::to_string(&line).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
        };
        write_state_file(&self.context.fs, self.root(), SEEN_PATH, &format!("{lines}\n"))
            .await
            .log_err();
    }

    /// Applies a sync-core write to a note through the `Fs`, after the
    /// history checkpoint every AI write takes.
    async fn apply_write(&mut self, write: &Write, seed: Option<String>) -> Result<WriteOutcome, HostError> {
        let absolute = self.root().join(&write.path);
        let existing = if self.context.fs.is_file(&absolute).await {
            Some(self.context.fs.load(&absolute).await.map_err(HostError::from)?)
        } else {
            None
        };
        let applied = apply(existing.as_deref(), write, seed.as_deref());
        if applied.outcome == Outcome::Noop || existing.as_deref() == Some(applied.text.as_str()) {
            return Ok(WriteOutcome::Noop);
        }
        let project = self.context.project.clone();
        self.cx
            .update(|cx| history::checkpoint_before_ai_write(&project, cx));
        if let Some(parent) = absolute.parent() {
            self.context.fs.create_dir(parent).await.map_err(HostError::from)?;
        }
        self.context
            .fs
            .atomic_write(absolute, applied.text)
            .await
            .map_err(HostError::from)?;
        Ok(WriteOutcome::from_outcome(applied.outcome))
    }
}

impl ToolHost for DeskHost<'_> {
    async fn context(&mut self) -> Result<String, HostError> {
        let today = Local::now().date_naive();
        let vault = self.context.vault.clone();
        let fs = self.context.fs.clone();
        let base = self
            .cx
            .background_spawn(async move {
                crate::hosted_agent::gather_vault_context(&fs, Some(&vault), today).await
            })
            .await
            .unwrap_or_default();
        let forbidden = self.forbidden_topics().await;
        Ok(crate::hosted_agent::bridge_context(&base, &self.identity, &forbidden, &self.context.config.today_heading))
    }

    async fn memory_index(&mut self) -> Result<String, HostError> {
        let lines = self.context.vault.config.memory.index_lines;
        Ok(crate::memory::read_index_capped(&self.context.fs, self.root(), lines)
            .await
            .unwrap_or_else(|| "Nothing yet: no session has learned anything about them.".into()))
    }

    async fn search(&mut self, query: &SearchQuery) -> Result<Vec<SearchHit>, HostError> {
        self.ensure_index().await?;
        let today = Local::now().date_naive();
        Ok(self
            .context
            .index
            .borrow()
            .as_ref()
            .map(|index| index.search(query, today))
            .unwrap_or_default())
    }

    async fn read(&mut self, target: &ReadTarget) -> Result<String, HostError> {
        let relative = match target {
            ReadTarget::Path(path) => path.trim().to_string(),
            ReadTarget::Link(link) => {
                self.ensure_index().await?;
                let resolution = self
                    .context
                    .index
                    .borrow()
                    .as_ref()
                    .map(|index| index.resolve_link(link))
                    .unwrap_or(thock_bridge::LinkResolution::Missing);
                match resolution {
                    thock_bridge::LinkResolution::Found(path) => path,
                    thock_bridge::LinkResolution::Ambiguous(paths) => {
                        return Err(HostError(format!(
                            "{link} names more than one note: {}",
                            paths.join(", ")
                        )));
                    }
                    thock_bridge::LinkResolution::Missing => {
                        return Err(HostError(format!("no note is titled or named {link}")));
                    }
                }
            }
        };
        let absolute = self.note_path(&relative).await?;
        if !self.context.fs.is_file(&absolute).await {
            return Err(HostError(format!("{relative} is not in the vault")));
        }
        self.context.fs.load(&absolute).await.map_err(HostError::from)
    }

    async fn recent(&mut self, days: u32, folder: Option<&str>) -> Result<Vec<RecentNote>, HostError> {
        self.ensure_index().await?;
        let today = Local::now().date_naive();
        Ok(self
            .context
            .index
            .borrow()
            .as_ref()
            .map(|index| index.recent(days, folder, today))
            .unwrap_or_default())
    }

    async fn remember(&mut self, fact: &str, meta: &CallMeta) -> Result<WriteOutcome, HostError> {
        if let Some(outcome) = self.seen(meta).await {
            return Ok(outcome);
        }
        let forbidden = self.forbidden_topics().await;
        let fact = thock_bridge::check_fact(fact, &forbidden)?;
        let date = meta.date.unwrap_or_else(|| Local::now().date_naive());
        let line = thock_bridge::inbox_line(date, &fact, &self.identity);
        let inbox = self.root().join(thock_bridge::INBOX_PATH);
        if let Ok(existing) = self.context.fs.load(&inbox).await
            && existing.lines().any(|existing_line| {
                thock_bridge::parse_inbox_line(existing_line)
                    .is_some_and(|parsed| parsed.fact.eq_ignore_ascii_case(&fact))
            })
        {
            return Err(HostError("that line is already in the inbox".into()));
        }
        let write = thock_bridge::inbox_write(WRITE_CLIENT_ID, &line);
        let outcome = self.apply_write(&write, None).await?;
        self.record_seen(meta, outcome).await;
        Ok(outcome)
    }

    async fn note_today(&mut self, lines: &[String], meta: &CallMeta) -> Result<WriteOutcome, HostError> {
        if let Some(outcome) = self.seen(meta).await {
            return Ok(outcome);
        }
        let forbidden = self.forbidden_topics().await;
        let mut rendered = Vec::with_capacity(lines.len());
        for line in lines {
            let line = thock_bridge::check_fact(line, &forbidden)?;
            rendered.push(thock_bridge::today_line(&self.identity, &line));
        }
        let date = meta.date.unwrap_or_else(|| Local::now().date_naive());
        let vault = &self.context.vault;
        let absolute = vault.note_path(NoteKind::Daily, date);
        let relative = absolute
            .strip_prefix(&vault.root)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|_| HostError("today's note is outside the vault".into()))?;
        let seed = if self.context.fs.is_file(&absolute).await {
            None
        } else {
            let template = self
                .context
                .fs
                .load(&vault.template_path(NoteKind::Daily))
                .await
                .unwrap_or_default();
            let title = absolute
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            Some(expand_template(&template, date, Local::now().time(), &title))
        };
        let write = thock_bridge::today_write(
            WRITE_CLIENT_ID,
            &relative,
            &self.context.config.today_heading,
            &rendered,
        );
        let outcome = self.apply_write(&write, seed).await?;
        self.record_seen(meta, outcome).await;
        Ok(outcome)
    }
}

/// One index entry's ingredients, read through the `Fs`.
async fn read_index_entry(fs: &Arc<dyn Fs>, root: &Path, relative: &str) -> Option<(NaiveDate, String)> {
    let absolute = root.join(relative);
    let metadata = fs.metadata(&absolute).await.ok().flatten()?;
    if metadata.is_dir || metadata.len > BODY_LIMIT as u64 {
        return None;
    }
    let body = fs.load(&absolute).await.ok()?;
    let modified: chrono::DateTime<Local> = metadata.mtime.timestamp_for_user().into();
    Some((modified.date_naive(), body))
}

/// Walks the vault for every `.md` outside a dot-folder (§9.3). A cold
/// first `search` pays for this once per vault open.
async fn build_index(fs: &Arc<dyn Fs>, root: &Path) -> Result<SearchIndex, HostError> {
    let mut index = SearchIndex::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(mut entries) = fs.read_dir(&dir).await else {
            continue;
        };
        while let Some(entry) = entries.next().await {
            let Ok(path) = entry else {
                continue;
            };
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if fs.is_dir(&path).await {
                pending.push(path);
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !SearchIndex::indexes(&relative) {
                continue;
            }
            if let Some((modified, body)) = read_index_entry(fs, root, &relative).await {
                index.upsert(&relative, modified, body);
            }
        }
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use settings::SettingsStore;
    use std::net::TcpStream;

    use crate::vault::{DEFAULT_DAILY_TEMPLATE, InstalledRoutine, VaultConfig};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    fn vault(enabled: bool) -> Vault {
        let mut config = VaultConfig::default();
        if enabled {
            config
                .routines
                .installed
                .push(InstalledRoutine::new(AGENTS_ROUTINE_ID.to_string(), true, 1));
        }
        Vault {
            root: PathBuf::from("/vault"),
            config,
        }
    }

    fn vault_tree() -> serde_json::Value {
        json!({
            ".thock": {
                "config.toml": "schema = 1\n",
                "routines": { "agents": { "bridge.toml": "port = 0\nbind = \"127.0.0.1\"\n" } },
            },
            "templates": { "daily.md": DEFAULT_DAILY_TEMPLATE },
            "profile.md": "# Diego\n\n## What Thock should not keep\n- salary\n",
            "memory": {
                "index.md": "# What Thock has learned\n\n## People\n- **Ana**, your manager. → people/ana.md\n",
                "inbox.md": "# Noted, not yet filed\n\n_explainer_\n",
                "people": { "ana.md": "# Ana\n\n- Manager. Prefers written updates.\n" },
            },
            "daily": {
                "2026-11-03.md": "# Tuesday\n\n- Met Ana about the newsletter\n",
            },
            "routines": { "agents": { "skills": { "thock-memory": { "SKILL.md": "---\nname: thock-memory\n---\nthe skill\n" } } } },
        })
    }

    struct Desk {
        service: Entity<BridgeService>,
        fs: Arc<FakeFs>,
        port: u16,
    }

    /// The desk answers from a real thread, so the deterministic scheduler
    /// is told to expect activity from outside it.
    async fn serve(cx: &mut TestAppContext, enabled: bool) -> Desk {
        init_test(cx);
        cx.executor().allow_parking();
        let fs = FakeFs::new(cx.executor());
        fs.set_next_mtime(std::time::SystemTime::now());
        fs.insert_tree("/vault", vault_tree()).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service =
            cx.update(|cx| BridgeService::attach_for_test(vault(enabled), &project, cx));
        cx.run_until_parked();
        let port = match service.read_with(cx, |service, _| service.state().clone()) {
            BridgeState::Serving { port, .. } => port,
            other => {
                assert!(!enabled, "expected to serve, got {other:?}");
                0
            }
        };
        Desk { service, fs, port }
    }

    /// A blocking round trip on another thread, pumping the foreground
    /// until the desk has answered.
    fn roundtrip(
        cx: &mut TestAppContext,
        port: u16,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, String) {
        let request = thock_bridge::HttpRequest {
            method: method.to_string(),
            url: format!("http://127.0.0.1:{port}{path}"),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        };
        let handle = std::thread::spawn(move || {
            thock_bridge::request(&request, Duration::from_secs(10))
        });
        while !handle.is_finished() {
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(2));
        }
        cx.run_until_parked();
        let response = handle.join().unwrap().unwrap();
        (response.status, response.text())
    }

    async fn pair(cx: &mut TestAppContext, desk: &Desk, name: &str) -> String {
        let invite = desk
            .service
            .update(cx, |service, cx| service.begin_pairing(cx))
            .await
            .unwrap();
        assert_eq!(invite.port, desk.port);
        assert!(invite.command(&invite.addresses[0]).starts_with("thock-bridge pair "));
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "POST",
            "/v1/pair",
            &[("Content-Type", "application/json")],
            &json!({"code": invite.code, "name": name, "version": "test"}).to_string(),
        );
        assert_eq!(status, 200, "{body}");
        let paired: PairResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(paired.vault, "vault");
        paired.token
    }

    fn call_tool(
        cx: &mut TestAppContext,
        desk: &Desk,
        token: &str,
        name: &str,
        arguments: serde_json::Value,
        meta: Option<serde_json::Value>,
    ) -> thock_bridge::CallToolResponse {
        let mut params = json!({"name": name, "arguments": arguments});
        if let Some(meta) = meta {
            params["_meta"] = meta;
        }
        let request = JsonRpcRequest::new(json!(1), "tools/call", params);
        let auth = format!("Bearer {token}");
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "POST",
            "/mcp",
            &[
                ("Authorization", auth.as_str()),
                ("Content-Type", "application/json"),
                (thock_bridge::HARNESS_HEADER, "claude-code"),
            ],
            &serde_json::to_string(&request).unwrap(),
        );
        assert_eq!(status, 200, "{body}");
        let response = thock_bridge::JsonRpcResponse::parse(&body).unwrap();
        response
            .tool_result()
            .unwrap_or_else(|| panic!("not a tool result: {body}"))
    }

    #[gpui::test]
    async fn the_routine_off_binds_nothing(cx: &mut TestAppContext) {
        let desk = serve(cx, false).await;
        assert_eq!(
            desk.service.read_with(cx, |service, _| service.state().clone()),
            BridgeState::Off
        );
        assert!(!desk.fs.is_dir(Path::new("/vault/.thock/state")).await);
    }

    #[gpui::test]
    async fn enabling_binds_and_a_bad_token_is_refused(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        assert!(desk.port > 0);
        let (status, body) = roundtrip(cx, desk.port, "GET", "/v1/health", &[], "");
        assert_eq!(status, 401);
        assert!(body.contains("unknown client; pair again from the desk"));
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "GET",
            "/v1/health",
            &[("Authorization", "Bearer nope")],
            "",
        );
        assert_eq!(status, 401);
        let (status, _) = roundtrip(cx, desk.port, "GET", "/mcp", &[], "");
        assert_eq!(status, 405);

        let token = pair(cx, &desk, "robin").await;
        let auth = format!("Bearer {token}");
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "GET",
            "/v1/health",
            &[("Authorization", auth.as_str())],
            "",
        );
        assert_eq!(status, 200);
        assert!(body.contains("\"vault\":\"vault\""));
        let clients = desk.fs.load(Path::new("/vault/.thock/state/bridge/clients.toml")).await.unwrap();
        assert!(clients.contains("name = \"robin\""));
        assert!(!clients.contains(&token), "tokens are never stored in the clear");
        assert!(!desk.fs.is_file(Path::new("/vault/.thock/state/bridge/pending.toml")).await);

        // The code is spent.
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "POST",
            "/v1/pair",
            &[],
            &json!({"code": "whatever", "name": "hp"}).to_string(),
        );
        assert_eq!(status, 410);

        // Disconnecting stops the token.
        desk.service
            .update(cx, |service, cx| service.disconnect("robin", cx));
        cx.run_until_parked();
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "GET",
            "/v1/health",
            &[("Authorization", auth.as_str())],
            "",
        );
        assert_eq!(status, 401);
        assert!(desk.service.read_with(cx, |service, _| service.clients().is_empty()));
    }

    #[gpui::test]
    async fn remember_appends_the_inbox_line_once(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let meta = json!({"call_id": "call-1", "date": "2026-11-04"});
        let result = call_tool(
            cx,
            &desk,
            &token,
            "remember",
            json!({"fact": "Prefers PR titles in the imperative"}),
            Some(meta.clone()),
        );
        assert!(!result.is_error, "{}", result.first_text());
        assert_eq!(
            result.first_text(),
            "Noted in memory/inbox.md; the Reflect ritual files it (applied)."
        );
        let inbox = desk.fs.load(Path::new("/vault/memory/inbox.md")).await.unwrap();
        assert!(inbox.ends_with(
            "- 2026-11-04 · Prefers PR titles in the imperative · via claude-code@robin\n"
        ));

        // The same call id again is the same answer, not a second line.
        let again = call_tool(
            cx,
            &desk,
            &token,
            "remember",
            json!({"fact": "Prefers PR titles in the imperative"}),
            Some(meta),
        );
        assert_eq!(again.first_text(), result.first_text());
        let inbox_again = desk.fs.load(Path::new("/vault/memory/inbox.md")).await.unwrap();
        assert_eq!(inbox, inbox_again);
        assert!(desk.fs.is_file(Path::new("/vault/.thock/state/bridge/seen.jsonl")).await);

        // A repeat with a new id is refused as a duplicate.
        let duplicate = call_tool(
            cx,
            &desk,
            &token,
            "remember",
            json!({"fact": "prefers PR titles in the imperative"}),
            Some(json!({"call_id": "call-2", "date": "2026-11-04"})),
        );
        assert!(duplicate.is_error);
        assert_eq!(duplicate.first_text(), "that line is already in the inbox");

        // A forbidden topic is refused.
        let refused = call_tool(
            cx,
            &desk,
            &token,
            "remember",
            json!({"fact": "Their salary went up"}),
            None,
        );
        assert!(refused.is_error);
        assert_eq!(refused.first_text(), "that topic is on the person's do-not-keep list");
    }

    #[gpui::test]
    async fn note_today_creates_the_note_and_its_section(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let result = call_tool(
            cx,
            &desk,
            &token,
            "note_today",
            json!({"lines": ["Shipped the PR (#184); CI green.", "Fixed the release staple."]}),
            Some(json!({"call_id": "n-1", "date": "2026-11-04"})),
        );
        assert!(!result.is_error, "{}", result.first_text());
        assert_eq!(result.first_text(), "Added to today's note (created).");
        let note = desk.fs.load(Path::new("/vault/daily/2026-11-04.md")).await.unwrap();
        assert!(note.starts_with("# "), "made from the template: {note}");
        assert!(note.contains("## From your agents\n"));
        assert!(note.ends_with(
            "- claude-code@robin · Shipped the PR (#184); CI green.\n- claude-code@robin · Fixed the release staple.\n"
        ));
        let more = call_tool(
            cx,
            &desk,
            &token,
            "note_today",
            json!({"line": "One more."}),
            Some(json!({"call_id": "n-2", "date": "2026-11-04"})),
        );
        assert_eq!(more.first_text(), "Added to today's note (applied).");
        let on_existing = call_tool(
            cx,
            &desk,
            &token,
            "note_today",
            json!({"line": "Yesterday's."}),
            Some(json!({"call_id": "n-3", "date": "2026-11-03"})),
        );
        assert_eq!(on_existing.first_text(), "Added to today's note (section_added).");
        let yesterday = desk.fs.load(Path::new("/vault/daily/2026-11-03.md")).await.unwrap();
        assert!(yesterday.starts_with("# Tuesday\n\n- Met Ana about the newsletter\n"));
        assert!(yesterday.contains("\n## From your agents\n"), "{yesterday}");
        assert!(yesterday.ends_with("- claude-code@robin · Yesterday's.\n"), "{yesterday}");
    }

    #[gpui::test]
    async fn search_read_recent_and_context_look_at_the_vault(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let hits = call_tool(cx, &desk, &token, "search", json!({"query": "ana"}), None);
        assert!(!hits.is_error, "{}", hits.first_text());
        let text = hits.first_text().to_string();
        assert!(text.contains("`memory/people/ana.md` — Ana"), "{text}");
        assert!(text.contains("`daily/2026-11-03.md` — Tuesday"), "{text}");
        assert!(text.find("ana.md").unwrap() < text.find("2026-11-03.md").unwrap(), "title beats body");

        let read = call_tool(cx, &desk, &token, "read", json!({"link": "Ana"}), None);
        assert_eq!(read.first_text(), "# Ana\n\n- Manager. Prefers written updates.\n");
        let closed = call_tool(cx, &desk, &token, "read", json!({"path": ".thock/config.toml"}), None);
        assert!(closed.is_error);
        let gone = call_tool(cx, &desk, &token, "read", json!({"path": "daily/2000-01-01.md"}), None);
        assert!(gone.is_error);

        let recent = call_tool(cx, &desk, &token, "recent", json!({"days": 7}), None);
        assert!(recent.first_text().contains("`daily/2026-11-03.md` — Tuesday"));

        let context = call_tool(cx, &desk, &token, "context", json!({}), None);
        let text = context.first_text();
        assert!(text.contains("# Right now"), "{text}");
        assert!(text.contains("**Ana**"), "the memory index rides along: {text}");
        assert!(text.contains("you are claude-code@robin"), "{text}");
        assert!(text.contains("salary"), "the do-not-keep topics ride along: {text}");

        // A new note shows up in the index through the worktree's events.
        desk.fs
            .insert_tree("/vault/projects", json!({"newsletter.md": "# Newsletter\n\nAna owns it.\n"}))
            .await;
        cx.run_until_parked();
        let hits = call_tool(cx, &desk, &token, "search", json!({"query": "newsletter"}), None);
        assert!(hits.first_text().contains("`projects/newsletter.md` — Newsletter"), "{}", hits.first_text());
    }

    #[gpui::test]
    async fn a_mirror_put_lands_under_memory_agents(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let auth = format!("Bearer {token}");
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/MEMORY.md",
            &[("Authorization", auth.as_str())],
            "# Memory\n- build needs cmake\n",
        );
        assert_eq!(status, 204);
        assert_eq!(
            desk.fs
                .load(Path::new("/vault/memory/agents/claude-code/robin/MEMORY.md"))
                .await
                .unwrap(),
            "# Memory\n- build needs cmake\n"
        );
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/..%2Fescape.md",
            &[("Authorization", auth.as_str())],
            "x",
        );
        assert_eq!(status, 400);
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/photo.png",
            &[("Authorization", auth.as_str())],
            "x",
        );
        assert_eq!(status, 400);
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "GET",
            "/v1/skill",
            &[("Authorization", auth.as_str())],
            "",
        );
        assert_eq!(status, 200);
        assert!(body.contains("name: thock-memory"));
    }

    #[gpui::test]
    async fn pausing_and_removing_the_routine_close_the_port(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let port = desk.port;
        desk.service.update(cx, |service, cx| service.toggle(cx));
        cx.run_until_parked();
        assert_eq!(
            desk.service.read_with(cx, |service, _| service.state().clone()),
            BridgeState::Paused
        );
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "the port is closed");
        desk.service.update(cx, |service, cx| service.toggle(cx));
        cx.run_until_parked();
        let reopened = desk.service.read_with(cx, |service, _| service.state().clone());
        assert!(matches!(reopened, BridgeState::Serving { .. }), "{reopened:?}");

        // The Routine removed: the registry rewrite is a config change.
        desk.service.update(cx, |service, cx| {
            service.enabled = false;
            service.apply_state(cx);
        });
        cx.run_until_parked();
        assert_eq!(
            desk.service.read_with(cx, |service, _| service.state().clone()),
            BridgeState::Off
        );
    }

    #[gpui::test]
    async fn a_port_in_use_is_a_visible_failure(cx: &mut TestAppContext) {
        init_test(cx);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let fs = FakeFs::new(cx.executor());
        let mut tree = vault_tree();
        tree[".thock"]["routines"]["agents"]["bridge.toml"] =
            json!(format!("port = {port}\nbind = \"127.0.0.1\"\n"));
        fs.insert_tree("/vault", tree).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = cx.update(|cx| BridgeService::attach_for_test(vault(true), &project, cx));
        cx.run_until_parked();
        let state = service.read_with(cx, |service, _| service.state().clone());
        assert_eq!(
            state,
            BridgeState::Failed {
                port,
                error: format!("port {port} is in use")
            }
        );
        assert!(state.label(0).starts_with("Not serving: port"));
        drop(listener);
        assert_eq!(
            BridgeState::Serving {
                address: "palloma.local".into(),
                port: 7557
            }
            .label(2),
            "Serving on palloma.local:7557 · 2 agents"
        );
    }
}
