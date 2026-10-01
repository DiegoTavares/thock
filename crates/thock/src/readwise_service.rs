//! The Readwise sync service (spec `v31-readwise-sync.md` §8): one GPUI
//! entity per local project that polls Readwise's export API, lands one note
//! per source in its category's folder, appends new highlights to existing
//! notes, and applies everything in crash-safe order — notes first, then the
//! landed state, then the watermark. The transport, the keychain, and the
//! token prompt live here too; the pure half is `readwise.rs`.

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use editor::Editor;
use fs::Fs;
use futures::AsyncReadExt as _;
use gpui::{
    App, AppContext as _, AsyncApp, BackgroundExecutor, Context, DismissEvent, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, Global, SharedString, Subscription, Task, WeakEntity,
    actions,
};
use http_client::{AsyncBody, HttpClient, Request, http};
use project::Project;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ui::prelude::*;
use ui::{Headline, HeadlineSize, Icon, IconSize, Label};
use workspace::{ModalView, Workspace};

use crate::backlog::apply_edits;
use crate::calendar_service::{ManualSyncFinished, SyncState, show_sync_toast};
use crate::readwise::{
    DEFAULT_CONFIG_TOML, ExportPage, LandedState, NoteChange, NoteWrite, READWISE_CONFIG_FILE,
    ReadwiseConfig, ReadwiseSource, ReadwiseVaultScan, append_highlights_edit,
    parse_readwise_config, plan_readwise_sync, scan_markers,
};
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

/// The keychain slot (spec §8.4): one token per machine, shared by every vault.
pub const KEYCHAIN_URL: &str = "https://readwise.io";
const KEYCHAIN_USERNAME: &str = "readwise";
/// Where the user copies the token from; opened by `ConnectReadwise`.
pub const TOKEN_PAGE_URL: &str = "https://readwise.io/access_token";
const EXPORT_URL: &str = "https://readwise.io/api/v2/export/";
const AUTH_URL: &str = "https://readwise.io/api/v2/auth/";

const STATE_DIR: &str = "state/readwise";
const LANDED_FILE: &str = "landed.jsonl";
const CURSOR_FILE: &str = "cursor.json";

/// Same typing guard as the Gmail and calendar services (V8 §9 guard 1).
const TYPING_GUARD_QUIET: Duration = Duration::from_secs(2);
const TYPING_GUARD_MAX_TRIES: usize = 15;
/// Transport errors back off from `poll_minutes` up to here (spec §8.1).
const BACKOFF_CEILING: Duration = Duration::from_secs(6 * 60 * 60);
/// A `429` without `Retry-After` waits this long; the export endpoint allows
/// twenty requests a minute.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_WAITS: usize = 10;
/// The clock-skew margin the watermark is backed off by (spec §4.4).
const WATERMARK_SKEW_MINUTES: i64 = 5;

actions!(
    thock,
    [
        /// Connects Readwise: paste an access token once, and your book
        /// highlights land as notes from then on.
        ConnectReadwise,
        /// Forgets the Readwise token. The highlight notes already in the
        /// vault stay exactly as they are.
        DisconnectReadwise,
        /// Checks Readwise for new highlights now.
        SyncReadwiseNow,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let service = cx.new(|cx| ReadwiseService::new(project.clone(), cx));
        cx.subscribe(&service, |workspace, _, event: &ManualSyncFinished, cx| {
            show_sync_toast(workspace, event, cx);
        })
        .detach();
        let project_id = project.entity_id();
        cx.default_global::<GlobalReadwiseServices>()
            .0
            .insert(project_id, service);
        cx.on_release(move |_, cx| {
            cx.default_global::<GlobalReadwiseServices>()
                .0
                .remove(&project_id);
        })
        .detach();

        workspace.register_action(|workspace, _: &ConnectReadwise, window, cx| {
            let Some(service) = service_for_project(workspace.project(), cx) else {
                return;
            };
            if !service.read(cx).has_vault() {
                workspace.show_error(
                    "This workspace isn't a Thock vault, so there is nowhere for highlights to land."
                        .to_string(),
                    cx,
                );
                return;
            }
            cx.open_url(TOKEN_PAGE_URL);
            let service = service.downgrade();
            workspace.toggle_modal(window, cx, |window, cx| {
                ReadwiseTokenPrompt::new(service, window, cx)
            });
        });
        workspace.register_action(|workspace, _: &DisconnectReadwise, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.disconnect(cx));
            }
        });
        workspace.register_action(|workspace, _: &SyncReadwiseNow, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.sync_now(cx));
            }
        });
    })
    .detach();
}

#[derive(Default)]
struct GlobalReadwiseServices(HashMap<EntityId, Entity<ReadwiseService>>);

impl Global for GlobalReadwiseServices {}

/// A service for `project`, registered the way `init` would, without a
/// workspace.
#[cfg(test)]
pub(crate) fn new_for_test(project: &Entity<Project>, cx: &mut App) -> Entity<ReadwiseService> {
    let service = cx.new(|cx| ReadwiseService::new(project.clone(), cx));
    cx.default_global::<GlobalReadwiseServices>()
        .0
        .insert(project.entity_id(), service.clone());
    service
}

/// The sync service for `project`, if one is running.
pub fn service_for_project(project: &Entity<Project>, cx: &App) -> Option<Entity<ReadwiseService>> {
    cx.try_global::<GlobalReadwiseServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

/// Readwise answered `401`: the token is gone or revoked. Distinct from every
/// other failure so the loop stops instead of backing off (spec §8.1).
#[derive(Debug)]
pub struct TokenRejected;

impl std::fmt::Display for TokenRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Readwise didn't accept that token")
    }
}

impl std::error::Error for TokenRejected {}

/// Transport abstraction: the export API is the implementation; tests stub
/// it, and a Reader source could join behind it later (spec §3).
pub trait ReadwiseTransport: Send + Sync {
    /// Every source updated after `updated_after` (the whole library when
    /// `None`), all pages fetched. A failed page fails the whole fetch, so the
    /// watermark never skips data.
    fn fetch(
        &self,
        updated_after: Option<DateTime<Utc>>,
        cx: &AsyncApp,
    ) -> Task<Result<Vec<ReadwiseSource>>>;
}

/// `GET /api/v2/export/` with the token header, paged on `nextPageCursor`
/// (spec §8.1). The token never appears in an error: failures carry the
/// status alone.
pub struct HttpReadwiseTransport {
    http: Arc<dyn HttpClient>,
    token: Arc<str>,
}

