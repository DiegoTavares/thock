//! The unified Gmail sync service (spec `v15-unified-gmail-sync.md` §7): one
//! GPUI entity per local project that polls every mapped label, lands one
//! note per thread in the mapping's folder, and applies everything in
//! crash-safe order — note files first, then landing integrations (the
//! backlog append), then the dedup state. It lives independently of the
//! Backlog panel; the panel only displays the status this service exposes.
//!
//! After each poll it also runs the Gmail archive pass (V53): new lines in
//! the triage log take their thread out of the Gmail inbox, and undone ones
//! put it back.

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Local, Utc};
use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use fs::Fs;
use gpui::{
    Action as _, App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, Global,
    SharedString, Subscription, Task, TaskExt as _, WeakEntity, Window, actions,
};
use project::Project;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use text::{Bias, Point};
use ui::IconName;
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{OpenOptions, OpenVisible, Toast, Workspace};

use crate::backlog::{
    DEFAULT_BACKLOG, EMAIL_ARCHIVE_DIR, SectionKind, append_to_section_edit, apply_edits,
    email_capture_line,
};
use crate::calendar_service::ConnectGoogleWorkspace;
use crate::calendar_service::{ManualSyncFinished, SyncState, show_sync_toast};
use crate::gmail::{
    ARCHIVE_GRACE, ArchiveConfig, ArchiveEvent, ArchiveState, GMAIL_CONFIG_FILE, GmailConfig,
    MailTransport, MappingFetched, MissingPermission, ThreadMove, archive_frontmatter_digest,
    logged_decisions, parse_gmail_config, parse_imported_index, plan_archive, scan_backlog_markers,
};
use crate::gmail_google::GmailTransport;
use crate::google_auth::{
    self, AuthRevoked, GOOGLE_CONFIG_FILE, GoogleClient, resolve_google_settings,
};
use crate::inbox::{
    INBOX_CONFIG_FILE, ImportRecord, TRIAGE_LOG_PATH, inbox_note_digest, parse_inbox_config,
    plan_inbox_capture, scan_triage_log_markers,
};
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

/// Same typing guard and backoff as the calendar service (V8 §9 guard 1).
const TYPING_GUARD_QUIET: Duration = Duration::from_secs(2);
const TYPING_GUARD_MAX_TRIES: usize = 15;
const BACKOFF_CEILING: Duration = Duration::from_secs(60 * 60);

const STATE_DIR: &str = "state/gmail";
const STATE_FILE: &str = "imported.jsonl";
/// Gmail archive's record (V53 §7.2), beside the capture state.
const ARCHIVE_STATE_FILE: &str = "archive.jsonl";
/// Read for its digests during dedup (pre-V15 Gmail captures were recorded
/// there); never written by this service.
const INBOX_STATE_DIR: &str = "state/inbox";

actions!(
    thock,
    [
        /// Checks Gmail for newly labeled emails and lands them in their
        /// mapped folders now.
        SyncGmailNow,
        /// Lets Thock archive an email in Gmail when you file it from the
        /// inbox, so you only triage it once. Asks Google for permission
        /// first.
        EnableGmailArchive,
        /// Stops archiving emails in Gmail when you file them. Nothing
        /// already archived moves.
        DisableGmailArchive,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        cx.subscribe(&service, |workspace, _, event: &ManualSyncFinished, cx| {
            show_sync_toast(workspace, event, cx);
        })
        .detach();
        let project_id = project.entity_id();
        cx.default_global::<GlobalGmailServices>()
            .0
            .insert(project_id, service);
        cx.on_release(move |_, cx| {
            cx.default_global::<GlobalGmailServices>()
                .0
                .remove(&project_id);
        })
        .detach();

        workspace.register_action(|workspace, _: &SyncGmailNow, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.sync_now(cx));
            }
        });
        workspace.register_action(|workspace, _: &EnableGmailArchive, _window, cx| {
            let handle = workspace.weak_handle();
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.enable_archive(handle, cx));
            }
        });
        workspace.register_action(|workspace, _: &DisableGmailArchive, _window, cx| {
            let handle = workspace.weak_handle();
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.disable_archive(handle, cx));
            }
        });
    })
    .detach();
}

/// The toast Gmail archive's on and off actions show.
struct GmailArchiveToast;

/// Where Gmail archive stands, for the connector row (V53 §8).
#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveStatus {
    Off,
    /// `[archive]` is in a shape this build can't read.
    Unreadable,
    /// On, but the Google grant lacks `gmail.modify`.
    NeedsPermission,
    /// The last pass couldn't archive `emails` threads, or couldn't record
    /// what it did when `emails` is 0.
    Failing {
        emails: usize,
        error: SharedString,
    },
    Healthy,
}

/// What one archive pass ended with.
#[derive(Default)]
struct ArchivePass {
    permission_missing: bool,
    failed: usize,
    error: Option<String>,
}

#[derive(Default)]
struct GlobalGmailServices(HashMap<EntityId, Entity<GmailService>>);

impl Global for GlobalGmailServices {}

/// A service for `project`, registered the way `init` would, without a
/// workspace.
#[cfg(test)]
pub(crate) fn new_for_test(project: &Entity<Project>, cx: &mut App) -> Entity<GmailService> {
    let service = cx.new(|cx| GmailService::new(project.clone(), cx));
    cx.default_global::<GlobalGmailServices>()
        .0
        .insert(project.entity_id(), service.clone());
    service
}

