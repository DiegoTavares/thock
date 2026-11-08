//! The Agents Routine's server inside the app (spec `v55-agents-bridge.md`
//! §11): while a vault with the Routine is open, the desk serves the vault's
//! memory over a small local HTTP endpoint that the `thock-bridge` client on
//! the person's other machines speaks to. The contract — routes, tools,
//! filters, lines — is `thock_bridge`; this module binds the port, checks
//! tokens, and implements the tools over the project `Fs`.

use anyhow::{Context as _, Result, anyhow};
use chrono::{Local, NaiveDate, Utc};
use fs::Fs;
use futures::channel::{mpsc, oneshot};
use futures::future::{Shared as SharedFuture, select};
use futures::{FutureExt as _, StreamExt as _};
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EntityId, Global, Task};
use project::Project;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::BufReader;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
pub use crate::routines::AGENTS_ROUTINE_ID;
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

/// Port and bind address, created on activation (§6.1).
pub const BRIDGE_CONFIG_PATH: &str = ".thock/routines/agents/bridge.toml";
pub const CLIENTS_PATH: &str = ".thock/state/bridge/clients.toml";
pub const PENDING_PATH: &str = ".thock/state/bridge/pending.toml";
pub const SEEN_PATH: &str = ".thock/state/bridge/seen.jsonl";
const PROFILE_PATH: &str = "profile.md";
/// What one authenticated request may carry; a mirrored file is capped
/// lower (§9.6) and an unpaired peer's `/v1/pair` lower still.
const BODY_LIMIT: usize = 2 * 1024 * 1024;
const PAIR_BODY_LIMIT: usize = 1024;
/// A connection's whole budget, and the head's share of it: an absolute
/// deadline, so a peer trickling a byte at a time holds a worker for this
/// long and no longer (R1).
const HEAD_DEADLINE: Duration = Duration::from_secs(10);
const CONNECTION_DEADLINE: Duration = Duration::from_secs(30);
/// An unpaired peer's `/v1/pair` body is a few hundred bytes; it gets this
/// long after its head, not the paired budget.
const PAIR_BODY_DEADLINE: Duration = Duration::from_secs(5);
/// Connections answered at once; the rest get a 503 rather than a queue.
const MAX_WORKERS: usize = 16;
/// Connections one address may hold before its head has arrived, so a
/// trickling peer cannot take the workers a paired client needs.
const MAX_PREAUTH_PER_IP: usize = 2;
/// How long a resume waits for the previous listener's thread to end
/// before saying so (R5).
const STOP_DEADLINE: Duration = Duration::from_secs(5);
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

/// Opens a connected computer's details — when it paired, when it last
/// called, which agent — with Disconnect behind a confirmation. The
/// Routines panel's machine rows and the Disconnect agent picker open it.
#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = thock)]
#[serde(deny_unknown_fields)]
pub struct ShowAgentDetails {
    pub name: String,
}

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
        register_workspace_actions(workspace);
    })
    .detach();
}

/// The palette actions, looked up against the workspace's service when they
/// run; split from `init` so a test can register them over a test service.
pub(crate) fn register_workspace_actions(workspace: &mut Workspace) {
    {
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
        workspace.register_action(|workspace, action: &ShowAgentDetails, window, cx| {
            let Some(service) = service_for_project(workspace.project(), cx) else {
                return;
            };
            let Some(client) = service
                .read(cx)
                .clients()
                .into_iter()
                .find(|client| client.name == action.name)
            else {
                workspace.show_error(
                    format!("{} isn't connected to this vault.", action.name),
                    cx,
                );
                return;
            };
            let service = service.downgrade();
            workspace.toggle_modal(window, cx, |window, cx| {
                crate::agent_details::AgentDetailsModal::new(service, client, window, cx)
            });
        });
    }
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
    Serving {
        address: String,
        port: u16,
    },
    Failed {
        port: u16,
        error: String,
    },
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
    pub paired_at: String,
    /// The harness behind its last call, empty before the first.
    pub last_harness: String,
    /// The desk address it paired against, as it typed it.
    pub paired_via: String,
}

impl ClientView {
    /// `robin · last seen 3 min ago`, or just the name before the first call.
    pub fn label(&self, now: chrono::DateTime<Utc>) -> String {
        match self.last_seen_ago(now) {
            Some(ago) => format!("{} · last seen {ago}", self.name),
            None => self.name.clone(),
        }
    }

    /// `just now`, `3 min ago`, `2 h ago`, `5 days ago`; `None` before the
    /// first call.
    pub fn last_seen_ago(&self, now: chrono::DateTime<Utc>) -> Option<String> {
        let seen = chrono::DateTime::parse_from_rfc3339(&self.last_seen).ok()?;
        let minutes = (now - seen.with_timezone(&Utc)).num_minutes();
        Some(if minutes < 1 {
            "just now".to_string()
        } else if minutes < 60 {
            format!("{minutes} min ago")
        } else if minutes < 60 * 24 {
            format!("{} h ago", minutes / 60)
        } else {
            format!("{} days ago", minutes / (60 * 24))
        })
    }