impl HttpReadwiseTransport {
    pub fn new(http: Arc<dyn HttpClient>, token: String) -> Self {
        Self {
            http,
            token: token.into(),
        }
    }
}

impl ReadwiseTransport for HttpReadwiseTransport {
    fn fetch(
        &self,
        updated_after: Option<DateTime<Utc>>,
        cx: &AsyncApp,
    ) -> Task<Result<Vec<ReadwiseSource>>> {
        let http = self.http.clone();
        let token = self.token.clone();
        let executor = cx.background_executor().clone();
        cx.background_spawn(
            async move { fetch_export(&http, &token, updated_after, &executor).await },
        )
    }
}

fn authorized_get(url: &str, token: &str) -> Result<Request<AsyncBody>> {
    Ok(Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header("Authorization", format!("Token {token}"))
        .header("Accept", "application/json")
        .body(AsyncBody::default())?)
}

async fn fetch_export(
    http: &Arc<dyn HttpClient>,
    token: &str,
    updated_after: Option<DateTime<Utc>>,
    executor: &BackgroundExecutor,
) -> Result<Vec<ReadwiseSource>> {
    let mut sources = Vec::new();
    let mut cursor: Option<String> = None;
    let mut rate_limit_waits = 0;
    loop {
        let mut url = url::Url::parse(EXPORT_URL).context("building the export URL")?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(after) = &updated_after {
                query.append_pair(
                    "updatedAfter",
                    &after.to_rfc3339_opts(SecondsFormat::Secs, true),
                );
            }
            if let Some(cursor) = &cursor {
                query.append_pair("pageCursor", cursor);
            }
        }
        let mut response = http
            .send(authorized_get(url.as_str(), token)?)
            .await
            .context("reaching Readwise")?;
        let status = response.status();
        if status == http::StatusCode::UNAUTHORIZED {
            return Err(anyhow!(TokenRejected));
        }
        if status == http::StatusCode::TOO_MANY_REQUESTS {
            rate_limit_waits += 1;
            if rate_limit_waits > MAX_RATE_LIMIT_WAITS {
                bail!("Readwise kept rate-limiting the export");
            }
            let wait = retry_after(response.headers()).unwrap_or(DEFAULT_RETRY_AFTER);
            executor.timer(wait).await;
            continue;
        }
        if !status.is_success() {
            bail!("Readwise export failed with status {status}");
        }
        let mut body = String::new();
        response
            .body_mut()
            .read_to_string(&mut body)
            .await
            .context("reading the Readwise export")?;
        let page: ExportPage =
            serde_json::from_str(&body).context("parsing the Readwise export")?;
        sources.extend(page.results);
        match page.next_page_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(sources)
}

fn retry_after(headers: &http::HeaderMap) -> Option<Duration> {
    headers
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// `GET /api/v2/auth/` → `204` (spec §5 step 2). A `401` or `403` is
/// [`TokenRejected`]; anything else is a reachability problem.
pub async fn validate_token(http: &Arc<dyn HttpClient>, token: &str) -> Result<()> {
    let response = http
        .send(authorized_get(AUTH_URL, token)?)
        .await
        .context("reaching Readwise")?;
    let status = response.status();
    if status == http::StatusCode::UNAUTHORIZED || status == http::StatusCode::FORBIDDEN {
        return Err(anyhow!(TokenRejected));
    }
    if !status.is_success() {
        bail!("Readwise answered with status {status}");
    }
    Ok(())
}

async fn read_token(cx: &AsyncApp) -> Result<Option<String>> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    let Some((_, token)) = provider.read_credentials(KEYCHAIN_URL, cx).await? else {
        return Ok(None);
    };
    let token = String::from_utf8(token).context("the stored Readwise token is not UTF-8")?;
    Ok(Some(token))
}

async fn write_token(token: &str, cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    provider
        .write_credentials(KEYCHAIN_URL, KEYCHAIN_USERNAME, token.as_bytes(), cx)
        .await
}

async fn delete_token(cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    provider.delete_credentials(KEYCHAIN_URL, cx).await
}

enum SyncOutcome {
    Synced {
        /// Highlights landed by this poll, for the status row and the
        /// manual-sync toast.
        landed: usize,
    },
    Failed(anyhow::Error),
    TokenRejected,
    /// The service was reconfigured or released mid-sync.
    Aborted,
}