/// The sync service for `project`, if one is running.
pub fn service_for_project(project: &Entity<Project>, cx: &App) -> Option<Entity<GmailService>> {
    cx.try_global::<GlobalGmailServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

enum SyncOutcome {
    Synced {
        /// Notes landed by this poll, for the manual-sync toast.
        captured: usize,
    },
    Held(gpui::SharedString),
    Failed(anyhow::Error),
    AuthRevoked,
    /// The service was reconfigured or released mid-sync.
    Aborted,
}

/// One mapped folder's on-disk answer, fresh every poll — triage moves files
/// out of `inbox/` between polls, so a cached scan would go stale.
#[derive(Default)]
struct DirScan {
    stems: HashSet<String>,
    /// Digest → stem, for the crash-window repair of the backlog line
    /// (spec §7.2) and stem-collision handling.
    digest_stems: HashMap<String, String>,
}

/// What one poll learned from the vault before fetching (spec §4.4).
#[derive(Default)]
struct VaultScan {
    /// Index-aligned with the configured mappings.
    dirs: Vec<DirScan>,
    backlog_markers: HashSet<String>,
    /// Every digest recorded anywhere in the vault: `capture:` and legacy
    /// `thread:` frontmatter in mapped folders, plus triage-log markers.
    digests: HashSet<String>,
}

pub struct GmailService {
    project: Entity<Project>,
    vault: Option<Vault>,
    config: Option<GmailConfig>,
    transport: Option<Arc<dyn MailTransport>>,
    state: SyncState,
    /// The one poll loop. Replacing it on reload cancels the old loop; the
    /// apply work it spawns is awaited inside it, never stored separately.
    poll_task: Option<Task<()>>,
    /// Digests from the state files, loaded once per transport start. The
    /// vault-side record is re-scanned per poll instead.
    imported: Option<HashSet<String>>,
    /// Set by `sync_now` so the next completed sync announces itself
    /// ([`ManualSyncFinished`]); background polls stay quiet.
    announce_next_sync: bool,
    /// Archive passes read the state file and append to it, so they run one
    /// at a time: a lost `archived` record would archive a thread twice.
    archive_lock: Arc<futures::lock::Mutex<()>>,
    /// The pass pair [`GmailService::reconcile_soon`] schedules.
    reconcile_soon_task: Option<Task<()>>,
    /// The enable or disable flow in flight.
    archive_toggle_task: Option<Task<()>>,
    /// Learned from the grant or a refused write; cleared when the transport
    /// is rebuilt, which is what a new consent does.
    archive_permission_missing: bool,
    archive_failure: Option<(usize, SharedString)>,
    now: Arc<dyn Fn() -> DateTime<Utc>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ManualSyncFinished> for GmailService {}

impl GmailService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        let mut this = Self {
            project,
            vault: None,
            config: None,
            transport: None,
            state: SyncState::NoConfig,
            poll_task: None,
            imported: None,
            announce_next_sync: false,
            archive_lock: Arc::new(futures::lock::Mutex::new(())),
            reconcile_soon_task: None,
            archive_toggle_task: None,
            archive_permission_missing: false,
            archive_failure: None,
            now: Arc::new(Utc::now),
            _subscriptions: vec![project_subscription],
        };
        this.reload(cx);
        this
    }

    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// Gmail archive's part of the connector row.
    pub fn archive_status(&self) -> ArchiveStatus {
        match self.config.as_ref().map(|config| &config.archive) {
            None | Some(ArchiveConfig::Off) => ArchiveStatus::Off,
            Some(ArchiveConfig::Unreadable) => ArchiveStatus::Unreadable,
            Some(ArchiveConfig::On(_)) if self.archive_permission_missing => {
                ArchiveStatus::NeedsPermission
            }
            Some(ArchiveConfig::On(_)) => match &self.archive_failure {
                Some((emails, error)) => ArchiveStatus::Failing {
                    emails: *emails,
                    error: error.clone(),
                },
                None => ArchiveStatus::Healthy,
            },
        }
    }

    /// Runs an archive pass now and another once the grace period is over,
    /// so an inbox decision or its undo reaches Gmail without waiting for
    /// the next poll (V53 §7.4).
    pub fn reconcile_soon(&mut self, cx: &mut Context<Self>) {
        let archive_on = self
            .config
            .as_ref()
            .is_some_and(|config| config.archive.is_on());
        if !archive_on || self.transport.is_none() {
            return;
        }
        self.reconcile_soon_task = Some(cx.spawn(async move |this, cx| {
            Self::reconcile_archive(&this, cx).await;
            // A second past the grace, so the pass sees it fully elapsed.
            cx.background_executor()
                .timer(ARCHIVE_GRACE + Duration::from_secs(1))
                .await;
            Self::reconcile_archive(&this, cx).await;
        }));
    }

    /// Whether the status row should be shown at all: only when the vault
    /// carries a Gmail config (V9 §10.3, G5).
    pub fn has_config(&self) -> bool {
        !matches!(self.state, SyncState::NoConfig)
    }

    /// The workspace connect flow started (it is owned by the calendar
    /// service): show progress here too.
    pub fn mark_connecting(&mut self, cx: &mut Context<Self>) {
        if self.has_config() {
            self.state = SyncState::Connecting;
            cx.notify();
        }
    }

    /// The workspace sign-out ran: stop polling and forget the transport.
    /// The vault — notes, backlog, state — is never touched.
    pub fn mark_signed_out(&mut self, cx: &mut Context<Self>) {
        self.poll_task = None;
        self.transport = None;
        if self.config.is_some() {
            self.state = SyncState::NeverConnected;
        }
        cx.notify();
    }

    /// Re-reads `.thock/gmail.toml` — the connect flow calls this after
    /// writing `google.toml`.
    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    /// `thock::SyncGmailNow`: restarts the loop, which checks immediately.
    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.transport.is_some() {
            self.announce_next_sync = true;
            self.start_poll(cx);
        }
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
                let gmail_config = format!("{VAULT_MARKER_DIR}/{GMAIL_CONFIG_FILE}");
                let google_config = format!("{VAULT_MARKER_DIR}/{GOOGLE_CONFIG_FILE}");
                let vault_config = format!("{VAULT_MARKER_DIR}/{VAULT_CONFIG_FILE}");
                if changes.iter().any(|(path, _, _)| {
                    let path = path.as_unix_str();
                    path == gmail_config || path == google_config || path == vault_config
                }) {
                    self.reload(cx);
                }
            }
            _ => {}
        }
    }

    /// Re-resolves the vault and `.thock/gmail.toml`, rebuilding the
    /// transport and poll loop when the configuration actually changed.
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

        let config_path = vault.root.join(VAULT_MARKER_DIR).join(GMAIL_CONFIG_FILE);
        // Same synchronous read as `Vault::detect`; the file is tiny.
        let config = match std::fs::read_to_string(&config_path) {
            Err(_) => None,
            Ok(text) => match parse_gmail_config(&text) {
                Ok(mut config) => {
                    // The account and client override resolve across the
                    // Google config files (V13 §7.4), `google.toml` first.
                    let settings = resolve_google_settings(&vault.root, GMAIL_CONFIG_FILE);
                    config.account = settings.account;
                    config.google = settings.google;
                    Some(config)
                }
                Err(error) => {
                    // A hand-edited file that doesn't parse disables sync,
                    // never panics (spec §6).
                    log::warn!(
                        "Thock: couldn't parse {}: {error:#}; Gmail sync is off",
                        config_path.display()
                    );
                    self.vault = Some(vault);
                    self.clear_sync(SyncState::Failing {
                        error: "gmail.toml could not be read".into(),
                    });
                    cx.notify();
                    return;
                }
            },
        };
        self.vault = Some(vault);

        match config {
            None => self.clear_sync(SyncState::NoConfig),
            Some(config) if config.account.is_none() => {
                self.config = Some(config);
                self.transport = None;
                self.poll_task = None;
                self.state = SyncState::NeverConnected;
            }
            Some(config) => {
                let unchanged = self.config.as_ref() == Some(&config) && self.transport.is_some();
                if !unchanged {
                    self.config = Some(config);
                    // The account (and so every digest) may have changed:
                    // reload the dedup state from disk on the next poll.
                    self.imported = None;
                    match self.build_transport(cx) {
                        Ok(transport) => {
                            self.transport = Some(transport);
                            self.archive_permission_missing = false;
                            self.archive_failure = None;
                            self.state = SyncState::Idle;
                            self.start_poll(cx);
                        }
                        Err(error) => {
                            self.clear_sync(SyncState::Failing {
                                error: format!("{error:#}").into(),
                            });
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    fn clear_sync(&mut self, state: SyncState) {
        self.config = None;
        self.transport = None;
        self.poll_task = None;
        self.imported = None;
        self.state = state;
    }

    fn build_transport(&self, cx: &App) -> Result<Arc<dyn MailTransport>> {
        let config = self.config.as_ref().context("no gmail config")?;
        let account = config.account.clone().context("no account connected")?;
        let client = GoogleClient::resolve(&config.google)?;
        Ok(Arc::new(GmailTransport::new(
            cx.http_client(),
            client,
            account,
            config.mappings.clone(),
        )))
    }

    /// (Re)starts the poll loop: an immediate check, then one tick per
    /// `poll_seconds`, doubling up to an hour on transport errors.
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
                SyncOutcome::Synced { captured } => Some(match captured {
                    0 => "Gmail synced — nothing new".into(),
                    1 => "Gmail synced — 1 new email".into(),
                    n => format!("Gmail synced — {n} new emails").into(),
                }),
                SyncOutcome::Held(reason) => Some(format!("Gmail sync held — {reason}").into()),
                SyncOutcome::Failed(error) => Some(format!("Gmail sync failed — {error:#}").into()),
                SyncOutcome::AuthRevoked => {
                    Some("Google sign-in expired — reconnect to sync Gmail".into())
                }
            })
            .flatten();
        let keep_going = match outcome {
            SyncOutcome::Aborted => false,
            SyncOutcome::Synced { .. } => {
                self.state = SyncState::Synced { at: Instant::now() };
                *delay = interval;
                true
            }
            SyncOutcome::Held(reason) => {
                self.state = SyncState::Holding {
                    reason: reason.into(),
                };
                *delay = interval;
                true
            }
            SyncOutcome::Failed(error) => {
                log::warn!("Thock Gmail sync failed: {error:#}");
                self.state = SyncState::Failing {
                    error: format!("{error:#}").into(),
                };
                // Offline is just an error: back off, keep trying.
                *delay = (*delay * 2).min(BACKOFF_CEILING);
                true
            }
            SyncOutcome::AuthRevoked => {
                self.state = SyncState::Disconnected;
                self.transport = None;
                false
            }
        };
        if let Some(message) = announcement {
            cx.emit(ManualSyncFinished {
                message,
                icon: IconName::Envelope,
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
        let account = config.account.clone().unwrap_or_default();
        let fs = project.read_with(cx, |project, _| project.fs().clone());

        let imported = match Self::imported_state(this, &fs, &vault, cx).await {
            Ok(imported) => imported,
            Err(error) => return SyncOutcome::Failed(error),
        };
        let scan = scan_vault(&fs, &vault, &config).await;

        // Everything already recorded anywhere — state, mapped folders,
        // backlog markers, triage log — is skipped at the transport, so a
        // captured thread costs no per-message request (spec §7.1).
        let mut skip = imported.clone();
        skip.extend(scan.digests.iter().cloned());
        skip.extend(scan.backlog_markers.iter().cloned());

        let fetched = match transport.fetch(&skip, cx).await {
            Err(error) if error.is::<AuthRevoked>() => return SyncOutcome::AuthRevoked,
            Err(error) => return SyncOutcome::Failed(error),
            Ok(fetched) => fetched,
        };

        // Digests present in the vault record mean "repair the state, don't
        // write a second file" (spec §4.4); backlog markers count — a line
        // whose archive the user deleted stays deleted.
        let mut vault_digests = scan.digests.clone();
        vault_digests.extend(scan.backlog_markers.iter().cloned());

        let captured_at = Local::now().to_rfc3339();
        let mut holding: Vec<&str> = Vec::new();
        let mut plans = Vec::new();
        // Two mappings may share a folder: stems claimed by an earlier plan
        // must be taken for the later one.
        let mut stems_by_path: HashMap<&str, HashSet<String>> = HashMap::new();
        for (index, mapping) in config.mappings.iter().enumerate() {
            if let Some(scanned) = scan.dirs.get(index) {
                stems_by_path
                    .entry(mapping.path.as_str())
                    .or_default()
                    .extend(scanned.stems.iter().cloned());
            }
        }
        for (index, fetch) in fetched.mappings.iter().enumerate() {
            let Some(mapping) = config.mappings.get(index) else {
                continue;
            };
            match fetch {
                MappingFetched::LabelNotFound => holding.push(&mapping.label),
                MappingFetched::Items(items) if items.is_empty() => {}
                MappingFetched::Items(items) => {
                    let taken = stems_by_path.entry(mapping.path.as_str()).or_default();
                    let plan = plan_inbox_capture(
                        items,
                        &account,
                        &imported,
                        &vault_digests,
                        taken,
                        &mapping.path,
                        &captured_at,
                    );
                    taken.extend(plan.files.iter().map(|file| file.stem.clone()));
                    if !plan.is_empty() {
                        plans.push((index, plan));
                    }
                }
            }
        }

        let mut captured = 0;
        if !plans.is_empty() {
            captured = plans.iter().map(|(_, plan)| plan.files.len()).sum();
            if let Err(error) = Self::apply_plans(
                this,
                &fs,
                &vault,
                &config,
                &project,
                &scan,
                plans,
                &captured_at,
                cx,
            )
            .await
            {
                return SyncOutcome::Failed(error);
            }
        }

        Self::reconcile_archive(this, cx).await;

        if let Some(reason) = hold_reason(&holding) {
            return SyncOutcome::Held(reason.into());
        }
        SyncOutcome::Synced { captured }
    }

    /// One Gmail archive pass (V53 §7), serialized with any other. Its
    /// outcome lands in [`GmailService::archive_status`]; capture never
    /// fails because of it.
    async fn reconcile_archive(this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        let context = this
            .read_with(cx, |service, _| {
                Some((
                    service.transport.clone()?,
                    service.config.clone()?,
                    service.vault.clone()?,
                    service.project.clone(),
                    service.archive_lock.clone(),
                ))
            })
            .ok()
            .flatten();
        let Some((transport, config, vault, project, lock)) = context else {
            return;
        };
        let _guard = lock.lock().await;
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let outcome = Self::archive_pass(this, &transport, &config, &vault, &fs, cx).await;
        this.update(cx, |service, cx| {
            match outcome {
                Ok(pass) => {
                    service.archive_permission_missing = pass.permission_missing;
                    service.archive_failure = pass
                        .error
                        .filter(|_| pass.failed > 0)
                        .map(|error| (pass.failed, error.into()));
                }
                Err(error) => {
                    log::warn!("Thock: the Gmail archive pass failed: {error:#}");
                    service.archive_failure = Some((0, format!("{error:#}").into()));
                }
            }
            cx.notify();
        })
        .ok();
    }

    async fn archive_pass(
        this: &WeakEntity<Self>,
        transport: &Arc<dyn MailTransport>,
        config: &GmailConfig,
        vault: &Vault,
        fs: &Arc<dyn Fs>,
        cx: &mut AsyncApp,
    ) -> Result<ArchivePass> {
        let (now, permission_missing) = this.read_with(cx, |service, _| {
            (service.now.clone(), service.archive_permission_missing)
        })?;
        let state_path = archive_state_path(vault);
        // A missing record is a fresh start; a lost one costs at most a
        // redundant `INBOX` removal.
        let mut contents = fs.load(&state_path).await.unwrap_or_default();
        let state = ArchiveState::from_jsonl(&contents);
        let mut pass = ArchivePass::default();
        let mut events = Vec::new();
        match &config.archive {
            ArchiveConfig::On(on) => {
                let imported = fs.load(&state_file_path(vault)).await.unwrap_or_default();
                let log = fs
                    .load(&vault.root.join(TRIAGE_LOG_PATH))
                    .await
                    .unwrap_or_default();
                let headings = &vault.config.backlog.headings;
                let decisions = logged_decisions(&log, &headings.soon, &headings.someday);
                let plan = plan_archive(
                    &decisions,
                    &parse_imported_index(&imported),
                    &state,
                    on,
                    &inbox_dir(fs, vault).await,
                    now(),
                );
                events = plan.events;
                pass.permission_missing =
                    permission_missing || transport.archive_permission() == Some(false);
                for action in plan.actions {
                    if pass.permission_missing {
                        break;
                    }
                    match transport
                        .modify_thread(&action.thread, action.change, cx)
                        .await
                    {
                        Ok(()) => events.push(match action.change {
                            ThreadMove::Archive => ArchiveEvent::Archived {
                                digest: action.digest,
                                thread: action.thread,
                                at: now(),
                            },
                            ThreadMove::Unarchive => ArchiveEvent::Unarchived {
                                digest: action.digest,
                                thread: action.thread,
                                at: now(),
                            },
                        }),
                        Err(error) if error.is::<MissingPermission>() => {
                            pass.permission_missing = true;
                        }
                        Err(error) => {
                            log::warn!("Thock: couldn't update a thread in Gmail: {error:#}");
                            pass.failed += 1;
                            pass.error = Some(format!("{error:#}"));
                            events.push(ArchiveEvent::Failed {
                                digest: action.digest,
                                thread: action.thread,
                                at: now(),
                                error: format!("{error:#}"),
                            });
                        }
                    }
                }
            }
            // Off: remember that, so turning it on again starts from a new
            // baseline instead of acting on what was pending.
            ArchiveConfig::Off | ArchiveConfig::Unreadable => {
                if state.last_on.as_ref().is_some_and(|on| !on.is_empty()) {
                    events.push(ArchiveEvent::On {
                        classes: BTreeSet::new(),
                        at: now(),
                    });
                }
            }
        }
        if events.is_empty() {
            return Ok(pass);
        }
        for event in &events {
            contents.push_str(&event.to_json_line());
            contents.push('\n');
        }
        if let Some(parent) = state_path.parent() {
            fs.create_dir(parent).await?;
        }
        fs.atomic_write(state_path, contents)
            .await
            .context("recording Gmail archive state")?;
        Ok(pass)
    }

    /// `thock::EnableGmailArchive` (V53 §5.1): Google's consent for
    /// `gmail.modify` alone, then `[archive]` in `.thock/gmail.toml`.
    fn enable_archive(&mut self, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) {
        let context = match (&self.vault, &self.config) {
            (Some(vault), Some(config)) => config
                .account
                .clone()
                .map(|account| (vault.root.clone(), account, config.google.clone())),
            _ => None,
        };
        let Some((vault_root, account, overrides)) = context else {
            show_archive_toast(
                &workspace,
                "Connect Google Workspace first, then turn on Gmail archive.".into(),
                None,
                cx,
            );
            return;
        };
        let http = cx.http_client();
        let fs = self.project.read(cx).fs().clone();
        self.archive_toggle_task = Some(cx.spawn(async move |this, cx| {
            let config_path = vault_root.join(VAULT_MARKER_DIR).join(GMAIL_CONFIG_FILE);
            let result = async {
                let client = GoogleClient::resolve(&overrides)?;
                if !google_auth::grant_gmail_modify(http, client, &account, cx).await? {
                    return Ok(None);
                }
                let existing = fs.load(&config_path).await.unwrap_or_default();
                let updated = with_archive_table(&existing)?;
                if updated != existing {
                    fs.atomic_write(config_path.clone(), updated.clone())
                        .await?;
                }
                anyhow::Ok(Some(archive_table_row(&updated)))
            }
            .await;
            let (message, button): (SharedString, Option<ToastButton>) = match result {
                Ok(Some(row)) => {
                    // The new grant is in the keychain; a fresh transport
                    // reads it instead of the cached read-only token.
                    this.update(cx, |service, cx| {
                        service.transport = None;
                        service.reload(cx);
                    })
                    .ok();
                    let workspace = workspace.clone();
                    let path = config_path.clone();
                    (
                        "Gmail archive is on".into(),
                        Some((
                            "Edit which decisions",
                            Arc::new(move |window: &mut Window, cx: &mut App| {
                                open_at_row(workspace.clone(), path.clone(), row, window, cx)
                            }),
                        )),
                    )
                }
                Ok(None) => (
                    "Gmail archive stays off: Google wasn't given the permission".into(),
                    None,
                ),
                Err(error) => {
                    log::warn!("Thock: turning on Gmail archive failed: {error:#}");
                    (
                        format!("Couldn't turn on Gmail archive: {error:#}").into(),
                        None,
                    )
                }
            };
            cx.update(|cx| show_archive_toast(&workspace, message, button, cx));
        }));
    }

    /// `thock::DisableGmailArchive` (V53 §5.3): removes `[archive]`. The
    /// toast offers to drop the permission itself, which means revoking the
    /// grant and connecting again read-only.
    fn disable_archive(&mut self, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let config_path = vault.root.join(VAULT_MARKER_DIR).join(GMAIL_CONFIG_FILE);
        let fs = self.project.read(cx).fs().clone();
        self.archive_toggle_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let Ok(existing) = fs.load(&config_path).await else {
                    return anyhow::Ok(());
                };
                let updated = without_archive_table(&existing)?;
                if updated != existing {
                    fs.atomic_write(config_path, updated).await?;
                }
                Ok(())
            }
            .await;
            let (message, button): (SharedString, Option<ToastButton>) = match result {
                Ok(()) => {
                    this.update(cx, |service, cx| service.reload(cx)).ok();
                    let error_workspace = workspace.clone();
                    (
                        "Gmail archive is off".into(),
                        Some((
                            "Reconnect read-only",
                            Arc::new(move |window: &mut Window, cx: &mut App| {
                                reconnect_read_only(error_workspace.clone(), window, cx)
                            }),
                        )),
                    )
                }
                Err(error) => (
                    format!("Couldn't turn off Gmail archive: {error:#}").into(),
                    None,
                ),
            };
            cx.update(|cx| show_archive_toast(&workspace, message, button, cx));
        }));
    }

    /// The state-file digests, loaded once per transport start (spec §4.4).
    /// Pre-V15 Gmail captures recorded in the inbox state file are read too,
    /// so nothing the old stack imported is ever captured twice (spec §9).
    async fn imported_state(
        this: &WeakEntity<Self>,
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        cx: &mut AsyncApp,
    ) -> Result<HashSet<String>> {
        if let Some(imported) = this.read_with(cx, |service, _| service.imported.clone())? {
            return Ok(imported);
        }
        let mut imported = HashSet::new();
        for state_dir in [STATE_DIR, INBOX_STATE_DIR] {
            let path = vault
                .root
                .join(VAULT_MARKER_DIR)
                .join(state_dir)
                .join(STATE_FILE);
            let Ok(contents) = fs.load(&path).await else {
                continue;
            };
            for line in contents.lines() {
                if let Ok(record) = serde_json::from_str::<serde_json::Value>(line)
                    && let Some(digest) = record.get("digest").and_then(|value| value.as_str())
                {
                    imported.insert(digest.to_string());
                }
            }
        }
        this.update(cx, |service, _| service.imported = Some(imported.clone()))?;
        Ok(imported)
    }

    /// Applies the mapped plans in crash-safe order (spec §7.2): note files
    /// first, create-if-missing, then the backlog landing integration, then
    /// the state append. A crash at any boundary re-plans next poll into a
    /// state repair, never a duplicate.
    #[allow(clippy::too_many_arguments)]
    async fn apply_plans(
        this: &WeakEntity<Self>,
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        config: &GmailConfig,
        project: &Entity<Project>,
        scan: &VaultScan,
        plans: Vec<(usize, crate::inbox::InboxPlan)>,
        captured_at: &str,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        for (_, plan) in &plans {
            for file in &plan.files {
                let path = vault.root.join(&file.rel_path);
                if fs.is_file(&path).await {
                    continue;
                }
                if let Some(parent) = path.parent() {
                    fs.create_dir(parent).await?;
                }
                fs.atomic_write(path, file.content.clone()).await?;
            }
        }

        // The landing integration (spec §4.3): notes in the email archive
        // folder get a Someday line. A record whose digest is already in the
        // backlog is settled; one that isn't is either fresh or the repair
        // of a crash between landing and appending — its stem comes from
        // this plan or from the folder scan.
        let mut lines: Vec<(String, String)> = Vec::new();
        for (index, plan) in &plans {
            let Some(mapping) = config.mappings.get(*index) else {
                continue;
            };
            if mapping.path != EMAIL_ARCHIVE_DIR {
                continue;
            }
            for record in &plan.newly_imported {
                if scan.backlog_markers.contains(&record.digest) {
                    continue;
                }
                let stem = plan
                    .files
                    .iter()
                    .find(|file| file.digest == record.digest)
                    .map(|file| file.stem.clone())
                    .or_else(|| {
                        scan.dirs
                            .get(*index)
                            .and_then(|dir| dir.digest_stems.get(&record.digest).cloned())
                    });
                if let Some(stem) = stem {
                    lines.push((
                        record.digest.clone(),
                        email_capture_line(&record.title, &stem, &record.digest),
                    ));
                }
            }
        }
        if !lines.is_empty() {
            Self::append_backlog_lines(fs, vault, project, lines, cx).await?;
        }

        let records: Vec<(&str, &ImportRecord)> = plans
            .iter()
            .flat_map(|(index, plan)| {
                let path = config
                    .mappings
                    .get(*index)
                    .map(|mapping| mapping.path.as_str())
                    .unwrap_or("");
                plan.newly_imported.iter().map(move |record| (path, record))
            })
            .collect();
        Self::append_state(fs, vault, &records, captured_at).await?;
        this.update(cx, |service, _| {
            if let Some(imported) = service.imported.as_mut() {
                imported.extend(records.iter().map(|(_, record)| record.digest.clone()));
            }
        })
        .ok();
        Ok(())
    }

    /// Appends the pending Someday lines, marker-guarded so retries and
    /// crash repairs never duplicate: through the open buffer as one
    /// finalized transaction behind the typing guard when `backlog.md` is
    /// open (undoable with one `u`, cannot clobber unsaved keystrokes),
    /// read-modify-write through the project `Fs` otherwise.
    async fn append_backlog_lines(
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        project: &Entity<Project>,
        lines: Vec<(String, String)>,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let backlog_path = vault.backlog_path();
        let buffer = project.update(cx, |project, cx| {
            project
                .project_path_for_absolute_path(&backlog_path, cx)
                .and_then(|path| project.get_open_buffer(&path, cx))
        });

        let pending_block = |text: &str| {
            let markers = scan_backlog_markers(text);
            let mut block = String::new();
            for (digest, line) in &lines {
                if markers.contains(digest) {
                    continue;
                }
                block.push_str(line);
                block.push('\n');
            }
            block
        };

        let Some(buffer) = buffer else {
            // A missing file is created from the scaffold — the backlog is a
            // core scaffolded file, not a user gesture like the daily note.
            let text = fs
                .load(&backlog_path)
                .await
                .unwrap_or_else(|_| DEFAULT_BACKLOG.to_string());
            let block = pending_block(&text);
            if block.is_empty() {
                return Ok(());
            }
            let edit = append_to_section_edit(
                &text,
                SectionKind::Someday,
                None,
                &block,
                &vault.config.backlog.headings,
            );
            fs.atomic_write(backlog_path, apply_edits(&text, vec![edit]))
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
            let block = pending_block(&text);
            if block.is_empty() {
                return Ok(());
            }
            let edit = append_to_section_edit(
                &text,
                SectionKind::Someday,
                None,
                &block,
                &vault.config.backlog.headings,
            );
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
            "the buffer kept changing while applying captured emails"
        ))
    }

    /// Appends the captured threads to `.thock/state/gmail/imported.jsonl`
    /// (spec §7.2).
    async fn append_state(
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        records: &[(&str, &ImportRecord)],
        captured_at: &str,
    ) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let path = state_file_path(vault);
        let mut contents = fs.load(&path).await.unwrap_or_default();
        for (mapping_path, record) in records {
            let entry = serde_json::json!({
                "digest": record.digest,
                "thread": record.external_id,
                "title": record.title,
                "path": mapping_path,
                "at": captured_at,
            });
            contents.push_str(&entry.to_string());
            contents.push('\n');
        }
        if let Some(parent) = path.parent() {
            fs.create_dir(parent).await?;
        }
        fs.atomic_write(path, contents).await
    }

    #[cfg(test)]
    pub(crate) fn set_state_for_test(&mut self, state: SyncState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn set_now_for_test(&mut self, now: Arc<dyn Fn() -> DateTime<Utc>>) {
        self.now = now;
    }

    #[cfg(test)]
    pub(crate) fn configure_for_test(
        &mut self,
        vault: Vault,
        config: GmailConfig,
        transport: Arc<dyn MailTransport>,
        cx: &mut Context<Self>,
    ) {
        self.vault = Some(vault);
        self.config = Some(config);
        self.transport = Some(transport);
        self.imported = None;
        self.archive_permission_missing = false;
        self.archive_failure = None;
        self.state = SyncState::Idle;
        self.start_poll(cx);
    }
}