    /// The day it paired, `2026-11-04`, or the raw value when unreadable.
    pub fn paired_on(&self) -> String {
        chrono::DateTime::parse_from_rfc3339(&self.paired_at)
            .map(|at| {
                at.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_else(|_| self.paired_at.clone())
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
    /// Where a connection reaches the accept loop, to wake it for a stop.
    wake: SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Listener {
    /// Asks the accept loop to stop and joins it off the foreground, so a
    /// stop never waits on a request still in flight (H1).
    fn stop(mut self, cx: &App) -> Task<()> {
        self.signal();
        let thread = self.thread.take();
        cx.background_spawn(async move {
            if let Some(thread) = thread
                && thread.join().is_err()
            {
                log::warn!("Thock: the agents bridge listener thread panicked");
            }
        })
    }

    fn signal(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // `accept` blocks; one connection from here lets it see the flag.
        TcpStream::connect_timeout(&self.wake, Duration::from_millis(200)).ok();
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.signal();
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
    /// The `Host` header: the desk address the machine reached.
    host: String,
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
    // Declared before `listener` so an entity drop releases the in-flight
    // replies (and the workers waiting on them) before the accept loop is
    // told to stop.
    receiver_task: Option<Task<()>>,
    listener: Option<Listener>,
    /// The previous listener's join, awaited before the next bind so the
    /// port is free again (a pause followed by a resume).
    stopping: Option<SharedFuture<Task<()>>>,
    /// The one writer of `clients.toml`: each write waits for the previous
    /// and renders the list as it is at that moment, so a stale snapshot
    /// can never put a revoked token back on disk.
    clients_write: Option<SharedFuture<Task<Result<(), String>>>>,
    start_task: Option<Task<()>>,
    /// Shared with the background thread a query runs on; the foreground
    /// only writes it, one entry per file event.
    index: Arc<std::sync::RwLock<Option<SearchIndex>>>,
    seen: Rc<RefCell<SeenCalls>>,
    /// How many history checkpoints the bridge asked for, so a test can see
    /// that every write that changes a file took one first.
    checkpoints: Rc<std::cell::Cell<usize>>,
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
            receiver_task: None,
            listener: None,
            stopping: None,
            clients_write: None,
            start_task: None,
            index: Arc::new(std::sync::RwLock::new(None)),
            seen: Rc::new(RefCell::new(SeenCalls::default())),
            checkpoints: Rc::new(std::cell::Cell::new(0)),
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

    /// Whether a pairing code is still waiting to be typed somewhere.
    pub fn has_pending_code(&self) -> bool {
        self.shared
            .lock()
            .map(|shared| shared.pending.is_some())
            .unwrap_or(false)
    }

    /// After the service has started: `start` replaces the client list
    /// with the one on disk.
    #[cfg(test)]
    pub(crate) fn add_client_for_test(&mut self, name: &str, token: &str, cx: &mut Context<Self>) {
        cx.notify();
        if let Ok(mut shared) = self.shared.lock() {
            shared.clients.push(PairedClient {
                name: name.to_string(),
                token_sha256: thock_bridge::sha256_hex(token),
                paired_at: "2026-11-04T08:00:00Z".into(),
                last_seen: "2026-11-04T08:00:00Z".into(),
                last_harness: "claude-code".into(),
                paired_via: "palloma.local:7557".into(),
            });
        }
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
                        paired_at: client.paired_at.clone(),
                        last_harness: client.last_harness.clone(),
                        paired_via: client.paired_via.clone(),
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
                let mut bridge_config_changed = false;
                for (path, _, change) in changes.iter() {
                    let path = path.as_unix_str();
                    if path == vault_config {
                        reload = true;
                    } else if path == BRIDGE_CONFIG_PATH {
                        bridge_config_changed = true;
                    } else if SearchIndex::indexes(path) {
                        self.index_changed(
                            path,
                            matches!(change, project::PathChange::Removed),
                            cx,
                        );
                    }
                }
                if reload {
                    self.reload(cx);
                } else if bridge_config_changed {
                    self.reload_bridge_config(cx);
                }
            }
            _ => {}
        }
    }

    /// `bridge.toml` was edited while serving: a new port or bind address
    /// rebinds, a new daily heading applies in place (the connect-agent
    /// skill promises "the desk rebinds on save").
    fn reload_bridge_config(&mut self, cx: &mut Context<Self>) {
        if self.listener.is_none() {
            self.apply_state(cx);
            return;
        }
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.fs(cx);
        cx.spawn(async move |this, cx| {
            let path = vault.root.join(BRIDGE_CONFIG_PATH);
            let config = match fs.load(&path).await {
                Ok(raw) => match BridgeConfig::parse(&raw) {
                    Ok(config) => config,
                    Err(error) => {
                        log::warn!("Thock: {BRIDGE_CONFIG_PATH} is not readable: {error}");
                        return;
                    }
                },
                // Deleted: the next start writes the defaults back.
                Err(_) => BridgeConfig::default(),
            };
            this.update(cx, |service, cx| {
                let rebind =
                    config.port != service.config.port || config.bind != service.config.bind;
                service.config = config;
                if rebind {
                    service.stop(BridgeState::Starting, cx);
                    service.apply_state(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keeps the search index current from the worktree's events (§9.3).
    /// Nothing happens before a first `search` builds it.
    fn index_changed(&mut self, path: &str, removed: bool, cx: &mut Context<Self>) {
        if self.index.read().ok().is_none_or(|index| index.is_none()) {
            return;
        }
        if removed {
            if let Ok(mut index) = self.index.write()
                && let Some(index) = index.as_mut()
            {
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
                && let Ok(mut index) = index.write()
                && let Some(index) = index.as_mut()
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
        let changed_root = self.vault.as_ref().map(|v| v.root.as_path())
            != vault.as_ref().map(|v| v.root.as_path());
        self.enabled = vault.as_ref().is_some_and(routine_enabled);
        self.vault = vault;
        if changed_root {
            self.stop(BridgeState::Off, cx);
            if let Ok(mut index) = self.index.write() {
                *index = None;
            }
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
            self.stop(state, cx);
            cx.notify();
            return;
        }
        if self.listener.is_some() || self.start_task.is_some() {
            return;
        }
        self.start(cx);
    }

    /// The replies go first: dropping `receiver_task` closes every pending
    /// request's channel, so a worker blocked on one answers 503 at once
    /// instead of holding the stop (H1).
    fn stop(&mut self, state: BridgeState, cx: &App) {
        self.receiver_task = None;
        if let Some(listener) = self.listener.take() {
            // Chained behind any earlier stop still joining, and kept here
            // rather than in the start task, so a resume cancelled by the
            // next pause never loses the join (R4).
            let previous = self.stopping.take();
            let join = listener.stop(cx);
            self.stopping = Some(
                cx.background_spawn(async move {
                    if let Some(previous) = previous {
                        previous.await;
                    }
                    join.await;
                })
                .shared(),
            );
        }
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
        let stopping = self.stopping.clone();
        let port = self.config.port;
        self.state = BridgeState::Starting;
        cx.notify();
        self.start_task = Some(cx.spawn(async move |this, cx| {
            if let Some(stopping) = stopping {
                let timeout = cx.background_executor().timer(STOP_DEADLINE);
                if let futures::future::Either::Right(_) =
                    select(stopping, std::pin::pin!(timeout)).await
                {
                    this.update(cx, |service, cx| {
                        service.start_task = None;
                        service.state = BridgeState::Failed {
                            port,
                            error: "the previous listener hasn't stopped yet; try Toggle \
                                    Bridge again"
                                .into(),
                        };
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
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
        let listener = match TcpListener::bind(&address) {
            Ok(listener) => listener,
            Err(error) => {
                let error = if error.kind() == std::io::ErrorKind::AddrInUse {
                    format!("port {} is in use", self.config.port)
                } else {
                    error.to_string()
                };
                log::warn!("Thock: the agents bridge couldn't bind {address}: {error}");
                self.state = BridgeState::Failed {
                    port: self.config.port,
                    error,
                };
                return;
            }
        };
        let bound = match listener.local_addr() {
            Ok(bound) => bound,
            Err(error) => {
                self.state = BridgeState::Failed {
                    port: self.config.port,
                    error: error.to_string(),
                };
                return;
            }
        };
        let port = bound.port();
        let wake = match bound.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::from(([127, 0, 0, 1], port)),
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port))
            }
            _ => bound,
        };
        let (sender, mut receiver) = mpsc::unbounded::<(Incoming, oneshot::Sender<Outgoing>)>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("thock-bridge-listener".into())
            .spawn({
                let shutdown = shutdown.clone();
                let shared = self.shared.clone();
                move || listen(listener, shutdown, shared, sender)
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
            wake,
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
            _ => {
                return Task::ready(Err(anyhow!(
                    "the desk isn't serving yet; try again in a moment"
                )));
            }
        };
        let code = thock_bridge::generate_code();
        let pending = PendingCode {
            code_sha256: thock_bridge::sha256_hex(&code),
            expires_at: (Utc::now()
                + chrono::Duration::minutes(thock_bridge::PAIRING_CODE_MINUTES))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        };
        if let Ok(mut shared) = self.shared.lock() {
            shared.pending = Some(pending.clone());
        }
        let fs = self.fs(cx);
        cx.background_spawn(async move {
            write_state_file(&fs, &vault.root, PENDING_PATH, &pending.render()).await?;
            Ok(PairingInvite {
                code,
                addresses: desk_addresses(port),
                port,
                expires_at: pending.expires_at,
            })
        })
    }

    /// Forgets a paired machine; its token stops working at once.
    pub fn disconnect(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.clients.retain(|client| client.name != name);
        }
        self.persist_clients_later(cx);
        cx.notify();
    }

    /// `persist_clients` for a caller with nothing to wait for: the write
    /// runs on the executor whether or not its handle is polled, and
    /// `clients_write` keeps it alive; a failure is logged by the task.
    fn persist_clients_later(&mut self, cx: &App) {
        drop(self.persist_clients(cx));
    }

    /// Queues one write of `clients.toml` behind any write still running.
    /// The list is read under the lock when the write runs, not when it is
    /// asked for.
    fn persist_clients(&mut self, cx: &App) -> SharedFuture<Task<Result<(), String>>> {
        let previous = self.clients_write.take();
        let shared = self.shared.clone();
        let fs = self.fs(cx);
        let root = self
            .vault
            .as_ref()
            .map(|vault| vault.root.clone())
            .unwrap_or_default();
        let write = cx
            .background_spawn(async move {
                if let Some(previous) = previous {
                    previous.await.ok();
                }
                if root.as_os_str().is_empty() {
                    return Err("the vault is closed".to_string());
                }
                let clients = shared
                    .lock()
                    .map(|shared| shared.clients.clone())
                    .map_err(|_| "the client list is poisoned".to_string())?;
                let written = write_state_file(
                    &fs,
                    &root,
                    CLIENTS_PATH,
                    &thock_bridge::render_clients(&clients),
                )
                .await
                .map_err(|error| format!("{error:#}"));
                if let Err(error) = &written {
                    log::error!("Thock: couldn't write {CLIENTS_PATH}: {error}");
                }
                written
            })
            .shared();
        self.clients_write = Some(write.clone());
        write
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

/// The `.local` name first when mDNS resolves it here (so it most likely
/// does on the other machine too), else the first non-loopback IPv4 the
/// machine would route out through, with the name second as a hint (§7).
/// Resolving blocks: call from a background thread.
fn desk_addresses(port: u16) -> Vec<String> {
    let local_name = format!("{}.local", desk_name());
    let mut addresses = Vec::new();
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
        && socket.connect("10.255.255.255:1").is_ok()
        && let Ok(local) = socket.local_addr()
        && !local.ip().is_loopback()
    {
        addresses.push(local.ip().to_string());
    }
    let resolves = (local_name.as_str(), port)
        .to_socket_addrs()
        .map(|mut found| found.next().is_some())
        .unwrap_or(false);
    if resolves {
        addresses.insert(0, local_name);
    } else {
        addresses.push(local_name);
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

/// The accept loop: every connection goes to a worker of its own, so a
/// peer that stalls on its body holds one worker for `SOCKET_TIMEOUT` and
/// never the loop (H2).
fn listen(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    sender: mpsc::UnboundedSender<(Incoming, oneshot::Sender<Outgoing>)>,
) {
    let workers = Arc::new(AtomicUsize::new(0));
    let preauth: Arc<Mutex<HashMap<IpAddr, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                log::warn!("Thock: the agents bridge couldn't accept a connection: {error}");
                continue;
            }
        };
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        if workers.load(Ordering::SeqCst) >= MAX_WORKERS {
            let mut stream = stream;
            thock_bridge::write_response(
                &mut stream,
                503,
                "application/json",
                thock_bridge::response_json("the desk is busy; try again in a moment").as_bytes(),
            )
            .ok();
            continue;
        }
        let peer_ip = peer.ip();
        let too_many = preauth.lock().ok().is_some_and(|mut counts| {
            let count = counts.entry(peer_ip).or_insert(0);
            if *count >= MAX_PREAUTH_PER_IP {
                true
            } else {
                *count += 1;
                false
            }
        });
        if too_many {
            let mut stream = stream;
            thock_bridge::write_response(
                &mut stream,
                503,
                "application/json",
                thock_bridge::response_json("too many connections from this address at once")
                    .as_bytes(),
            )
            .ok();
            continue;
        }
        workers.fetch_add(1, Ordering::SeqCst);
        let spawned = std::thread::Builder::new()
            .name("thock-bridge-worker".into())
            .spawn({
                let workers = workers.clone();
                let preauth = preauth.clone();
                let shared = shared.clone();
                let sender = sender.clone();
                move || {
                    // Released once, the first time it is called: after the
                    // token checks out, or when the request is answered.
                    let released = std::cell::Cell::new(false);
                    let release = || {
                        if !released.replace(true) {
                            release_preauth(&preauth, peer_ip);
                        }
                    };
                    handle_connection(stream, &shared, &sender, &release);
                    workers.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if spawned.is_err() {
            workers.fetch_sub(1, Ordering::SeqCst);
            release_preauth(&preauth, peer_ip);
        }
    }
}

fn release_preauth(preauth: &Arc<Mutex<HashMap<IpAddr, usize>>>, ip: IpAddr) {
    if let Ok(mut counts) = preauth.lock()
        && let Some(count) = counts.get_mut(&ip)
    {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(&ip);
        }
    }
}

/// One connection, one request: route and authenticate from the head
/// before a byte of body is read, cap the body by who is asking, hand the
/// rest to the foreground, write the reply. `release` frees the peer's
/// pre-auth slot; it is called once the token checks out, else when the
/// request has been answered, so an unpaired peer holds its slot for the
/// whole of its (short) budget.
fn handle_connection(
    mut stream: TcpStream,
    shared: &Arc<Mutex<Shared>>,
    sender: &mpsc::UnboundedSender<(Incoming, oneshot::Sender<Outgoing>)>,
    release: &dyn Fn(),
) {
    let opened = std::time::Instant::now();
    stream.set_write_timeout(Some(CONNECTION_DEADLINE)).ok();
    let outgoing = {
        let Ok(reading) = stream.try_clone() else {
            release();
            return;
        };
        let mut reader = BufReader::new(thock_bridge::DeadlineStream::new(
            reading,
            opened + HEAD_DEADLINE,
        ));
        match thock_bridge::read_request_head(&mut reader) {
            Ok(head) => answer(head, &mut reader, opened, shared, sender, release),
            Err(status) => Outgoing::error(status, "the desk couldn't read that request"),
        }
    };
    release();
    let written = thock_bridge::write_response(
        &mut stream,
        outgoing.status,
        outgoing.content_type,
        &outgoing.body,
    );
    if let Err(error) = written {
        log::debug!("Thock: couldn't answer an agents bridge request: {error}");
    }
}

fn answer(
    head: thock_bridge::RequestHead,
    reader: &mut BufReader<thock_bridge::DeadlineStream>,
    opened: std::time::Instant,
    shared: &Arc<Mutex<Shared>>,
    sender: &mpsc::UnboundedSender<(Incoming, oneshot::Sender<Outgoing>)>,
    release: &dyn Fn(),
) -> Outgoing {
    let route = thock_bridge::parse_route(&head.method, &head.url);
    let client = match route {
        Route::NotFound => return Outgoing::error(404, "nothing here"),
        Route::McpNotPost => return Outgoing::error(405, "POST one JSON-RPC request to /mcp"),
        Route::Pair => None,
        _ => {
            let token = thock_bridge::bearer_token(
                head.headers
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            let hash = token.map(thock_bridge::sha256_hex);
            let known = shared.lock().ok().and_then(|shared| {
                shared
                    .clients
                    .iter()
                    .find(|client| {
                        hash.as_deref().is_some_and(|hash| {
                            thock_bridge::constant_time_eq(hash, &client.token_sha256)
                        })
                    })
                    .map(|client| client.name.clone())
            });
            match known {
                Some(name) => Some(name),
                None => return Outgoing::error(401, "unknown client; pair again from the desk"),
            }
        }
    };
    let limit = if client.is_some() {
        release();
        reader.get_mut().set_deadline(opened + CONNECTION_DEADLINE);
        BODY_LIMIT
    } else {
        reader
            .get_mut()
            .set_deadline(std::time::Instant::now() + PAIR_BODY_DEADLINE);
        PAIR_BODY_LIMIT
    };
    if head.content_length > limit {
        return Outgoing::error(413, "that is more than the desk takes in one request");
    }
    let body = match thock_bridge::read_body(reader, head.content_length) {
        Ok(body) => body,
        Err(status) => return Outgoing::error(status, "the request's body didn't arrive in time"),
    };
    let harness = head.header(thock_bridge::HARNESS_HEADER).unwrap_or("agent");
    let host: String = head
        .header("host")
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_' | '[' | ']'))
        .take(120)
        .collect();
    let incoming = Incoming {
        route,
        client,
        harness: thock_bridge::sanitize_harness(harness),
        host,
        body,
    };
    let (reply_sender, reply_receiver) = oneshot::channel();
    if sender.unbounded_send((incoming, reply_sender)).is_err() {
        return Outgoing::error(503, "the desk is closing");
    }
    match futures::executor::block_on(reply_receiver) {
        Ok(outgoing) => outgoing,
        Err(_) => Outgoing::error(503, "the desk is closing"),
    }
}

/// What the foreground needs per request, read in one entity update.
struct RequestContext {
    fs: Arc<dyn Fs>,
    vault: Vault,
    config: BridgeConfig,
    shared: Arc<Mutex<Shared>>,
    index: Arc<std::sync::RwLock<Option<SearchIndex>>>,
    seen: Rc<RefCell<SeenCalls>>,
    checkpoints: Rc<std::cell::Cell<usize>>,
    project: Entity<Project>,
    serving_since: String,
}

/// The restore point every bridge write that changes a file takes first,
/// like the hosted agent's `append`.
fn take_checkpoint(context: &RequestContext, cx: &mut AsyncApp) {
    context.checkpoints.set(context.checkpoints.get() + 1);
    let project = context.project.clone();
    cx.update(|cx| history::checkpoint_before_ai_write(&project, cx));
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
            checkpoints: service.checkpoints.clone(),
            project: service.project.clone(),
            serving_since: service.serving_since.clone(),
        })
    });
    let Some(context) = context else {
        return Outgoing::error(503, "the vault is closed");
    };
    let outgoing = match incoming.route {
        Route::Pair => handle_pair(&context, &incoming.body, &incoming.host, &service, cx).await,
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
            handle_agent_file(
                &context,
                incoming.client.as_deref(),
                &harness,
                &path,
                &incoming.body,
                cx,
            )
            .await
        }
        Route::NotFound => Outgoing::error(404, "nothing here"),
    };
    if let Some(name) = incoming.client {
        let persist = touch_client(&context, &name, &incoming.harness);
        service.update(cx, |service, cx| {
            if persist {
                service.persist_clients_later(cx);
            }
            cx.notify();
        });
    }
    outgoing
}

/// Records when a paired machine last called; says whether the list is
/// worth persisting (once a minute per client).
fn touch_client(context: &RequestContext, name: &str, harness: &str) -> bool {
    let now = Utc::now();
    let Ok(mut shared) = context.shared.lock() else {
        return false;
    };
    let Some(client) = shared.clients.iter_mut().find(|client| client.name == name) else {
        return false;
    };
    let stale = chrono::DateTime::parse_from_rfc3339(&client.last_seen)
        .map(|seen| (now - seen.with_timezone(&Utc)).num_seconds() >= 60)
        .unwrap_or(true);
    let harness_changed = client.last_harness != harness;
    client.last_seen = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    client.last_harness = harness.to_string();
    stale || harness_changed
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
    host: &str,
    service: &Entity<BridgeService>,
    cx: &mut AsyncApp,
) -> Outgoing {
    let Ok(request) = serde_json::from_slice::<PairRequest>(body) else {
        return Outgoing::error(400, "pair needs {\"code\", \"name\"}");
    };
    let name = request.name.trim();
    let plain_host_name = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !plain_host_name {
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
                last_harness: String::new(),
                paired_via: host.to_string(),
            });
            shared.clients.clone()
        }
        Err(_) => return Outgoing::error(500, "the desk couldn't record the pairing"),
    };
    drop(clients);
    let persisted = service
        .update(cx, |service, cx| service.persist_clients(cx))
        .await;
    if let Err(error) = persisted {
        log::error!("Thock: couldn't write {CLIENTS_PATH}: {error}");
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
    cx: &mut AsyncApp,
) -> Outgoing {
    let Some(client) = client else {
        return Outgoing::error(401, "unknown client; pair again from the desk");
    };
    if body.len() as u64 > thock_bridge::MIRROR_MAX_BYTES {
        return Outgoing::error(413, "mirrored files stay under 1 MiB");
    }
    if !thock_bridge::is_mirrored_path(path) {
        return Outgoing::error(
            400,
            "only .md, .txt and .json files at plain paths are mirrored",
        );
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
    if !resolves_inside(&context.fs, &context.vault.root, &absolute).await {
        return Outgoing::error(400, "that path points outside the vault");
    }
    let Ok(text) = String::from_utf8(body.to_vec()) else {
        return Outgoing::error(400, "mirrored files must be UTF-8 text");
    };
    let forbidden = match context
        .fs
        .load(&context.vault.root.join(PROFILE_PATH))
        .await
    {
        Ok(profile) => thock_bridge::forbidden_topics(&profile),
        Err(_) => Vec::new(),
    };
    if let Some(reason) = thock_bridge::find_unkeepable(&text, &forbidden) {
        return Outgoing::error(422, &format!("{path}: {reason}; the vault doesn't keep it"));
    }
    // The same bytes again (a mirror pass re-sending an unchanged file)
    // change nothing: no checkpoint, no write, no file event.
    if context.fs.is_file(&absolute).await
        && context.fs.load(&absolute).await.ok().as_deref() == Some(text.as_str())
    {
        return Outgoing::empty(204);
    }
    take_checkpoint(context, cx);
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

/// Whether `path`, once its deepest existing ancestor is canonicalised,
/// still sits under the vault root (§9.1): a symlink out of the vault is
/// refused whether the file exists yet or not.
async fn resolves_inside(fs: &Arc<dyn Fs>, root: &Path, path: &Path) -> bool {
    let canonical_root = fs
        .canonicalize(root)
        .await
        .unwrap_or_else(|_| root.to_path_buf());
    let mut probe = path.to_path_buf();
    loop {
        match fs.canonicalize(&probe).await {
            Ok(canonical) => return canonical.starts_with(&canonical_root),
            Err(_) => match probe.parent() {
                Some(parent) if parent.starts_with(root) && parent != root => {
                    probe = parent.to_path_buf()
                }
                _ => return true,
            },
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
        if self
            .context
            .index
            .read()
            .ok()
            .is_some_and(|index| index.is_some())
        {
            return Ok(());
        }
        let index = build_index(&self.context.fs, self.root()).await?;
        if let Ok(mut slot) = self.context.index.write() {
            *slot = Some(index);
        }
        Ok(())
    }

    /// Runs a read over the index off the foreground thread, so a big vault
    /// never stalls the app for a query.
    async fn with_index<R: Send + 'static>(
        &self,
        read: impl FnOnce(&SearchIndex) -> R + Send + 'static,
    ) -> Option<R> {
        let index = self.context.index.clone();
        self.cx
            .background_spawn(async move {
                let guard = index.read().ok()?;
                guard.as_ref().map(read)
            })
            .await
    }

    /// The absolute path of a vault-relative note, refused when it leaves
    /// the vault (§9.1).
    async fn note_path(&self, relative: &str) -> Result<PathBuf, HostError> {
        thock_bridge::validate_vault_path(relative)
            .map_err(|error| HostError::Refused(error.to_string()))?;
        let absolute = self.root().join(relative.trim());
        if !resolves_inside(&self.context.fs, self.root(), &absolute).await {
            return Err(HostError::Refused(format!(
                "{relative} points outside the vault"
            )));
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
        write_state_file(
            &self.context.fs,
            self.root(),
            SEEN_PATH,
            &format!("{lines}\n"),
        )
        .await
        .log_err();
    }

    /// Applies a sync-core write to a note through the `Fs`, after the
    /// history checkpoint every AI write takes.
    async fn apply_write(
        &mut self,
        write: &Write,
        seed: Option<String>,
    ) -> Result<WriteOutcome, HostError> {
        let absolute = self.root().join(&write.path);
        let existing = if self.context.fs.is_file(&absolute).await {
            Some(
                self.context
                    .fs
                    .load(&absolute)
                    .await
                    .map_err(HostError::from)?,
            )
        } else {
            None
        };
        let applied = apply(existing.as_deref(), write, seed.as_deref());
        if applied.outcome == Outcome::Noop || existing.as_deref() == Some(applied.text.as_str()) {
            return Ok(WriteOutcome::Noop);
        }
        take_checkpoint(self.context, &mut self.cx);
        if let Some(parent) = absolute.parent() {
            self.context
                .fs
                .create_dir(parent)
                .await
                .map_err(HostError::from)?;
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
        Ok(crate::hosted_agent::bridge_context(
            &base,
            &self.identity,
            &forbidden,
            &self.context.config.today_heading,
        ))
    }

    async fn memory_index(&mut self) -> Result<String, HostError> {
        let lines = self.context.vault.config.memory.index_lines;
        Ok(
            crate::memory::read_index_capped(&self.context.fs, self.root(), lines)
                .await
                .unwrap_or_else(|| {
                    "Nothing yet: no session has learned anything about them.".into()
                }),
        )
    }

    async fn search(&mut self, query: &SearchQuery) -> Result<Vec<SearchHit>, HostError> {
        self.ensure_index().await?;
        let today = Local::now().date_naive();
        let query = query.clone();
        Ok(self
            .with_index(move |index| index.search(&query, today))
            .await
            .unwrap_or_default())
    }

    async fn read(&mut self, target: &ReadTarget) -> Result<String, HostError> {
        let relative = match target {
            ReadTarget::Path(path) => path.trim().to_string(),
            ReadTarget::Link(link) => {
                self.ensure_index().await?;
                let wanted = link.clone();
                let resolution = self
                    .with_index(move |index| index.resolve_link(&wanted))
                    .await
                    .unwrap_or(thock_bridge::LinkResolution::Missing);
                match resolution {
                    thock_bridge::LinkResolution::Found(path) => path,
                    thock_bridge::LinkResolution::Ambiguous(paths) => {
                        return Err(HostError::Refused(format!(
                            "{link} names more than one note: {}",
                            paths.join(", ")
                        )));
                    }
                    thock_bridge::LinkResolution::Missing => {
                        return Err(HostError::Refused(format!(
                            "no note is titled or named {link}"
                        )));
                    }
                }
            }
        };
        let absolute = self.note_path(&relative).await?;
        if !self.context.fs.is_file(&absolute).await {
            return Err(HostError::Refused(format!(
                "{relative} is not in the vault"
            )));
        }
        self.context
            .fs
            .load(&absolute)
            .await
            .map_err(HostError::from)
    }

    async fn recent(
        &mut self,
        days: u32,
        folder: Option<&str>,
    ) -> Result<Vec<RecentNote>, HostError> {
        self.ensure_index().await?;
        let today = Local::now().date_naive();
        let folder = folder.map(str::to_string);
        Ok(self
            .with_index(move |index| index.recent(days, folder.as_deref(), today))
            .await
            .unwrap_or_default())
    }

    async fn remember(&mut self, fact: &str, meta: &CallMeta) -> Result<WriteOutcome, HostError> {
        if let Some(outcome) = self.seen(meta).await {
            return Ok(outcome);
        }
        let forbidden = self.forbidden_topics().await;
        let fact = thock_bridge::check_fact(fact, &forbidden)?;
        // A queued line keeps its day within the window; an older or future
        // date is not one the agent may label, so today's stands in (M1).
        let today = Local::now().date_naive();
        let date = thock_bridge::accepted_call_date(meta.date, today).unwrap_or(today);
        let line = thock_bridge::inbox_line(date, &fact, &self.identity);
        let inbox = self.root().join(thock_bridge::INBOX_PATH);
        if let Ok(existing) = self.context.fs.load(&inbox).await
            && existing.lines().any(|existing_line| {
                thock_bridge::parse_inbox_line(existing_line)
                    .is_some_and(|parsed| parsed.fact.eq_ignore_ascii_case(&fact))
            })
        {
            return Err(HostError::Refused(
                "that line is already in the inbox".into(),
            ));
        }
        let write = thock_bridge::inbox_write(WRITE_CLIENT_ID, &line);
        let outcome = self.apply_write(&write, None).await?;
        self.record_seen(meta, outcome).await;
        Ok(outcome)
    }

    async fn note_today(
        &mut self,
        lines: &[String],
        meta: &CallMeta,
    ) -> Result<WriteOutcome, HostError> {
        if let Some(outcome) = self.seen(meta).await {
            return Ok(outcome);
        }
        let forbidden = self.forbidden_topics().await;
        let mut rendered = Vec::with_capacity(lines.len());
        for line in lines {
            let line = thock_bridge::check_fact(line, &forbidden)?;
            rendered.push(thock_bridge::today_line(&self.identity, &line));
        }
        let today = Local::now().date_naive();
        let date = match meta.date {
            None => today,
            Some(asked) => {
                thock_bridge::accepted_call_date(Some(asked), today).ok_or_else(|| {
                    HostError::Refused(format!(
                        "{asked} is outside the {} days the desk writes into; only a queued line \
                     keeps its day, and only that far back",
                        thock_bridge::CALL_DATE_WINDOW_DAYS
                    ))
                })?
            }
        };
        let vault = &self.context.vault;
        let absolute = vault.note_path(NoteKind::Daily, date);
        let relative = absolute
            .strip_prefix(&vault.root)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|_| HostError::Refused("today's note is outside the vault".into()))?;
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
            Some(expand_template(
                &template,
                date,
                Local::now().time(),
                &title,
            ))
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
async fn read_index_entry(
    fs: &Arc<dyn Fs>,
    root: &Path,
    relative: &str,
) -> Option<(NaiveDate, String)> {
    let absolute = root.join(relative);
    let metadata = fs.metadata(&absolute).await.ok().flatten()?;
    if metadata.is_dir || metadata.is_symlink || metadata.len > BODY_LIMIT as u64 {
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
            // A link is never followed: it could lead out of the vault, or
            // back into it forever (M2).
            let Ok(Some(metadata)) = fs.metadata(&path).await else {
                continue;
            };
            if metadata.is_symlink {
                continue;
            }
            if metadata.is_dir {
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

    use std::io::Write as _;

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
            config.routines.installed.push(InstalledRoutine::new(
                AGENTS_ROUTINE_ID.to_string(),
                true,
                1,
            ));
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
        serve_bound(cx, enabled, "127.0.0.1").await
    }

    async fn serve_bound(cx: &mut TestAppContext, enabled: bool, bind: &str) -> Desk {
        init_test(cx);
        cx.executor().allow_parking();
        let fs = FakeFs::new(cx.executor());
        fs.set_next_mtime(std::time::SystemTime::now());
        let mut tree = vault_tree();
        tree[".thock"]["routines"]["agents"]["bridge.toml"] =
            json!(format!("port = 0\nbind = \"{bind}\"\n"));
        fs.insert_tree("/vault", tree).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = cx.update(|cx| BridgeService::attach_for_test(vault(enabled), &project, cx));
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
        let handle =
            std::thread::spawn(move || thock_bridge::request(&request, Duration::from_secs(10)));
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
        assert!(
            invite
                .command(&invite.addresses[0])
                .starts_with("thock-bridge pair ")
        );
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
            desk.service
                .read_with(cx, |service, _| service.state().clone()),
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
        let clients = desk
            .fs
            .load(Path::new("/vault/.thock/state/bridge/clients.toml"))
            .await
            .unwrap();
        assert!(clients.contains("name = \"robin\""));
        assert!(clients.contains("paired_via = \"127.0.0.1:"), "{clients}");
        let robin = desk
            .service
            .read_with(cx, |service, _| service.clients())
            .remove(0);
        assert!(robin.paired_via.starts_with("127.0.0.1:"));
        assert_eq!(
            robin.last_harness, "agent",
            "a health call named no harness"
        );
        assert!(
            !clients.contains(&token),
            "tokens are never stored in the clear"
        );
        assert!(
            !desk
                .fs
                .is_file(Path::new("/vault/.thock/state/bridge/pending.toml"))
                .await
        );

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
        assert!(
            desk.service
                .read_with(cx, |service, _| service.clients().is_empty())
        );
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
        let inbox = desk
            .fs
            .load(Path::new("/vault/memory/inbox.md"))
            .await
            .unwrap();
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
        let inbox_again = desk
            .fs
            .load(Path::new("/vault/memory/inbox.md"))
            .await
            .unwrap();
        assert_eq!(inbox, inbox_again);
        assert!(
            desk.fs
                .is_file(Path::new("/vault/.thock/state/bridge/seen.jsonl"))
                .await
        );

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
        assert_eq!(
            refused.first_text(),
            "that topic is on the person's do-not-keep list"
        );
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
        let note = desk
            .fs
            .load(Path::new("/vault/daily/2026-11-04.md"))
            .await
            .unwrap();
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
        assert_eq!(
            on_existing.first_text(),
            "Added to today's note (section_added)."
        );
        let yesterday = desk
            .fs
            .load(Path::new("/vault/daily/2026-11-03.md"))
            .await
            .unwrap();
        assert!(yesterday.starts_with("# Tuesday\n\n- Met Ana about the newsletter\n"));
        assert!(yesterday.contains("\n## From your agents\n"), "{yesterday}");
        assert!(
            yesterday.ends_with("- claude-code@robin · Yesterday's.\n"),
            "{yesterday}"
        );
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
        assert!(
            text.find("ana.md").unwrap() < text.find("2026-11-03.md").unwrap(),
            "title beats body"
        );

        let read = call_tool(cx, &desk, &token, "read", json!({"link": "Ana"}), None);
        assert_eq!(
            read.first_text(),
            "# Ana\n\n- Manager. Prefers written updates.\n"
        );
        let closed = call_tool(
            cx,
            &desk,
            &token,
            "read",
            json!({"path": ".thock/config.toml"}),
            None,
        );
        assert!(closed.is_error);
        let gone = call_tool(
            cx,
            &desk,
            &token,
            "read",
            json!({"path": "daily/2000-01-01.md"}),
            None,
        );
        assert!(gone.is_error);

        let recent = call_tool(cx, &desk, &token, "recent", json!({"days": 7}), None);
        assert!(
            recent
                .first_text()
                .contains("`daily/2026-11-03.md` — Tuesday")
        );

        let context = call_tool(cx, &desk, &token, "context", json!({}), None);
        let text = context.first_text();
        assert!(text.contains("# Right now"), "{text}");
        assert!(
            text.contains("**Ana**"),
            "the memory index rides along: {text}"
        );
        assert!(text.contains("you are claude-code@robin"), "{text}");
        assert!(
            text.contains("salary"),
            "the do-not-keep topics ride along: {text}"
        );

        // A new note shows up in the index through the worktree's events.
        desk.fs
            .insert_tree(
                "/vault/projects",
                json!({"newsletter.md": "# Newsletter\n\nAna owns it.\n"}),
            )
            .await;
        cx.run_until_parked();
        let hits = call_tool(
            cx,
            &desk,
            &token,
            "search",
            json!({"query": "newsletter"}),
            None,
        );
        assert!(
            hits.first_text()
                .contains("`projects/newsletter.md` — Newsletter"),
            "{}",
            hits.first_text()
        );
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
                .load(Path::new(
                    "/vault/memory/agents/claude-code/robin/MEMORY.md"
                ))
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
            desk.service
                .read_with(cx, |service, _| service.state().clone()),
            BridgeState::Paused
        );
        // The listener is gone with its thread joined; another test may
        // already have been handed the same port number, so the proof is
        // the service's own state, not a refused connection.
        assert!(
            desk.service
                .read_with(cx, |service, _| service.listener.is_none())
        );
        assert!(port > 0);
        desk.service.update(cx, |service, cx| service.toggle(cx));
        cx.run_until_parked();
        let reopened = desk
            .service
            .read_with(cx, |service, _| service.state().clone());
        assert!(
            matches!(reopened, BridgeState::Serving { .. }),
            "{reopened:?}"
        );

        // The Routine removed: the registry rewrite is a config change.
        desk.service.update(cx, |service, cx| {
            service.enabled = false;
            service.apply_state(cx);
        });
        cx.run_until_parked();
        assert_eq!(
            desk.service
                .read_with(cx, |service, _| service.state().clone()),
            BridgeState::Off
        );
    }

    /// A request already handed to the foreground when the bridge stops
    /// gets its 503 at once; the stop never waits on it (H1).
    #[gpui::test]
    async fn stopping_with_a_request_in_flight_returns_at_once(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let port = desk.port;
        let request = thock_bridge::HttpRequest {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}/v1/health"),
            headers: vec![("Authorization".into(), format!("Bearer {token}"))],
            body: Vec::new(),
        };
        // Not pumped: the worker has sent the request to the foreground and
        // is blocked on the reply when the pause lands.
        let pending =
            std::thread::spawn(move || thock_bridge::request(&request, Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(200));
        let started = std::time::Instant::now();
        desk.service.update(cx, |service, cx| service.toggle(cx));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the stop did not wait"
        );
        // The foreground still has to run the cancelled task's drop, which
        // is what releases the worker; the app's executor does that at once.
        while !pending.is_finished() {
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(2));
        }
        let response = pending.join().unwrap().unwrap();
        assert_eq!(response.status, 503);
        assert!(response.text().contains("the desk is closing"));
        cx.run_until_parked();
        assert_eq!(
            desk.service
                .read_with(cx, |service, _| service.state().clone()),
            BridgeState::Paused
        );
    }

    /// A peer that announces a body and never sends it holds one worker,
    /// not the desk: the next paired request is answered meanwhile (H2).
    #[gpui::test]
    async fn a_stalled_pair_body_does_not_block_the_desk(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let mut stalled = TcpStream::connect(("127.0.0.1", desk.port)).unwrap();
        stalled
            .write_all(b"POST /v1/pair HTTP/1.1\r\nHost: x\r\nContent-Length: 900\r\n\r\n")
            .unwrap();
        stalled.flush().unwrap();
        let auth = format!("Bearer {token}");
        let started = std::time::Instant::now();
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "GET",
            "/v1/health",
            &[("Authorization", auth.as_str())],
            "",
        );
        assert_eq!(status, 200);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "answered beside the stalled peer"
        );
        // An oversized pair body is refused from its head alone.
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "POST",
            "/v1/pair",
            &[],
            &"x".repeat(PAIR_BODY_LIMIT + 1),
        );
        assert_eq!(status, 413);
        drop(stalled);
    }

    /// Peers trickling their request head hold at most two workers per
    /// address and ten seconds each; a paired client is served meanwhile
    /// (R1). The tricklers come from `127.0.0.2`, which only Linux's
    /// loopback answers by default.
    #[cfg(target_os = "linux")]
    #[gpui::test]
    async fn trickled_heads_do_not_starve_paired_clients(cx: &mut TestAppContext) {
        use std::io::Read as _;

        let desk = serve_bound(cx, true, "0.0.0.0").await;
        let token = pair(cx, &desk, "robin").await;
        // Linux picks `127.0.0.1` as the source for any loopback target,
        // so the tricklers bind their own address first.
        let from_other_address = |port: u16| -> TcpStream {
            let socket =
                socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
            let source: SocketAddr = "127.0.0.2:0".parse().unwrap();
            socket.bind(&source.into()).unwrap();
            let target = SocketAddr::from(([127, 0, 0, 1], port));
            socket.connect(&target.into()).unwrap();
            socket.into()
        };
        let mut tricklers = Vec::new();
        for _ in 0..6 {
            let mut stream = from_other_address(desk.port);
            stream.write_all(b"GET /v1/hea").unwrap();
            stream.flush().unwrap();
            tricklers.push(stream);
        }
        let auth = format!("Bearer {token}");
        let started = std::time::Instant::now();
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "GET",
            "/v1/health",
            &[("Authorization", auth.as_str())],
            "",
        );
        assert_eq!(status, 200);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        // The extra tricklers from this address were turned away at once.
        let mut turned_away = 0;
        for stream in &mut tricklers {
            stream
                .set_read_timeout(Some(Duration::from_millis(300)))
                .ok();
            let mut buf = [0u8; 64];
            if matches!(stream.read(&mut buf), Ok(n) if n > 0) {
                turned_away += 1;
            }
        }
        assert_eq!(
            turned_away, 4,
            "two per address trickle, the rest get a 503"
        );
        // A complete `/v1/pair` head with no body holds its slot until it
        // is answered, so sixteen of them from one address can't take the
        // workers: two hold, the rest are turned away, the paired client
        // is served meanwhile.
        drop(tricklers);
        std::thread::sleep(Duration::from_millis(50));
        let mut headless: Vec<TcpStream> = Vec::new();
        for _ in 0..6 {
            let mut stream = from_other_address(desk.port);
            stream
                .write_all(b"POST /v1/pair HTTP/1.1\r\nHost: x\r\nContent-Length: 500\r\n\r\n")
                .unwrap();
            stream.flush().unwrap();
            headless.push(stream);
        }
        let started = std::time::Instant::now();
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "GET",
            "/v1/health",
            &[("Authorization", auth.as_str())],
            "",
        );
        assert_eq!(status, 200);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let mut turned_away = 0;
        for stream in &mut headless {
            stream
                .set_read_timeout(Some(Duration::from_millis(300)))
                .ok();
            let mut buf = [0u8; 64];
            if matches!(stream.read(&mut buf), Ok(n) if n > 0) {
                turned_away += 1;
            }
        }
        assert_eq!(
            turned_away, 4,
            "the slot is held until the pair is answered"
        );
        let tricklers = headless;
        // A head with no newline at all is cut at the cap, not grown (R2).
        drop(tricklers);
        std::thread::sleep(Duration::from_millis(50));
        let mut endless = from_other_address(desk.port);
        endless
            .write_all("a".repeat(thock_bridge::MAX_HEAD_BYTES * 2).as_bytes())
            .unwrap();
        endless.flush().unwrap();
        endless.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut reply = String::new();
        endless.read_to_string(&mut reply).ok();
        assert!(reply.starts_with("HTTP/1.1 431"), "{reply}");
    }

    /// A client's `_meta.date` is honoured only inside the outbox's own
    /// window (M1): older labels fall back to today for the inbox, and
    /// `note_today` refuses rather than write into a distant day.
    #[gpui::test]
    async fn dates_outside_the_window_are_not_written(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let today = Local::now().date_naive();
        let old = (today - chrono::Duration::days(30)).to_string();
        let refused = call_tool(
            cx,
            &desk,
            &token,
            "note_today",
            json!({"line": "Back-dated."}),
            Some(json!({"call_id": "d-1", "date": old})),
        );
        assert!(refused.is_error);
        assert!(
            refused.first_text().contains("outside the 7 days"),
            "{}",
            refused.first_text()
        );
        let future = (today + chrono::Duration::days(1)).to_string();
        let refused = call_tool(
            cx,
            &desk,
            &token,
            "note_today",
            json!({"line": "Tomorrow's."}),
            Some(json!({"call_id": "d-2", "date": future})),
        );
        assert!(refused.is_error);
        let remembered = call_tool(
            cx,
            &desk,
            &token,
            "remember",
            json!({"fact": "An old fact"}),
            Some(json!({"call_id": "d-3", "date": old})),
        );
        assert!(!remembered.is_error);
        let inbox = desk
            .fs
            .load(Path::new("/vault/memory/inbox.md"))
            .await
            .unwrap();
        assert!(
            inbox.contains(&format!("- {today} · An old fact · via claude-code@robin")),
            "{inbox}"
        );
        let recent = (today - chrono::Duration::days(3)).to_string();
        let kept = call_tool(
            cx,
            &desk,
            &token,
            "note_today",
            json!({"line": "Three days ago."}),
            Some(json!({"call_id": "d-4", "date": recent})),
        );
        assert!(!kept.is_error, "{}", kept.first_text());
        assert!(
            desk.fs
                .is_file(Path::new(&format!("/vault/daily/{recent}.md")))
                .await
        );
    }

    /// The index never follows a link: not out of the vault, not back into
    /// it (M2).
    #[gpui::test]
    async fn the_index_skips_symlinks(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        desk.fs
            .insert_tree(
                "/elsewhere",
                json!({"secret.md": "# Elsewhere\n\nxyzzy outside\n"}),
            )
            .await;
        desk.fs
            .insert_symlink("/vault/outside", PathBuf::from("/elsewhere"))
            .await;
        desk.fs
            .insert_symlink("/vault/loop", PathBuf::from("/vault"))
            .await;
        desk.fs
            .insert_symlink("/vault/linked.md", PathBuf::from("/elsewhere/secret.md"))
            .await;
        cx.run_until_parked();
        let hits = call_tool(cx, &desk, &token, "search", json!({"query": "xyzzy"}), None);
        assert_eq!(hits.first_text(), "No notes match `xyzzy`.");
        let hits = call_tool(cx, &desk, &token, "search", json!({"query": "ana"}), None);
        assert!(
            !hits.first_text().contains("loop/"),
            "{}",
            hits.first_text()
        );
        let read = call_tool(
            cx,
            &desk,
            &token,
            "read",
            json!({"path": "outside/secret.md"}),
            None,
        );
        assert!(read.is_error, "{}", read.first_text());
        assert!(read.first_text().contains("outside the vault"));
    }

    /// A mirrored file lands only where its path really resolves (M4), and
    /// only when nothing in it is something the vault refuses to keep (M5a).
    #[gpui::test]
    async fn mirrored_files_are_checked_before_they_land(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let auth = format!("Bearer {token}");
        desk.fs.create_dir(Path::new("/elsewhere")).await.unwrap();
        desk.fs
            .create_dir(Path::new("/vault/memory/agents"))
            .await
            .unwrap();
        desk.fs
            .insert_symlink("/vault/memory/agents/codex", PathBuf::from("/elsewhere"))
            .await;
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/codex/MEMORY.md",
            &[("Authorization", auth.as_str())],
            "# fine\n",
        );
        assert_eq!(status, 400, "{body}");
        assert!(
            !desk
                .fs
                .is_file(Path::new("/elsewhere/robin/MEMORY.md"))
                .await
        );

        // Hashes and ids are what a memory file is made of; they mirror.
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/MEMORY.md",
            &[("Authorization", auth.as_str())],
            "# Memory\n- fixed in 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822c\n",
        );
        assert_eq!(status, 204, "{body}");
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/MEMORY.md",
            &[("Authorization", auth.as_str())],
            "# Memory\n- token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef12\n",
        );
        assert_eq!(status, 422, "{body}");
        assert!(body.contains("line 2 looks like an API token"), "{body}");
        let (status, body) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/topics/pay.md",
            &[("Authorization", auth.as_str())],
            "- Their salary went up\n",
        );
        assert_eq!(status, 422, "{body}");
        assert!(body.contains("do-not-keep"), "{body}");
        assert!(
            desk.fs
                .load(Path::new(
                    "/vault/memory/agents/claude-code/robin/MEMORY.md"
                ))
                .await
                .unwrap()
                .contains("9f86d081"),
            "the refused version never replaced the mirrored one"
        );
    }

    /// A write of the client list queued before a disconnect still runs
    /// after it, and renders the list as it is then: the revoked machine
    /// never comes back on disk.
    #[gpui::test]
    async fn a_disconnect_during_a_pending_write_never_resurrects_the_client(
        cx: &mut TestAppContext,
    ) {
        let desk = serve(cx, true).await;
        pair(cx, &desk, "robin").await;
        pair(cx, &desk, "hp").await;
        cx.run_until_parked();
        let clients_path = Path::new("/vault/.thock/state/bridge/clients.toml");
        assert!(
            desk.fs
                .load(clients_path)
                .await
                .unwrap()
                .contains("name = \"robin\"")
        );
        // A touch-style write is queued, then the disconnect lands before
        // either has run.
        desk.service.update(cx, |service, cx| {
            service.persist_clients_later(cx);
            service.disconnect("robin", cx);
        });
        cx.run_until_parked();
        let on_disk = desk.fs.load(clients_path).await.unwrap();
        assert!(!on_disk.contains("robin"), "{on_disk}");
        assert!(on_disk.contains("name = \"hp\""), "{on_disk}");
    }

    /// Editing `bridge.toml` while serving rebinds on a new port and takes
    /// a new daily heading in place.
    #[gpui::test]
    async fn editing_the_config_while_serving_rebinds(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let first_port = desk.port;
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let new_port = free.local_addr().unwrap().port();
        drop(free);
        desk.fs
            .atomic_write(
                PathBuf::from("/vault/.thock/routines/agents/bridge.toml"),
                format!("port = {new_port}\nbind = \"127.0.0.1\"\ntoday_heading = \"Agentes\"\n"),
            )
            .await
            .unwrap();
        let mut rebound = false;
        for _ in 0..200 {
            cx.run_until_parked();
            if desk.service.read_with(cx, |service, _| {
                matches!(service.state(), BridgeState::Serving { port, .. } if *port == new_port)
            }) {
                rebound = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            rebound,
            "{:?}",
            desk.service
                .read_with(cx, |service, _| service.state().clone())
        );
        assert_ne!(first_port, new_port);
        assert_eq!(
            desk.service
                .read_with(cx, |service, _| service.config.today_heading.clone()),
            "Agentes"
        );
        // A heading-only edit changes nothing about the port.
        desk.fs
            .atomic_write(
                PathBuf::from("/vault/.thock/routines/agents/bridge.toml"),
                format!(
                    "port = {new_port}\nbind = \"127.0.0.1\"\ntoday_heading = \"Agents said\"\n"
                ),
            )
            .await
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            desk.service
                .read_with(cx, |service, _| service.config.today_heading.clone()),
            "Agents said"
        );
        assert!(desk.service.read_with(cx, |service, _| {
            matches!(service.state(), BridgeState::Serving { port, .. } if *port == new_port)
        }));
    }

    /// A mirrored file takes a restore point before it changes, and the
    /// same bytes again change nothing: no checkpoint, no file event.
    #[gpui::test]
    async fn mirrored_files_checkpoint_once_and_skip_unchanged_bytes(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let auth = format!("Bearer {token}");
        let project = desk
            .service
            .read_with(cx, |service, _| service.project.clone());
        let events: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update({
            let events = events.clone();
            move |cx| {
                cx.subscribe(&project, move |_, event, _| {
                    if let project::Event::WorktreeUpdatedEntries(_, changes) = event {
                        for (path, _, _) in changes.iter() {
                            events.borrow_mut().push(path.as_unix_str().to_string());
                        }
                    }
                })
            }
        });
        let counter = desk
            .service
            .read_with(cx, |service, _| service.checkpoints.clone());
        let checkpoints = || counter.get();
        assert_eq!(checkpoints(), 0);
        let (status, _) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/MEMORY.md",
            &[("Authorization", auth.as_str())],
            "# Memory\n- one\n",
        );
        assert_eq!(status, 204);
        cx.run_until_parked();
        assert_eq!(
            checkpoints(),
            1,
            "a changed file took a restore point first"
        );
        let mirrored = "memory/agents/claude-code/robin/MEMORY.md";
        let writes = |events: &Rc<RefCell<Vec<String>>>| {
            events
                .borrow()
                .iter()
                .filter(|path| *path == mirrored)
                .count()
        };
        assert!(writes(&events) >= 1);
        let before = writes(&events);

        let (status, _) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/MEMORY.md",
            &[("Authorization", auth.as_str())],
            "# Memory\n- one\n",
        );
        assert_eq!(status, 204);
        cx.run_until_parked();
        assert_eq!(checkpoints(), 1, "the same bytes took no restore point");
        assert_eq!(writes(&events), before, "and fired no file event");

        let (status, _) = roundtrip(
            cx,
            desk.port,
            "PUT",
            "/v1/agents/claude-code/MEMORY.md",
            &[("Authorization", auth.as_str())],
            "# Memory\n- two\n",
        );
        assert_eq!(status, 204);
        cx.run_until_parked();
        assert_eq!(checkpoints(), 2);
        assert!(writes(&events) > before);
    }

    /// The desk's write goes through the `Fs`, so the worktree sees it
    /// like any edit (§14).
    #[gpui::test]
    async fn remember_fires_a_file_event(cx: &mut TestAppContext) {
        let desk = serve(cx, true).await;
        let token = pair(cx, &desk, "robin").await;
        let project = desk
            .service
            .read_with(cx, |service, _| service.project.clone());
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update({
            let seen = seen.clone();
            move |cx| {
                cx.subscribe(&project, move |_, event, _| {
                    if let project::Event::WorktreeUpdatedEntries(_, changes) = event {
                        for (path, _, _) in changes.iter() {
                            seen.borrow_mut().push(path.as_unix_str().to_string());
                        }
                    }
                })
            }
        });
        let result = call_tool(
            cx,
            &desk,
            &token,
            "remember",
            json!({"fact": "Likes short PR titles"}),
            Some(json!({"call_id": "e-1"})),
        );
        assert!(!result.is_error, "{}", result.first_text());
        cx.run_until_parked();
        assert!(
            seen.borrow().iter().any(|path| path == "memory/inbox.md"),
            "events: {:?}",
            seen.borrow()
        );
    }

    /// The lines the skill tells an agent to write by hand, when the tools
    /// are absent, parse with the same functions the desk writes them with
    /// (V55 §14).
    #[test]
    fn the_skill_prescribes_the_lines_the_desk_writes() {
        let skill = include_str!("../assets/routines/agents/skills/thock-memory/SKILL.md");
        let snippet = |prefix: &str| -> String {
            skill
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with(prefix))
                .unwrap_or_else(|| panic!("the skill no longer shows {prefix:?}"))
                .trim_end_matches('.')
                .trim_matches('`')
                .replace("YYYY-MM-DD", "2026-11-04")
                .replace("<harness>@<machine>", "claude-code@robin")
                .replace("the fact", "Prefers short PR titles")
                .replace("what you did", "Shipped the PR.")
        };
        let inbox = thock_bridge::parse_inbox_line(&snippet("`- YYYY-MM-DD ·")).unwrap();
        assert_eq!(inbox.fact, "Prefers short PR titles");
        assert_eq!(
            inbox.via,
            Some(thock_bridge::ClientIdentity::new("claude-code", "robin"))
        );
        assert_eq!(
            thock_bridge::inbox_line(inbox.date, &inbox.fact, &inbox.via.unwrap()),
            snippet("`- YYYY-MM-DD ·")
        );
        let (identity, text) =
            thock_bridge::parse_today_line(&snippet("`- <harness>@<machine> ·")).unwrap();
        assert_eq!(identity.label(), "claude-code@robin");
        assert_eq!(text, "Shipped the PR.");
        assert!(skill.contains("## From your agents"));
        assert!(skill.starts_with("---\nname: thock-memory\n"));
        let description = skill
            .lines()
            .find_map(|line| line.strip_prefix("description: "))
            .unwrap();
        assert!(description.len() <= 60, "{description}");

        // The core files learned about the bridge (V55 §10).
        let agents_md = include_str!("../assets/AGENTS.md");
        assert!(agents_md.contains(
            "Pages under
   `memory/agents/` belong to the agent named in their path"
        ));
        let reflect = include_str!("../assets/skills/reflect.md");
        assert!(reflect.contains("← agents/<harness>/<host>/<file>"));
        assert!(reflect.contains("`· via <harness>@<host>`"));
        let system = include_str!("../assets/hosted-agent/SYSTEM.md");
        assert!(system.contains("## From your agents"));
        let wrap_today = include_str!("../assets/routines/timeline/skills/wrap-today.md");
        assert!(wrap_today.contains("`## From your agents`"));
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