pub struct ReadwiseService {
    project: Entity<Project>,
    vault: Option<Vault>,
    config: Option<ReadwiseConfig>,
    /// A `readwise.toml` that didn't parse or validate (spec §4.1): the
    /// status row shows it instead of a sync state, with no retry button.
    config_error: Option<SharedString>,
    transport: Option<Arc<dyn ReadwiseTransport>>,
    state: SyncState,
    /// The one poll loop. Replacing it on reload cancels the old loop; the
    /// apply work it spawns is awaited inside it, never stored separately.
    poll_task: Option<Task<()>>,
    /// The keychain lookup a reload starts; the newest config wins.
    token_task: Option<Task<()>>,
    /// The `landed.jsonl` record, loaded once per transport start and kept
    /// current by every apply. The vault side is re-scanned per poll.
    landed: Option<LandedState>,
    /// Whether a watermark exists: without one the running sync is the first
    /// import of the whole library (spec §8.5).
    has_watermark: bool,
    /// Highlights the last completed sync landed, shown for one poll.
    last_landed: usize,
    /// Set by `sync_now` so the next completed sync announces itself
    /// ([`ManualSyncFinished`]); background polls stay quiet.
    announce_next_sync: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ManualSyncFinished> for ReadwiseService {}

impl ReadwiseService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        let mut this = Self {
            project,
            vault: None,
            config: None,
            config_error: None,
            transport: None,
            state: SyncState::NoConfig,
            poll_task: None,
            token_task: None,
            landed: None,
            has_watermark: false,
            last_landed: 0,
            announce_next_sync: false,
            _subscriptions: vec![project_subscription],
        };
        this.reload(cx);
        this
    }

    pub fn state(&self) -> &SyncState {
        &self.state
    }

    pub fn has_vault(&self) -> bool {
        self.vault.is_some()
    }

    /// Whether the status row should be shown at all: only when the vault
    /// carries `.thock/readwise.toml` (spec §8.5).
    pub fn has_config(&self) -> bool {
        !matches!(self.state, SyncState::NoConfig)
    }

    /// The config problem to show in place of a sync state, if any.
    pub fn config_error(&self) -> Option<&SharedString> {
        self.config_error.as_ref()
    }

    /// True while the first import of the library is running.
    pub fn importing_library(&self) -> bool {
        matches!(self.state, SyncState::Idle) && !self.has_watermark
    }

    /// Highlights the last completed sync landed (spec §8.5).
    pub fn last_landed(&self) -> usize {
        self.last_landed
    }

    /// `thock::SyncReadwiseNow`: restarts the loop, which checks immediately.
    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.transport.is_some() {
            self.announce_next_sync = true;
            self.start_poll(cx);
        }
    }

    /// `thock::DisconnectReadwise`: stops polling and forgets the token. The
    /// config file and every landed note stay (spec §7).
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.poll_task = None;
        self.token_task = None;
        self.transport = None;
        self.landed = None;
        if self.config.is_some() {
            self.state = SyncState::NeverConnected;
        }
        cx.spawn(async move |_, cx| delete_token(cx).await)
            .detach_and_log_err(cx);
        cx.notify();
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
                let readwise_config = format!("{VAULT_MARKER_DIR}/{READWISE_CONFIG_FILE}");
                let vault_config = format!("{VAULT_MARKER_DIR}/{VAULT_CONFIG_FILE}");
                if changes.iter().any(|(path, _, _)| {
                    let path = path.as_unix_str();
                    path == readwise_config || path == vault_config
                }) {
                    self.reload(cx);
                }
            }
            _ => {}
        }
    }

    /// Re-resolves the vault and `.thock/readwise.toml`, looking the token
    /// up and restarting the poll loop when the configuration changed.
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
            self.clear_sync(SyncState::NoConfig);
            cx.notify();
            return;
        };

        let config_path = vault.root.join(VAULT_MARKER_DIR).join(READWISE_CONFIG_FILE);
        self.vault = Some(vault);
        // Same synchronous read as `Vault::detect`; the file is tiny.
        let config = match std::fs::read_to_string(&config_path) {
            Err(_) => None,
            Ok(text) => match parse_readwise_config(&text) {
                Ok(config) => Some(config),
                Err(error) => {
                    log::warn!(
                        "Thock: couldn't use {}: {error:#}; Readwise sync is off",
                        config_path.display()
                    );
                    self.clear_sync(SyncState::Failing {
                        error: format!("{error:#}").into(),
                    });
                    self.config_error = Some(format!("{error:#}").into());
                    cx.notify();
                    return;
                }
            },
        };
        self.config_error = None;

        match config {
            None => self.clear_sync(SyncState::NoConfig),
            Some(config) => {
                let unchanged = self.config.as_ref() == Some(&config)
                    && (self.transport.is_some()
                        || matches!(self.state, SyncState::NeverConnected));
                if !unchanged {
                    self.config = Some(config);
                    self.transport = None;
                    self.poll_task = None;
                    self.landed = None;
                    self.lookup_token(cx);
                }
            }
        }
        cx.notify();
    }

    fn clear_sync(&mut self, state: SyncState) {
        self.config = None;
        self.config_error = None;
        self.transport = None;
        self.poll_task = None;
        self.token_task = None;
        self.landed = None;
        self.state = state;
    }

    /// Reads the keychain; a stored token starts the loop, none leaves the
    /// row offering to connect.
    fn lookup_token(&mut self, cx: &mut Context<Self>) {
        self.state = SyncState::Connecting;
        self.token_task = Some(cx.spawn(async move |this, cx| {
            let token = read_token(cx).await;
            this.update(cx, |service, cx| {
                match token {
                    Ok(Some(token)) => service.start_with_token(token, cx),
                    Ok(None) => service.state = SyncState::NeverConnected,
                    Err(error) => {
                        log::warn!("Thock: couldn't read the Readwise token: {error:#}");
                        service.state = SyncState::Failing {
                            error: "the keychain couldn't be read".into(),
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn start_with_token(&mut self, token: String, cx: &mut Context<Self>) {
        self.transport = Some(Arc::new(HttpReadwiseTransport::new(
            cx.http_client(),
            token,
        )));
        self.landed = None;
        self.state = SyncState::Idle;
        self.start_poll(cx);
        cx.notify();
    }

    /// The token prompt's `enter` (spec §8.4): validates, stores in the
    /// keychain, writes the default config when there is none, and starts
    /// syncing. The error comes back to the prompt, which stays open.
    fn adopt_token(&mut self, token: String, cx: &mut Context<Self>) -> Task<Result<()>> {
        let Some(vault) = &self.vault else {
            return Task::ready(Err(anyhow!("this workspace isn't a Thock vault")));
        };
        let config_path = vault.root.join(VAULT_MARKER_DIR).join(READWISE_CONFIG_FILE);
        let http = cx.http_client();
        let fs = self.project.read(cx).fs().clone();
        cx.spawn(async move |this, cx| {
            validate_token(&http, &token).await?;
            write_token(&token, cx).await?;
            if !fs.is_file(&config_path).await {
                if let Some(parent) = config_path.parent() {
                    fs.create_dir(parent).await?;
                }
                fs.atomic_write(config_path, DEFAULT_CONFIG_TOML.to_string())
                    .await?;
            }
            this.update(cx, |service, cx| {
                service.reload(cx);
                if service.config.is_some() {
                    service.token_task = None;
                    service.start_with_token(token, cx);
                }
            })?;
            Ok(())
        })
    }

    /// (Re)starts the poll loop: an immediate check, then one tick per
    /// `poll_minutes`, doubling up to six hours on transport errors.
    fn start_poll(&mut self, cx: &mut Context<Self>) {
        let Some(interval) = self.config.as_ref().map(|config| config.poll_interval) else {
            return;
        };
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            let mut delay = interval;
            loop {
                let outcome = Self::sync_once(&this, cx).await;
                let keep_going = this
                    .update(cx, |service, cx| {
                        service.finish_sync(outcome, interval, &mut delay, cx)
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
                cx.background_executor().timer(delay).await;
            }
        }));
    }

    fn finish_sync(
        &mut self,
        outcome: SyncOutcome,
        interval: Duration,
        delay: &mut Duration,
        cx: &mut Context<Self>,
    ) -> bool {
        let announcement = std::mem::take(&mut self.announce_next_sync)
            .then(|| match &outcome {
                SyncOutcome::Aborted => None,
                SyncOutcome::Synced { landed } => Some(match landed {
                    0 => "Readwise synced — nothing new".into(),
                    1 => "Readwise synced — 1 new highlight".into(),
                    n => format!("Readwise synced — {n} new highlights").into(),
                }),
                SyncOutcome::Failed(error) => {
                    Some(format!("Readwise sync failed — {error:#}").into())
                }
                SyncOutcome::TokenRejected => {
                    Some("Readwise rejected the token — reconnect to sync highlights".into())
                }
            })
            .flatten();
        let keep_going = match outcome {
            SyncOutcome::Aborted => false,
            SyncOutcome::Synced { landed } => {
                self.state = SyncState::Synced { at: Instant::now() };
                self.last_landed = landed;
                *delay = interval;
                true
            }
            SyncOutcome::Failed(error) => {
                log::warn!("Thock Readwise sync failed: {error:#}");
                self.state = SyncState::Failing {
                    error: format!("{error:#}").into(),
                };
                // Offline is just an error: back off, keep trying.
                *delay = (*delay * 2).min(BACKOFF_CEILING);
                true
            }
            SyncOutcome::TokenRejected => {
                self.state = SyncState::Disconnected;
                self.transport = None;
                false
            }
        };
        if let Some(message) = announcement {
            cx.emit(ManualSyncFinished {
                message,
                icon: IconName::Book,
            });
        }
        cx.notify();
        keep_going
    }

    async fn sync_once(this: &WeakEntity<Self>, cx: &mut AsyncApp) -> SyncOutcome {
        let context = this
            .read_with(cx, |service, _| {
                match (&service.transport, &service.config, &service.vault) {
                    (Some(transport), Some(config), Some(vault)) => Some((
                        transport.clone(),
                        config.clone(),
                        vault.clone(),
                        service.project.clone(),
                    )),
                    _ => None,
                }
            })
            .ok()
            .flatten();
        let Some((transport, config, vault, project)) = context else {
            return SyncOutcome::Aborted;
        };
        let fs = project.read_with(cx, |project, _| project.fs().clone());

        let landed = match Self::landed_state(this, &fs, &vault, cx).await {
            Ok(landed) => landed,
            Err(error) => return SyncOutcome::Failed(error),
        };
        let watermark = load_watermark(&fs, &vault).await;
        this.update(cx, |service, cx| {
            service.has_watermark = watermark.is_some();
            cx.notify();
        })
        .ok();
        // The next watermark is this pass's start, so anything updated while
        // the fetch runs is seen again next time (spec §4.4).
        let started_at = Utc::now();
        let scan = scan_vault(&fs, &vault, &config).await;

        let sources = match transport.fetch(watermark, cx).await {
            Err(error) if error.is::<TokenRejected>() => return SyncOutcome::TokenRejected,
            Err(error) => return SyncOutcome::Failed(error),
            Ok(sources) => sources,
        };

        let changes = plan_readwise_sync(&sources, &config, &scan, &landed, &Local);
        let landed_count = changes
            .iter()
            .map(|change| change.highlight_ids.len())
            .sum();
        let repairs = repair_rows(&scan, &landed);
        if let Err(error) =
            Self::apply_changes(this, &fs, &vault, &project, &changes, repairs, cx).await
        {
            return SyncOutcome::Failed(error);
        }
        let watermark = started_at - chrono::Duration::minutes(WATERMARK_SKEW_MINUTES);
        if let Err(error) = write_watermark(&fs, &vault, watermark).await {
            return SyncOutcome::Failed(error);
        }
        this.update(cx, |service, _| service.has_watermark = true)
            .ok();
        SyncOutcome::Synced {
            landed: landed_count,
        }
    }

    /// The `landed.jsonl` record, loaded once per transport start (spec
    /// §4.3). A missing or unreadable file is an empty record: the vault scan
    /// is the fallback, and the repair pass rebuilds the file.
    async fn landed_state(
        this: &WeakEntity<Self>,
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        cx: &mut AsyncApp,
    ) -> Result<LandedState> {
        if let Some(landed) = this.read_with(cx, |service, _| service.landed.clone())? {
            return Ok(landed);
        }
        let mut landed = LandedState::default();
        if let Ok(contents) = fs.load(&state_path(vault, LANDED_FILE)).await {
            for line in contents.lines() {
                let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if let Some(highlight) = record.get("highlight").and_then(|value| value.as_u64()) {
                    landed.highlights.insert(highlight);
                }
                if let Some(book) = record.get("book").and_then(|value| value.as_u64()) {
                    landed.books.insert(book);
                }
            }
        }
        this.update(cx, |service, _| service.landed = Some(landed.clone()))?;
        Ok(landed)
    }

    /// Applies the plan in crash-safe order (spec §8.3): notes first, then
    /// the landed rows. A crash between the two re-plans next poll into a
    /// state repair through the vault scan, never a duplicate line.
    async fn apply_changes(
        this: &WeakEntity<Self>,
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        project: &Entity<Project>,
        changes: &[NoteChange],
        repairs: Vec<LandedRow>,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        for change in changes {
            let path = vault.root.join(&change.rel_path);
            match &change.write {
                NoteWrite::Create { contents } => {
                    if fs.is_file(&path).await {
                        // The scan saw no note here moments ago; whatever
                        // appeared is not ours to overwrite. Next poll
                        // re-plans against it.
                        bail!("{} appeared while syncing", change.rel_path);
                    }
                    if let Some(parent) = path.parent() {
                        fs.create_dir(parent).await?;
                    }
                    fs.atomic_write(path, contents.clone()).await?;
                }
                NoteWrite::Append { lines } => {
                    Self::append_to_note(fs, project, &path, lines, cx).await?;
                }
            }
        }

        let mut rows = repairs;
        let at = Local::now().to_rfc3339();
        for change in changes {
            for highlight in &change.highlight_ids {
                rows.push(LandedRow {
                    highlight: *highlight,
                    book: change.book_id,
                    rel_path: change.rel_path.clone(),
                    at: at.clone(),
                });
            }
        }
        append_landed(fs, vault, &rows).await?;
        this.update(cx, |service, _| {
            if let Some(landed) = service.landed.as_mut() {
                for row in &rows {
                    landed.highlights.insert(row.highlight);
                    landed.books.insert(row.book);
                }
            }
        })
        .ok();
        Ok(())
    }

    /// Inserts `lines` at the end of the note's `## Highlights` section,
    /// marker-guarded so retries never duplicate: through the open buffer as
    /// one finalized transaction behind the typing guard when the note is
    /// open (undoable with one `u`, cannot clobber unsaved keystrokes),
    /// read-modify-write through the project `Fs` otherwise.
    async fn append_to_note(
        fs: &Arc<dyn Fs>,
        project: &Entity<Project>,
        path: &Path,
        lines: &[String],
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let buffer = project.update(cx, |project, cx| {
            project
                .project_path_for_absolute_path(path, cx)
                .and_then(|project_path| project.get_open_buffer(&project_path, cx))
        });

        let pending = |text: &str| -> Vec<String> {
            let present = scan_markers(text);
            lines
                .iter()
                .filter(|line| scan_markers(line).is_disjoint(&present))
                .cloned()
                .collect()
        };

        let Some(buffer) = buffer else {
            let text = fs
                .load(path)
                .await
                .with_context(|| format!("reading {}", path.display()))?;
            let pending = pending(&text);
            if pending.is_empty() {
                return Ok(());
            }
            let edit = append_highlights_edit(&text, &pending);
            fs.atomic_write(path.to_path_buf(), apply_edits(&text, vec![edit]))
                .await?;
            return Ok(());
        };

        for _ in 0..TYPING_GUARD_MAX_TRIES {
            let version = buffer.read_with(cx, |buffer, _| buffer.version());
            cx.background_executor().timer(TYPING_GUARD_QUIET).await;
            if buffer.read_with(cx, |buffer, _| buffer.version() == version) {
                break;
            }
        }

        // The buffer can change between computing the diff and applying it;
        // `apply_diff` refuses stale diffs, so just recompute — the marker
        // guard makes a re-run against fresh text converge.
        for _ in 0..3 {
            let text = buffer.read_with(cx, |buffer, _| buffer.text());
            let pending = pending(&text);
            if pending.is_empty() {
                return Ok(());
            }
            let edit = append_highlights_edit(&text, &pending);
            let new_text = apply_edits(&text, vec![edit]);
            let diff = buffer
                .read_with(cx, |buffer, cx| buffer.diff(new_text, cx))
                .await;
            let applied = buffer.update(cx, |buffer, cx| {
                buffer.start_transaction();
                let applied = buffer.apply_diff(diff, cx).is_some();
                buffer.end_transaction(cx);
                // Not grouped with the user's own edit history entry.
                buffer.finalize_last_transaction();
                applied
            });
            if applied {
                return Ok(());
            }
        }
        Err(anyhow!(
            "{} kept changing while landing highlights",
            path.display()
        ))
    }

    #[cfg(test)]
    pub(crate) fn set_state_for_test(&mut self, state: SyncState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    #[cfg(test)]
    fn configure_for_test(
        &mut self,
        vault: Vault,
        config: ReadwiseConfig,
        transport: Arc<dyn ReadwiseTransport>,
        cx: &mut Context<Self>,
    ) {
        self.vault = Some(vault);
        self.config = Some(config);
        self.config_error = None;
        self.transport = Some(transport);
        self.landed = None;
        self.state = SyncState::Idle;
        self.start_poll(cx);
    }
}

/// One `landed.jsonl` row (spec §4.3).
struct LandedRow {
    highlight: u64,
    book: u64,
    rel_path: String,
    at: String,
}

/// Highlights marked in a note but missing from the state — the record lost
/// them (a crash between the note write and the state append, or a deleted
/// state folder), so they are written back without touching the note.
fn repair_rows(scan: &ReadwiseVaultScan, landed: &LandedState) -> Vec<LandedRow> {
    let at = Local::now().to_rfc3339();
    let mut rows = Vec::new();
    for (book, note) in &scan.notes {
        let mut missing: Vec<u64> = note
            .markers
            .iter()
            .copied()
            .filter(|highlight| !landed.highlights.contains(highlight))
            .collect();
        missing.sort_unstable();
        for highlight in missing {
            rows.push(LandedRow {
                highlight,
                book: *book,
                rel_path: note.rel_path.clone(),
                at: at.clone(),
            });
        }
    }
    rows
}

fn state_path(vault: &Vault, file: &str) -> PathBuf {
    vault.root.join(VAULT_MARKER_DIR).join(STATE_DIR).join(file)
}

async fn append_landed(fs: &Arc<dyn Fs>, vault: &Vault, rows: &[LandedRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let path = state_path(vault, LANDED_FILE);
    let mut contents = fs.load(&path).await.unwrap_or_default();
    for row in rows {
        let entry = serde_json::json!({
            "highlight": row.highlight,
            "book": row.book,
            "path": row.rel_path,
            "at": row.at,
        });
        contents.push_str(&entry.to_string());
        contents.push('\n');
    }
    if let Some(parent) = path.parent() {
        fs.create_dir(parent).await?;
    }
    fs.atomic_write(path, contents).await
}

async fn load_watermark(fs: &Arc<dyn Fs>, vault: &Vault) -> Option<DateTime<Utc>> {
    let contents = fs.load(&state_path(vault, CURSOR_FILE)).await.ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let stamp = value.get("updated_after")?.as_str()?;
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|moment| moment.with_timezone(&Utc))
}

async fn write_watermark(fs: &Arc<dyn Fs>, vault: &Vault, watermark: DateTime<Utc>) -> Result<()> {
    let path = state_path(vault, CURSOR_FILE);
    if let Some(parent) = path.parent() {
        fs.create_dir(parent).await?;
    }
    let contents = serde_json::json!({
        "updated_after": watermark.to_rfc3339_opts(SecondsFormat::Secs, true),
    });
    fs.atomic_write(path, contents.to_string()).await
}

/// One pass over every mapped folder: `readwise_id` → note and its markers,
/// plus the stems in use (spec §4.3). A missing folder is an empty state.
async fn scan_vault(fs: &Arc<dyn Fs>, vault: &Vault, config: &ReadwiseConfig) -> ReadwiseVaultScan {
    use futures::StreamExt as _;
    let mut scan = ReadwiseVaultScan::default();
    for mapping in &config.mappings {
        let dir = vault.root.join(&mapping.path);
        let Ok(mut entries) = fs.read_dir(&dir).await else {
            continue;
        };
        while let Some(entry) = entries.next().await {
            let Ok(path) = entry else { continue };
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if stem.starts_with('.') {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let rel_path = format!("{}/{file_name}", mapping.path);
            let content = fs.load(&path).await.unwrap_or_default();
            scan.record(&mapping.path, stem, &rel_path, &content);
        }
    }
    scan
}

/// The token prompt (spec §8.4): one masked line, `enter` validates and
/// stores, `escape` cancels. A rejected token keeps the prompt open with the
/// reason under the field.
pub struct ReadwiseTokenPrompt {
    editor: Entity<Editor>,
    service: WeakEntity<ReadwiseService>,
    error: Option<SharedString>,
    validating: bool,
    validation: Option<Task<()>>,
}

impl EventEmitter<DismissEvent> for ReadwiseTokenPrompt {}
impl ModalView for ReadwiseTokenPrompt {}

impl Focusable for ReadwiseTokenPrompt {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl ReadwiseTokenPrompt {
    pub fn new(
        service: WeakEntity<ReadwiseService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_masked(true, cx);
            editor.set_placeholder_text("Paste your Readwise access token", window, cx);
            editor
        });
        Self {
            editor,
            service,
            error: None,
            validating: false,
            validation: None,
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        if self.validating {
            return;
        }
        let token = self.editor.read(cx).text(cx).trim().to_string();
        if token.is_empty() {
            self.error = Some("Paste the token first.".into());
            cx.notify();
            return;
        }
        let Some(service) = self.service.upgrade() else {
            cx.emit(DismissEvent);
            return;
        };
        self.validating = true;
        self.error = None;
        cx.notify();
        let adopted = service.update(cx, |service, cx| service.adopt_token(token, cx));
        self.validation = Some(cx.spawn(async move |this, cx| {
            let result = adopted.await;
            this.update(cx, |this, cx| {
                this.validating = false;
                match result {
                    Ok(()) => cx.emit(DismissEvent),
                    Err(error) => {
                        this.error = Some(describe_connect_error(&error).into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
}

fn describe_connect_error(error: &anyhow::Error) -> String {
    if error.is::<TokenRejected>() {
        "Readwise didn't accept that token.".to_string()
    } else {
        format!("Couldn't connect: {error:#}")
    }
}

impl Render for ReadwiseTokenPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ReadwiseTokenPrompt")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(36.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1p5()
                    .child(Icon::new(IconName::Book).size(IconSize::XSmall))
                    .child(Headline::new("Connect Readwise").size(HeadlineSize::XSmall)),
            )
            .child(
                div().px_3().pb_2().child(
                    Label::new(
                        "Copy the token from readwise.io/access_token (it just opened in your \
                         browser) and paste it here. Thock checks it, then keeps it in your \
                         system keychain — never in the vault.",
                    )
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                ),
            )
            .child(
                div()
                    .py_2()
                    .px_3()
                    .bg(cx.theme().colors().editor_background)
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(self.editor.clone()),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
            })
            .when(self.validating, |this| {
                this.child(
                    div().px_3().py_1().child(
                        Label::new("Checking the token…")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readwise::note_readwise_id;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};
    use settings::SettingsStore;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const BOOK_PATH: &str = "/vault/reference/readwise/books/A Fé Na Era Do Ceticismo.md";

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

    fn book_json(highlights: &str) -> String {
        format!(
            r#"{{"user_book_id": 28374651, "title": "A Fé Na Era Do Ceticismo",
                "author": "Timothy Keller", "category": "books", "asin": "B06XTSG7LR",
                "cover_image_url": null, "book_tags": [], "highlights": [{highlights}]}}"#
        )
    }

    fn highlight_json(id: u64, location: i64, text: &str) -> String {
        format!(
            r#"{{"id": {id}, "text": "{text}", "location": {location}, "location_type": "location",
                "note": null, "highlighted_at": "2026-09-28T12:00:00Z", "tags": [],
                "is_discard": false, "readwise_url": "https://readwise.io/open/{id}"}}"#
        )
    }

    fn json_response(status: u16, body: String) -> Response<AsyncBody> {
        Response::builder()
            .status(status)
            .body(AsyncBody::from(body))
            .unwrap()
    }

    /// A service running the real HTTP transport against the fake client
    /// the test installed.
    fn start_service(
        project: &Entity<Project>,
        cx: &mut TestAppContext,
    ) -> Entity<ReadwiseService> {
        let service = cx.new(|cx| ReadwiseService::new(project.clone(), cx));
        service.update(cx, |service, cx| {
            let transport = Arc::new(HttpReadwiseTransport::new(
                cx.http_client(),
                "secret-token".to_string(),
            ));
            service.configure_for_test(test_vault(), ReadwiseConfig::default(), transport, cx)
        });
        service
    }

    #[gpui::test]
    async fn first_sync_lands_the_library_across_pages(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let http = FakeHttpClient::create({
            let requests = requests.clone();
            move |request| {
                let requests = requests.clone();
                async move {
                    let uri = request.uri().to_string();
                    assert_eq!(
                        request.headers().get("Authorization").unwrap(),
                        "Token secret-token"
                    );
                    requests.lock().unwrap().push(uri.clone());
                    let body = if uri.contains("pageCursor=p2") {
                        format!(
                            r#"{{"count": 1, "nextPageCursor": null, "results": [{{
                                "user_book_id": 2, "title": "Second Book", "author": "B",
                                "category": "books", "highlights": [{}]}}]}}"#,
                            highlight_json(20, 5, "From the second book")
                        )
                    } else {
                        format!(
                            r#"{{"count": 1, "nextPageCursor": "p2", "results": [{}, {{
                                "user_book_id": 3, "title": "An Article", "category": "articles",
                                "highlights": [{}]}}]}}"#,
                            book_json(&highlight_json(10, 426, "First")),
                            highlight_json(30, 1, "Unmapped")
                        )
                    };
                    Ok(json_response(200, body))
                }
            }
        });
        cx.update(|cx| cx.set_http_client(http));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = start_service(&project, cx);
        cx.run_until_parked();

        // Both pages landed, one note per book, in the plugin template; the
        // unmapped article was dropped.
        let note = fs.load(Path::new(BOOK_PATH)).await.unwrap();
        assert!(
            note.starts_with("---\nsource: readwise\nreadwise_id: 28374651\n"),
            "{note}"
        );
        assert!(note.contains("- Author: [[Timothy Keller]]"), "{note}");
        assert!(
            note.contains("- First ([Location 426](https://readwise.io/to_kindle?action=open&asin=B06XTSG7LR&location=426)) <!--rw:10@"),
            "{note}"
        );
        let second = fs
            .load(Path::new("/vault/reference/readwise/books/Second Book.md"))
            .await
            .unwrap();
        assert!(second.contains("From the second book"), "{second}");
        assert!(
            !fs.is_dir(Path::new("/vault/reference/readwise/articles"))
                .await,
            "unmapped categories must not land"
        );

        // State: one row per highlight, and a watermark.
        let landed = fs
            .load(Path::new("/vault/.thock/state/readwise/landed.jsonl"))
            .await
            .unwrap();
        assert_eq!(landed.lines().count(), 2, "{landed}");
        assert!(landed.contains("\"highlight\":10"), "{landed}");
        let cursor = fs
            .load(Path::new("/vault/.thock/state/readwise/cursor.json"))
            .await
            .unwrap();
        assert!(cursor.contains("updated_after"), "{cursor}");
        service.read_with(cx, |service, _| {
            assert!(matches!(service.state(), SyncState::Synced { .. }));
            assert_eq!(service.last_landed(), 2);
            assert!(!service.importing_library());
        });

        // The first pass was the full export; the next one is incremental.
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2, "{requests:?}");
            assert!(!requests[0].contains("updatedAfter"), "{requests:?}");
        }
        cx.executor().advance_clock(Duration::from_secs(3601));
        cx.run_until_parked();
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 4, "{requests:?}");
            assert!(requests[2].contains("updatedAfter="), "{requests:?}");
        }
        // Idempotent: the same export changes nothing.
        assert_eq!(fs.load(Path::new(BOOK_PATH)).await.unwrap(), note);
        assert_eq!(
            fs.load(Path::new("/vault/.thock/state/readwise/landed.jsonl"))
                .await
                .unwrap(),
            landed
        );
        service.read_with(cx, |service, _| assert_eq!(service.last_landed(), 0));
    }

    #[gpui::test]
    async fn incremental_sync_appends_without_touching_other_lines(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault/reference/readwise/books"))
            .await
            .unwrap();
        // A landed note the user has reworded and extended, plus a highlight
        // (id 11) that landed once and was then deleted from the note.
        let existing = "---\nsource: readwise\nreadwise_id: 28374651\ncategory: books\n---\n\
                        # A Fé Na Era Do Ceticismo\n\n## Metadata\n- Author: [[Timothy Keller]]\n\n\
                        ## Highlights\n- First, reworded by me ([Location 426](x)) <!--rw:10@2026-09-28-->\n\
                        \x20   - Note: mine\n\n## My thoughts\n\nKeep this.\n";
        fs.insert_file(Path::new(BOOK_PATH), existing.as_bytes().to_vec())
            .await;
        fs.create_dir(Path::new("/vault/.thock/state/readwise"))
            .await
            .unwrap();
        fs.insert_file(
            Path::new("/vault/.thock/state/readwise/landed.jsonl"),
            b"{\"highlight\":10,\"book\":28374651,\"path\":\"x\",\"at\":\"t\"}\n\
              {\"highlight\":11,\"book\":28374651,\"path\":\"x\",\"at\":\"t\"}\n"
                .to_vec(),
        )
        .await;
        fs.insert_file(
            Path::new("/vault/.thock/state/readwise/cursor.json"),
            b"{\"updated_after\":\"2026-09-27T00:00:00Z\"}".to_vec(),
        )
        .await;
        let http = FakeHttpClient::create(|request| async move {
            let uri = request.uri().to_string();
            assert!(uri.contains("updatedAfter=2026-09-27T00"), "{uri}");
            let highlights = [
                highlight_json(10, 426, "First"),
                highlight_json(11, 500, "Deleted by the user"),
                highlight_json(12, 90, "Late but early in the book"),
            ]
            .join(",");
            Ok(json_response(
                200,
                format!(
                    r#"{{"count": 1, "nextPageCursor": null, "results": [{}]}}"#,
                    book_json(&highlights)
                ),
            ))
        });
        cx.update(|cx| cx.set_http_client(http));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = start_service(&project, cx);
        cx.run_until_parked();

        let note = fs.load(Path::new(BOOK_PATH)).await.unwrap();
        let (before_thoughts, _) = existing
            .split_once("\n## My thoughts")
            .expect("fixture has a thoughts section");
        let expected = before_thoughts.replace(
            "    - Note: mine\n",
            "    - Note: mine\n- Late but early in the book ([Location 90](https://readwise.io/to_kindle?action=open&asin=B06XTSG7LR&location=90)) <!--rw:12@",
        );
        assert!(note.starts_with(&expected), "{note}");
        assert!(
            note.ends_with("-->\n\n## My thoughts\n\nKeep this.\n"),
            "{note}"
        );
        assert!(!note.contains("Deleted by the user"), "{note}");
        assert_eq!(note.matches("First").count(), 1, "{note}");
        service.read_with(cx, |service, _| assert_eq!(service.last_landed(), 1));
    }

    #[gpui::test]
    async fn rate_limit_waits_for_retry_after(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let http = FakeHttpClient::create({
            let calls = calls.clone();
            move |_request| {
                let calls = calls.clone();
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Ok(Response::builder()
                            .status(429)
                            .header("Retry-After", "30")
                            .body(AsyncBody::from("slow down".to_string()))
                            .unwrap());
                    }
                    Ok(json_response(
                        200,
                        format!(
                            r#"{{"count": 1, "nextPageCursor": null, "results": [{}]}}"#,
                            book_json(&highlight_json(10, 426, "First"))
                        ),
                    ))
                }
            }
        });
        cx.update(|cx| cx.set_http_client(http));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let _service = start_service(&project, cx);
        cx.run_until_parked();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            !fs.is_file(Path::new(BOOK_PATH)).await,
            "must wait out Retry-After"
        );

        cx.executor().advance_clock(Duration::from_secs(29));
        cx.run_until_parked();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(fs.is_file(Path::new(BOOK_PATH)).await);
    }

    #[gpui::test]
    async fn rejected_token_disconnects_and_stops_polling(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let http = FakeHttpClient::create({
            let calls = calls.clone();
            move |_request| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(json_response(
                        401,
                        "{\"detail\":\"Invalid token.\"}".to_string(),
                    ))
                }
            }
        });
        cx.update(|cx| cx.set_http_client(http));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = start_service(&project, cx);
        cx.run_until_parked();
        service.read_with(cx, |service, _| {
            assert!(matches!(service.state(), SyncState::Disconnected));
            assert!(service.transport.is_none());
        });
        cx.executor().advance_clock(Duration::from_secs(4 * 3600));
        cx.run_until_parked();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a rejected token ends the loop"
        );
        assert!(
            !fs.is_file(Path::new("/vault/.thock/state/readwise/cursor.json"))
                .await
        );

        // `validate_token` tells the prompt the same thing.
        let http = cx.update(|cx| cx.http_client());
        let error = validate_token(&http, "secret-token").await.unwrap_err();
        assert!(error.is::<TokenRejected>());
        assert_eq!(
            describe_connect_error(&error),
            "Readwise didn't accept that token."
        );
    }

    #[gpui::test]
    async fn mid_pagination_failure_leaves_the_watermark_unmoved(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let failing = Arc::new(Mutex::new(true));
        let http = FakeHttpClient::create({
            let failing = failing.clone();
            move |request| {
                let failing = failing.clone();
                async move {
                    let uri = request.uri().to_string();
                    if uri.contains("pageCursor=p2") {
                        if *failing.lock().unwrap() {
                            return Ok(json_response(500, "boom".to_string()));
                        }
                        return Ok(json_response(
                            200,
                            format!(
                                r#"{{"count": 1, "nextPageCursor": null, "results": [{{
                                    "user_book_id": 2, "title": "Second Book", "category": "books",
                                    "highlights": [{}]}}]}}"#,
                                highlight_json(20, 5, "Second")
                            ),
                        ));
                    }
                    Ok(json_response(
                        200,
                        format!(
                            r#"{{"count": 1, "nextPageCursor": "p2", "results": [{}]}}"#,
                            book_json(&highlight_json(10, 426, "First"))
                        ),
                    ))
                }
            }
        });
        cx.update(|cx| cx.set_http_client(http));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = start_service(&project, cx);
        cx.run_until_parked();
        // Nothing from the good page landed either: the pass aborted before
        // any apply, and there is no watermark to skip data with.
        assert!(!fs.is_file(Path::new(BOOK_PATH)).await);
        assert!(
            !fs.is_file(Path::new("/vault/.thock/state/readwise/cursor.json"))
                .await
        );
        service.read_with(cx, |service, _| {
            assert!(
                matches!(service.state(), SyncState::Failing { .. }),
                "{:?}",
                service.state()
            );
            assert!(service.config_error().is_none());
        });

        // The retry backs off to twice the interval, then lands everything.
        *failing.lock().unwrap() = false;
        cx.executor().advance_clock(Duration::from_secs(3601));
        cx.run_until_parked();
        assert!(
            !fs.is_file(Path::new(BOOK_PATH)).await,
            "backoff doubled the delay"
        );
        cx.executor().advance_clock(Duration::from_secs(3601));
        cx.run_until_parked();
        assert!(fs.is_file(Path::new(BOOK_PATH)).await);
        assert!(
            fs.is_file(Path::new("/vault/reference/readwise/books/Second Book.md"))
                .await
        );
        assert!(
            fs.is_file(Path::new("/vault/.thock/state/readwise/cursor.json"))
                .await
        );
    }

    #[gpui::test]
    async fn lost_state_is_rebuilt_from_the_vault_scan(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let http = FakeHttpClient::create(|_request| async move {
            Ok(json_response(
                200,
                format!(
                    r#"{{"count": 1, "nextPageCursor": null, "results": [{}]}}"#,
                    book_json(
                        &[
                            highlight_json(10, 426, "First"),
                            highlight_json(11, 500, "Second"),
                        ]
                        .join(",")
                    )
                ),
            ))
        });
        cx.update(|cx| cx.set_http_client(http));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = start_service(&project, cx);
        cx.run_until_parked();
        let note = fs.load(Path::new(BOOK_PATH)).await.unwrap();
        assert_eq!(note_readwise_id(&note), Some(28374651));

        // The state folder vanishes; the user also deletes one line.
        fs.remove_dir(
            Path::new("/vault/.thock/state/readwise"),
            fs::RemoveOptions {
                recursive: true,
                ignore_if_not_exists: false,
            },
        )
        .await
        .unwrap();
        let trimmed = note.replace(
            &note
                .lines()
                .find(|line| line.contains("Second"))
                .map(|line| format!("{line}\n"))
                .unwrap(),
            "",
        );
        fs.atomic_write(PathBuf::from(BOOK_PATH), trimmed.clone())
            .await
            .unwrap();
        service.update(cx, |service, cx| {
            let transport = Arc::new(HttpReadwiseTransport::new(
                cx.http_client(),
                "secret-token".to_string(),
            ));
            service.configure_for_test(test_vault(), ReadwiseConfig::default(), transport, cx)
        });
        cx.run_until_parked();

        // No second note, the marked highlight repaired into the state, and
        // — with no record of the deleted one — it comes back exactly once.
        // From here on it is in the state and stays gone if deleted again.
        let after = fs.load(Path::new(BOOK_PATH)).await.unwrap();
        assert_eq!(after.matches("First").count(), 1, "{after}");
        assert_eq!(after.matches("Second").count(), 1, "{after}");
        assert!(
            !fs.is_file(Path::new(
                "/vault/reference/readwise/books/A Fé Na Era Do Ceticismo (2).md"
            ))
            .await
        );
        let landed = fs
            .load(Path::new("/vault/.thock/state/readwise/landed.jsonl"))
            .await
            .unwrap();
        assert!(landed.contains("\"highlight\":10"), "{landed}");
        assert!(landed.contains("\"highlight\":11"), "{landed}");

        let trimmed_again = after.replace(
            &after
                .lines()
                .find(|line| line.contains("Second"))
                .map(|line| format!("{line}\n"))
                .unwrap(),
            "",
        );
        fs.atomic_write(PathBuf::from(BOOK_PATH), trimmed_again.clone())
            .await
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(3601));
        cx.run_until_parked();
        assert_eq!(fs.load(Path::new(BOOK_PATH)).await.unwrap(), trimmed_again);
    }

    #[gpui::test]
    async fn appends_into_an_open_dirty_buffer(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault/reference/readwise/books"))
            .await
            .unwrap();
        let existing = "---\nsource: readwise\nreadwise_id: 28374651\n---\n# A Fé Na Era Do Ceticismo\n\n\
                        ## Highlights\n- First <!--rw:10@2026-09-28-->\n";
        fs.insert_file(Path::new(BOOK_PATH), existing.as_bytes().to_vec())
            .await;
        let http = FakeHttpClient::create(|_request| async move {
            Ok(json_response(
                200,
                format!(
                    r#"{{"count": 1, "nextPageCursor": null, "results": [{}]}}"#,
                    book_json(
                        &[
                            highlight_json(10, 426, "First"),
                            highlight_json(12, 900, "Fresh"),
                        ]
                        .join(",")
                    )
                ),
            ))
        });
        cx.update(|cx| cx.set_http_client(http));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(Path::new(BOOK_PATH), cx)
            })
            .await
            .unwrap();
        buffer.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "An unsaved thought.\n")], None, cx);
        });

        let _service = start_service(&project, cx);
        cx.run_until_parked();
        // The typing guard waits for two quiet seconds.
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();

        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert!(text.starts_with("An unsaved thought.\n---\n"), "{text}");
        assert!(
            text.contains(
                "- First <!--rw:10@2026-09-28-->\n- Fresh ([Location 900](https://readwise.io/to_kindle?action=open&asin=B06XTSG7LR&location=900)) <!--rw:12@"
            ),
            "{text}"
        );
        assert!(buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        // The file on disk is untouched: the buffer owns it until saved.
        assert_eq!(fs.load(Path::new(BOOK_PATH)).await.unwrap(), existing);
    }
}