/// A transport that returns canned items and records every write.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct StubTransport {
    /// Items per mapping, index-aligned.
    pub(crate) items: std::sync::Mutex<Vec<Vec<crate::inbox::CapturedItem>>>,
    pub(crate) skips_seen: std::sync::Mutex<Vec<HashSet<String>>>,
    /// Every `threads.modify` sent.
    pub(crate) modified: std::sync::Mutex<Vec<(String, ThreadMove)>>,
    /// When set, every write fails with it.
    pub(crate) modify_error: std::sync::Mutex<Option<String>>,
    pub(crate) permission: std::sync::Mutex<Option<bool>>,
}

#[cfg(test)]
impl MailTransport for StubTransport {
    fn modify_thread(
        &self,
        thread_id: &str,
        change: ThreadMove,
        _cx: &AsyncApp,
    ) -> Task<Result<()>> {
        if let Some(error) = self.modify_error.lock().unwrap().clone() {
            return Task::ready(Err(anyhow!(error)));
        }
        self.modified
            .lock()
            .unwrap()
            .push((thread_id.to_string(), change));
        Task::ready(Ok(()))
    }

    fn archive_permission(&self) -> Option<bool> {
        *self.permission.lock().unwrap()
    }

    fn fetch(
        &self,
        skip: &HashSet<String>,
        _cx: &AsyncApp,
    ) -> Task<Result<crate::gmail::GmailFetched>> {
        self.skips_seen.lock().unwrap().push(skip.clone());
        let mappings = self
            .items
            .lock()
            .unwrap()
            .clone()
            .into_iter()
            .map(MappingFetched::Items)
            .collect();
        Task::ready(Ok(crate::gmail::GmailFetched { mappings }))
    }
}

type ToastButton = (&'static str, Arc<dyn Fn(&mut Window, &mut App)>);

fn show_archive_toast(
    workspace: &WeakEntity<Workspace>,
    message: SharedString,
    button: Option<ToastButton>,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            let mut toast = Toast::new(
                NotificationId::unique::<GmailArchiveToast>(),
                message.to_string(),
            )
            .autohide();
            if let Some((label, on_click)) = button {
                toast = toast.on_click(label, move |window, cx| on_click(window, cx));
            }
            workspace.show_toast(toast, cx);
        })
        .log_err();
}

/// Opens `path` with the cursor at the start of `row`.
fn open_at_row(
    workspace: WeakEntity<Workspace>,
    path: PathBuf,
    row: u32,
    window: &mut Window,
    cx: &mut App,
) {
    window
        .spawn(cx, async move |cx| {
            let item = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_abs_path(
                        path,
                        OpenOptions {
                            visible: Some(OpenVisible::All),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                })?
                .await?;
            if let Some(editor) = item.downcast::<Editor>() {
                editor.update_in(cx, |editor, window, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let point = snapshot.clip_point(Point::new(row, 0), Bias::Left);
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()),
                        window,
                        cx,
                        |selections| selections.select_ranges([point..point]),
                    );
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
}

/// Revokes the Google grant, then runs the usual connect, which asks for
/// the read-only scopes alone now that `[archive]` is gone.
fn reconnect_read_only(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let http = cx.http_client();
    window
        .spawn(cx, async move |cx| {
            match google_auth::revoke_grant(http, cx).await {
                Ok(()) => {
                    cx.update(|window, cx| {
                        window.dispatch_action(ConnectGoogleWorkspace.boxed_clone(), cx)
                    })
                    .log_err();
                }
                Err(error) => {
                    log::warn!("Thock: revoking the Google grant failed: {error:#}");
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_error(
                                format!("Couldn't remove Thock's Google permission: {error:#}"),
                                cx,
                            )
                        })
                        .log_err();
                }
            }
        })
        .detach();
}

/// `.thock/gmail.toml` with an `[archive]` table that is on: added with the
/// default `on = ["archive"]` when missing or empty, left alone when it
/// already lists decisions. Everything else in the file keeps its bytes.
fn with_archive_table(text: &str) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text.parse().context("parsing .thock/gmail.toml")?;
    if !document.contains_key("schema") {
        document.insert("schema", toml_edit::value(2));
    }
    let listed = document
        .get("archive")
        .and_then(|archive| archive.get("on"))
        .and_then(|on| on.as_array())
        .is_some_and(|on| !on.is_empty());
    if listed {
        return Ok(document.to_string());
    }
    let mut on = toml_edit::Array::new();
    on.push("archive");
    match document
        .get_mut("archive")
        .and_then(|item| item.as_table_mut())
    {
        Some(table) => {
            table.insert("on", toml_edit::value(on));
        }
        None => {
            let mut table = toml_edit::Table::new();
            table.decor_mut().set_prefix(
                "\n# Which inbox decisions also archive the email in Gmail.\n\
                 # Any of: today, soon, someday, archive, discard, other.\n",
            );
            table.insert("on", toml_edit::value(on));
            document.insert("archive", toml_edit::Item::Table(table));
        }
    }
    Ok(document.to_string())
}

/// `.thock/gmail.toml` without its `[archive]` table, everything else kept.
fn without_archive_table(text: &str) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text.parse().context("parsing .thock/gmail.toml")?;
    document.remove("archive");
    Ok(document.to_string())
}

/// The zero-based row of the `[archive]` header, for the toast's link.
fn archive_table_row(text: &str) -> u32 {
    text.lines()
        .position(|line| line.trim() == "[archive]")
        .and_then(|row| u32::try_from(row).ok())
        .unwrap_or(0)
}

fn archive_state_path(vault: &Vault) -> PathBuf {
    vault
        .root
        .join(VAULT_MARKER_DIR)
        .join(STATE_DIR)
        .join(ARCHIVE_STATE_FILE)
}

/// The Inbox Routine's landing folder: only threads captured there go
/// through triage (V53 §3).
async fn inbox_dir(fs: &Arc<dyn Fs>, vault: &Vault) -> String {
    let path = vault.root.join(VAULT_MARKER_DIR).join(INBOX_CONFIG_FILE);
    fs.load(&path)
        .await
        .ok()
        .and_then(|text| parse_inbox_config(&text).ok())
        .map(|config| config.dir)
        .unwrap_or_else(|| crate::inbox::InboxConfig::default().dir)
}

fn state_file_path(vault: &Vault) -> PathBuf {
    vault
        .root
        .join(VAULT_MARKER_DIR)
        .join(STATE_DIR)
        .join(STATE_FILE)
}

fn hold_reason(missing_labels: &[&str]) -> Option<String> {
    match missing_labels {
        [] => None,
        [label] => Some(format!("label \"{label}\" not found in Gmail")),
        labels => Some(format!(
            "labels {} not found in Gmail",
            labels
                .iter()
                .map(|label| format!("\"{label}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// One pass over every mapped folder, the backlog, and the triage log: stems
/// and digest→stem for collision and repair handling, and the vault-side
/// dedup record (spec §4.4). Legacy `thread:` frontmatter counts alongside
/// `capture:` (spec §9). A missing folder is an empty state, not an error.
async fn scan_vault(fs: &Arc<dyn Fs>, vault: &Vault, config: &GmailConfig) -> VaultScan {
    use futures::StreamExt as _;
    let mut scan = VaultScan::default();
    for mapping in &config.mappings {
        let mut dir_scan = DirScan::default();
        let dir = vault.root.join(&mapping.path);
        if let Ok(mut entries) = fs.read_dir(&dir).await {
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
                dir_scan.stems.insert(stem.to_string());
                let Ok(content) = fs.load(&path).await else {
                    continue;
                };
                if let Some(digest) =
                    inbox_note_digest(&content).or_else(|| archive_frontmatter_digest(&content))
                {
                    scan.digests.insert(digest.clone());
                    dir_scan.digest_stems.insert(digest, stem.to_string());
                }
            }
        }
        scan.dirs.push(dir_scan);
    }
    if let Ok(text) = fs.load(&vault.backlog_path()).await {
        scan.backlog_markers = scan_backlog_markers(&text);
    }
    if let Ok(log) = fs.load(&vault.root.join(TRIAGE_LOG_PATH)).await {
        scan.digests.extend(scan_triage_log_markers(&log));
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmail::{GmailFetched, thread_marker_id};
    use crate::inbox::{CapturedItem, capture_digest};
    use chrono::{FixedOffset, TimeZone as _};
    use fs::FakeFs;
    use gpui::TestAppContext;
    use settings::SettingsStore;
    use std::path::Path;
    use std::sync::Mutex;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    fn email(thread_id: &str, title: &str, body: &str) -> CapturedItem {
        CapturedItem {
            source: "gmail",
            external_id: thread_id.to_string(),
            title: title.to_string(),
            from: Some("Ana <ana@example.com>".to_string()),
            url: None,
            link: Some(format!(
                "https://mail.google.com/mail/u/diego@example.com/#all/{thread_id}"
            )),
            body: Some(body.to_string()),
            occurred_at: Some(
                FixedOffset::west_opt(7 * 3600)
                    .unwrap()
                    .with_ymd_and_hms(2026, 8, 18, 9, 30, 0)
                    .unwrap(),
            ),
            due: None,
        }
    }

    fn test_config() -> GmailConfig {
        GmailConfig {
            account: Some("diego@example.com".to_string()),
            ..GmailConfig::default()
        }
    }

    fn test_vault() -> Vault {
        Vault {
            root: PathBuf::from("/vault"),
            config: crate::vault::VaultConfig::default(),
        }
    }

    #[gpui::test]
    async fn mapped_capture_lands_notes_and_the_backlog_line(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        let transport = Arc::new(StubTransport {
            items: Mutex::new(vec![
                vec![email("t-invoice", "Invoice #4821", "Pay up.")],
                vec![email("t-idea", "An idea from the road", "Two thoughts.")],
            ]),
            skips_seen: Mutex::new(Vec::new()),
            ..Default::default()
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(test_vault(), test_config(), transport.clone(), cx)
        });
        cx.run_until_parked();

        // The backlog mapping landed an archive note in the V13 note format…
        let digest = capture_digest("diego@example.com", "gmail", "t-invoice");
        let archive_path = Path::new("/vault/archives/emails/2026-08-18-0930-invoice-4821.md");
        let archive = fs.load(archive_path).await.unwrap();
        assert!(
            archive.contains(&format!("capture:  {digest}")),
            "{archive}"
        );
        assert!(
            archive.contains("from:     Ana <ana@example.com>"),
            "{archive}"
        );
        assert!(archive.contains("Pay up."), "{archive}");
        // …and its line in Someday, linked to the archive and marked.
        let backlog = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert!(backlog.starts_with("# Backlog\n"), "{backlog}");
        assert!(
            backlog.contains(&format!(
                "- [ ] Invoice #4821 [[2026-08-18-0930-invoice-4821]] <!--gmail:{digest}-->"
            )),
            "{backlog}"
        );
        // The inbox mapping landed a plain note and touched nothing else.
        let inbox_note = fs
            .load(Path::new(
                "/vault/inbox/2026-08-18-0930-an-idea-from-the-road.md",
            ))
            .await
            .unwrap();
        assert!(inbox_note.contains("Two thoughts."), "{inbox_note}");
        assert!(!backlog.contains("An idea from the road"), "{backlog}");

        // The state recorded both, with their mapping paths.
        let state = fs
            .load(Path::new("/vault/.thock/state/gmail/imported.jsonl"))
            .await
            .unwrap();
        assert_eq!(state.lines().count(), 2);
        assert!(state.contains("\"path\":\"archives/emails\""), "{state}");
        assert!(state.contains("\"path\":\"inbox\""), "{state}");
        service.read_with(cx, |service, _| {
            assert!(
                matches!(service.state(), SyncState::Synced { .. }),
                "unexpected state {:?}",
                service.state()
            );
        });

        // The next poll passes every digest to the transport and changes
        // nothing.
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new("/vault/backlog.md")).await.unwrap(),
            backlog
        );
        let skips = transport.skips_seen.lock().unwrap();
        assert!(skips.last().unwrap().contains(&digest));
    }

    #[gpui::test]
    async fn legacy_v9_records_and_state_loss_never_duplicate(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault/archives/emails"))
            .await
            .unwrap();
        // A V9-format archive with `thread:` frontmatter and its old-digest
        // backlog line.
        let legacy_digest = thread_marker_id("diego@example.com", "t-legacy");
        fs.insert_file(
            Path::new("/vault/archives/emails/2026-08-01-old-invoice.md"),
            format!("---\nsubject: Old invoice\nthread: {legacy_digest}\n---\n\n# Old invoice\n")
                .into_bytes(),
        )
        .await;
        fs.insert_file(
            Path::new("/vault/backlog.md"),
            format!(
                "# Backlog\n\n## Soon\n\n## Someday\n\n\
                 - [ ] Old invoice [[2026-08-01-old-invoice]] <!--gmail:{legacy_digest}-->\n\n\
                 ## Completed\n"
            )
            .into_bytes(),
        )
        .await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        let transport = Arc::new(StubTransport {
            items: Mutex::new(vec![vec![email("t-new", "Fresh thing", "body")], vec![]]),
            skips_seen: Mutex::new(Vec::new()),
            ..Default::default()
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(test_vault(), test_config(), transport.clone(), cx)
        });
        cx.run_until_parked();

        // The transport was told about the legacy digest (it is the one who
        // compares both constructions), and the fresh thread captured.
        {
            let skips = transport.skips_seen.lock().unwrap();
            assert!(skips.last().unwrap().contains(&legacy_digest));
        }
        let backlog = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert!(backlog.contains("- [ ] Fresh thing [["), "{backlog}");
        assert_eq!(backlog.matches("Old invoice [[").count(), 1, "{backlog}");

        // Crash story: the state file vanishes, the service restarts. The
        // notes and markers keep every capture from duplicating.
        fs.remove_file(
            Path::new("/vault/.thock/state/gmail/imported.jsonl"),
            Default::default(),
        )
        .await
        .unwrap();
        service.update(cx, |service, cx| {
            service.configure_for_test(test_vault(), test_config(), transport.clone(), cx)
        });
        cx.run_until_parked();
        let after = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert_eq!(backlog, after);
    }

    #[gpui::test]
    async fn crash_between_landing_and_append_is_repaired(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault/archives/emails"))
            .await
            .unwrap();
        // The landed note exists (V15 format), but neither the backlog line
        // nor the state made it — the crash window of spec §7.2.
        let digest = capture_digest("diego@example.com", "gmail", "t-invoice");
        fs.insert_file(
            Path::new("/vault/archives/emails/2026-08-18-0930-invoice-4821.md"),
            format!(
                "---\nsource:   gmail\ncapture:  {digest}\n\
                 captured: 2026-08-18T09:31:00-07:00\ntitle:    Invoice #4821\n---\n\n\
                 # Invoice #4821\n\nPay up.\n"
            )
            .into_bytes(),
        )
        .await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        let transport = Arc::new(StubTransport {
            // The stub keeps returning the thread (a real transport would
            // skip it), so this also exercises the planner's vault-digest
            // guard: repair, never a second file.
            items: Mutex::new(vec![
                vec![email("t-invoice", "Invoice #4821", "Pay up.")],
                vec![],
            ]),
            skips_seen: Mutex::new(Vec::new()),
            ..Default::default()
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(test_vault(), test_config(), transport.clone(), cx)
        });
        cx.run_until_parked();

        // One line appended, no second file, state repaired.
        let backlog = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert_eq!(
            backlog
                .matches(&format!(
                    "- [ ] Invoice #4821 [[2026-08-18-0930-invoice-4821]] <!--gmail:{digest}-->"
                ))
                .count(),
            1,
            "{backlog}"
        );
        let state = fs
            .load(Path::new("/vault/.thock/state/gmail/imported.jsonl"))
            .await
            .unwrap();
        assert!(state.contains(&digest), "{state}");

        // And it converges: another restart appends nothing new.
        service.update(cx, |service, cx| {
            service.configure_for_test(test_vault(), test_config(), transport.clone(), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new("/vault/backlog.md")).await.unwrap(),
            backlog
        );
    }

    #[gpui::test]
    async fn missing_labels_hold_with_their_names(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        struct HoldingTransport;
        impl MailTransport for HoldingTransport {
            fn modify_thread(&self, _: &str, _: ThreadMove, _cx: &AsyncApp) -> Task<Result<()>> {
                Task::ready(Ok(()))
            }

            fn fetch(&self, _: &HashSet<String>, _cx: &AsyncApp) -> Task<Result<GmailFetched>> {
                Task::ready(Ok(GmailFetched {
                    mappings: vec![
                        MappingFetched::LabelNotFound,
                        MappingFetched::Items(Vec::new()),
                    ],
                }))
            }
        }

        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        service.update(cx, |service, cx| {
            service.configure_for_test(test_vault(), test_config(), Arc::new(HoldingTransport), cx)
        });
        cx.run_until_parked();
        service.read_with(cx, |service, _| match service.state() {
            SyncState::Holding { reason } => {
                assert!(reason.summary().contains("thock/backlog"), "{reason}");
            }
            other => panic!("expected holding, got {other:?}"),
        });
        assert_eq!(hold_reason(&[]), None);
        assert_eq!(
            hold_reason(&["a", "b"]).unwrap(),
            "labels \"a\", \"b\" not found in Gmail"
        );
    }

    const ARCHIVE_DIGEST: &str = "4d1f9a02c7b3";

    /// A vault with one email captured into `inbox/`, the service on a
    /// pinned clock, and Gmail archive configured by `archive`.
    async fn archive_harness(
        archive: ArchiveConfig,
        cx: &mut TestAppContext,
    ) -> (
        Arc<FakeFs>,
        Entity<GmailService>,
        Arc<StubTransport>,
        Arc<Mutex<DateTime<Utc>>>,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault/.thock/state/gmail"))
            .await
            .unwrap();
        fs.create_dir(Path::new("/vault/archives/inbox"))
            .await
            .unwrap();
        fs.insert_file(
            Path::new("/vault/.thock/state/gmail/imported.jsonl"),
            format!(
                "{{\"digest\":\"{ARCHIVE_DIGEST}\",\"thread\":\"t-q4\",\"title\":\"Q4\",\
                 \"path\":\"inbox\",\"at\":\"2026-11-02T09:00:00Z\"}}\n"
            )
            .into_bytes(),
        )
        .await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let clock = Arc::new(Mutex::new(
            DateTime::parse_from_rfc3339("2026-11-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ));
        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        let transport = Arc::new(StubTransport {
            items: Mutex::new(vec![vec![], vec![]]),
            ..Default::default()
        });
        service.update(cx, |service, cx| {
            service.set_now_for_test(Arc::new({
                let clock = clock.clone();
                move || *clock.lock().unwrap()
            }));
            let config = GmailConfig {
                archive,
                ..test_config()
            };
            service.configure_for_test(test_vault(), config, transport.clone(), cx)
        });
        cx.run_until_parked();
        (fs, service, transport, clock)
    }

    fn advance(clock: &Mutex<DateTime<Utc>>, seconds: u64, cx: &mut TestAppContext) {
        *clock.lock().unwrap() += chrono::Duration::seconds(seconds as i64);
        cx.executor().advance_clock(Duration::from_secs(seconds));
        cx.run_until_parked();
    }

    async fn write_log(fs: &FakeFs, text: &str) {
        fs.insert_file(
            Path::new("/vault/archives/inbox/triage-log.md"),
            text.as_bytes().to_vec(),
        )
        .await;
    }

    fn archive_on() -> ArchiveConfig {
        ArchiveConfig::On([thock_sync_core::DecisionClass::Archive].into())
    }

    fn decided_line() -> String {
        format!("- 2026-11-02 · Q4 → Archived <!--inbox:{ARCHIVE_DIGEST}-->\n")
    }

    #[gpui::test]
    async fn a_desk_decision_archives_after_the_grace_and_undo_puts_it_back(
        cx: &mut TestAppContext,
    ) {
        let (fs, service, transport, clock) = archive_harness(archive_on(), cx).await;
        // Turning it on wrote the baseline record and sent nothing.
        assert!(transport.modified.lock().unwrap().is_empty());
        let state = fs
            .load(Path::new("/vault/.thock/state/gmail/archive.jsonl"))
            .await
            .unwrap();
        assert!(state.contains("\"event\":\"on\""), "{state}");

        // The desk files the email: one write, after the grace period.
        write_log(&fs, &decided_line()).await;
        service.update(cx, |service, cx| service.reconcile_soon(cx));
        cx.run_until_parked();
        assert!(transport.modified.lock().unwrap().is_empty());
        advance(&clock, 30, cx);
        assert!(transport.modified.lock().unwrap().is_empty());
        advance(&clock, 31, cx);
        assert_eq!(
            *transport.modified.lock().unwrap(),
            vec![("t-q4".to_string(), ThreadMove::Archive)]
        );
        let state = fs
            .load(Path::new("/vault/.thock/state/gmail/archive.jsonl"))
            .await
            .unwrap();
        assert!(state.contains("\"event\":\"archived\""), "{state}");
        service.read_with(cx, |service, _| {
            assert_eq!(service.archive_status(), ArchiveStatus::Healthy);
        });

        // A poll later, nothing is sent twice.
        advance(&clock, 301, cx);
        assert_eq!(transport.modified.lock().unwrap().len(), 1);

        // Desk `u` removes the line: the thread goes back to the inbox.
        write_log(&fs, "").await;
        service.update(cx, |service, cx| service.reconcile_soon(cx));
        cx.run_until_parked();
        assert_eq!(
            *transport.modified.lock().unwrap(),
            vec![
                ("t-q4".to_string(), ThreadMove::Archive),
                ("t-q4".to_string(), ThreadMove::Unarchive),
            ]
        );
        advance(&clock, 301, cx);
        assert_eq!(transport.modified.lock().unwrap().len(), 2);
    }

    #[gpui::test]
    async fn a_failing_write_shows_and_is_retried_on_the_next_poll(cx: &mut TestAppContext) {
        let (fs, service, transport, clock) = archive_harness(archive_on(), cx).await;
        *transport.modify_error.lock().unwrap() = Some("Gmail said 503".to_string());
        write_log(&fs, &decided_line()).await;
        service.update(cx, |service, cx| service.reconcile_soon(cx));
        cx.run_until_parked();
        advance(&clock, 61, cx);
        service.read_with(cx, |service, _| {
            assert_eq!(
                service.archive_status(),
                ArchiveStatus::Failing {
                    emails: 1,
                    error: "Gmail said 503".into(),
                }
            );
            let row = crate::sync_status::gmail_status(service).unwrap();
            assert_eq!(row.summary.as_ref(), "couldn't archive 1 email in Gmail");
        });

        *transport.modify_error.lock().unwrap() = None;
        advance(&clock, 301, cx);
        assert_eq!(
            *transport.modified.lock().unwrap(),
            vec![("t-q4".to_string(), ThreadMove::Archive)]
        );
        service.read_with(cx, |service, _| {
            assert_eq!(service.archive_status(), ArchiveStatus::Healthy);
        });
    }

    #[gpui::test]
    async fn a_missing_permission_holds_without_writing(cx: &mut TestAppContext) {
        let (fs, service, transport, clock) = archive_harness(archive_on(), cx).await;
        *transport.permission.lock().unwrap() = Some(false);
        write_log(&fs, &decided_line()).await;
        service.update(cx, |service, cx| service.reconcile_soon(cx));
        cx.run_until_parked();
        advance(&clock, 61, cx);
        advance(&clock, 301, cx);
        assert!(transport.modified.lock().unwrap().is_empty());
        service.read_with(cx, |service, _| {
            assert_eq!(service.archive_status(), ArchiveStatus::NeedsPermission);
        });
    }

    #[gpui::test]
    async fn no_archive_table_never_writes_to_gmail(cx: &mut TestAppContext) {
        let (fs, service, transport, clock) = archive_harness(ArchiveConfig::Off, cx).await;
        write_log(&fs, &decided_line()).await;
        service.update(cx, |service, cx| service.reconcile_soon(cx));
        advance(&clock, 61, cx);
        advance(&clock, 301, cx);
        assert!(transport.modified.lock().unwrap().is_empty());
        assert!(
            !fs.is_file(Path::new("/vault/.thock/state/gmail/archive.jsonl"))
                .await
        );
        service.read_with(cx, |service, _| {
            assert_eq!(service.archive_status(), ArchiveStatus::Off);
        });
    }

    #[test]
    fn the_archive_table_is_added_and_removed_in_place() {
        let original = "# My Gmail settings\nschema = 2\npoll_seconds = 120 # every two minutes\n\n\
                        [[sync]]\nlabel = \"thock/inbox\"\npath = \"inbox\"\n";
        let on = with_archive_table(original).unwrap();
        assert!(on.starts_with(original), "{on}");
        let config = parse_gmail_config(&on).unwrap();
        assert_eq!(config.archive, archive_on());
        assert_eq!(config.poll_interval, Duration::from_secs(120));
        assert_eq!(
            on.lines().nth(archive_table_row(&on) as usize),
            Some("[archive]")
        );
        assert!(on.contains("# Any of: today, soon, someday"), "{on}");

        // Already on with the user's own list: left exactly as it is.
        let custom = format!("{original}\n[archive]\non = [\"soon\", \"discard\"]\n");
        assert_eq!(with_archive_table(&custom).unwrap(), custom);
        // On but empty: given the default list, keeping the table.
        let empty = format!("{original}\n[archive]\non = []\n");
        assert_eq!(
            parse_gmail_config(&with_archive_table(&empty).unwrap())
                .unwrap()
                .archive,
            archive_on()
        );

        // Turning it off removes only the table.
        let off = without_archive_table(&custom).unwrap();
        assert_eq!(off.trim_end(), original.trim_end());
        assert_eq!(without_archive_table(original).unwrap(), original);
        // A file with nothing in it gets a schema too.
        let fresh = with_archive_table("").unwrap();
        assert!(fresh.starts_with("schema = 2\n"), "{fresh}");
        assert!(with_archive_table("not [toml").is_err());
    }
}
