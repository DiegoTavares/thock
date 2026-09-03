//! The Inbox capture service (spec `v13-inbox-routine.md` §10.3): one GPUI
//! entity per local project that polls every enabled source, plans the
//! capture, and applies it in crash-safe order — note files first through the
//! project `Fs`, then the dedup state. There is no buffer path and no typing
//! guard: inbox notes are new files nobody has open. It lives independently
//! of the Backlog panel; the panel only displays the status and queue depth
//! this service exposes.

use anyhow::{Context as _, Result};
use chrono::Local;
use fs::Fs;
use gpui::TaskExt as _;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, Global, Subscription,
    Task, WeakEntity, actions,
};
use project::Project;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use workspace::Workspace;

use ui::IconName;

use crate::calendar_service::{ManualSyncFinished, SyncState, show_sync_toast};
use crate::google_auth::{AuthRevoked, GOOGLE_CONFIG_FILE, GoogleClient, resolve_google_settings};
use crate::inbox::{
    INBOX_CONFIG_FILE, ImportRecord, InboxConfig, InboxFetched, InboxSource, TRIAGE_LOG_PATH,
    inbox_note_digest, parse_inbox_config, plan_inbox_capture, scan_triage_log_markers,
};
use crate::tasks_google::GoogleTasksSource;
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

const BACKOFF_CEILING: Duration = Duration::from_secs(60 * 60);

const STATE_DIR: &str = "state/inbox";
const STATE_FILE: &str = "imported.jsonl";

actions!(
    thock,
    [
        /// Checks Google Tasks for newly captured items and lands them in
        /// the inbox folder now.
        SyncInboxNow,
        /// Shows the inbox folder — where captured items wait for triage —
        /// in the project panel.
        OpenInbox
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let service = cx.new(|cx| InboxService::new(project.clone(), cx));
        cx.subscribe(&service, |workspace, _, event: &ManualSyncFinished, cx| {
            show_sync_toast(workspace, event, cx);
        })
        .detach();
        let project_id = project.entity_id();
        cx.default_global::<GlobalInboxServices>()
            .0
            .insert(project_id, service);
        cx.on_release(move |_, cx| {
            cx.default_global::<GlobalInboxServices>()
                .0
                .remove(&project_id);
        })
        .detach();

        workspace.register_action(|workspace, _: &SyncInboxNow, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.sync_now(cx));
            }
        });
        workspace.register_action(|workspace, _: &OpenInbox, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.open_inbox(cx));
            }
        });
    })
    .detach();
}

#[derive(Default)]
struct GlobalInboxServices(HashMap<EntityId, Entity<InboxService>>);

impl Global for GlobalInboxServices {}

use std::collections::HashMap;

/// The capture service for `project`, if one is running.
pub fn service_for_project(project: &Entity<Project>, cx: &App) -> Option<Entity<InboxService>> {
    cx.try_global::<GlobalInboxServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

enum SyncOutcome {
    Synced {
        /// Items landed in the inbox by this poll, for the manual-sync toast.
        captured: usize,
    },
    Held(gpui::SharedString),
    Failed(anyhow::Error),
    AuthRevoked,
    /// The service was reconfigured or released mid-sync.
    Aborted,
}

/// What one poll learned from the vault before fetching: the landing zone's
/// current stems and digests (re-scanned every poll — triage moves files, so
/// a cached scan would go stale), plus the queue depth for the status row.
#[derive(Default)]
struct VaultScan {
    stems: HashSet<String>,
    digests: HashSet<String>,
    depth: usize,
}

pub struct InboxService {
    project: Entity<Project>,
    vault: Option<Vault>,
    config: Option<InboxConfig>,
    account: Option<String>,
    sources: Vec<Arc<dyn InboxSource>>,
    state: SyncState,
    /// The one poll loop. Replacing it on reload cancels the old loop; the
    /// apply work it spawns is awaited inside it, never stored separately.
    poll_task: Option<Task<()>>,
    /// Digests from `.thock/state/inbox/imported.jsonl`, loaded once per
    /// provider start. The vault-side record is re-scanned per poll instead.
    imported: Option<HashSet<String>>,
    /// `*.md` files currently waiting in the landing zone (spec §10.3).
    queue_depth: usize,
    /// Latest-wins depth refresh outside the poll (worktree events under the
    /// landing zone); cancel-by-replace is correct here because only the
    /// newest count matters.
    depth_task: Option<Task<()>>,
    /// Set by `sync_now` so the next completed sync announces itself
    /// ([`ManualSyncFinished`]); background polls stay quiet.
    announce_next_sync: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ManualSyncFinished> for InboxService {}

impl InboxService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        let mut this = Self {
            project,
            vault: None,
            config: None,
            account: None,
            sources: Vec::new(),
            state: SyncState::NoConfig,
            poll_task: None,
            imported: None,
            queue_depth: 0,
            depth_task: None,
            announce_next_sync: false,
            _subscriptions: vec![project_subscription],
        };
        this.reload(cx);
        this
    }

    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// Whether the status row should be shown at all: only when the vault
    /// carries `.thock/inbox.toml` (spec §10.4, G5).
    pub fn has_config(&self) -> bool {
        !matches!(self.state, SyncState::NoConfig)
    }

    /// How many items are waiting in the landing zone.
    pub fn queue_depth(&self) -> usize {
        self.queue_depth
    }

    /// The workspace connect flow started (it is owned by the calendar
    /// service): show progress here too.
    pub fn mark_connecting(&mut self, cx: &mut Context<Self>) {
        if self.has_config() {
            self.state = SyncState::Connecting;
            cx.notify();
        }
    }

    /// The workspace sign-out ran: stop polling and forget the sources. The
    /// vault — inbox notes, state, the triage log — is never touched.
    pub fn mark_signed_out(&mut self, cx: &mut Context<Self>) {
        self.poll_task = None;
        self.sources = Vec::new();
        if self.config.is_some() {
            self.state = SyncState::NeverConnected;
        }
        cx.notify();
    }

    /// Re-reads `.thock/inbox.toml` — the connect flow calls this after
    /// writing `google.toml`.
    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    /// `thock::SyncInboxNow`: restarts the loop, which checks immediately.
    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if !self.sources.is_empty() {
            self.announce_next_sync = true;
            self.start_poll(cx);
        }
    }

    /// `thock::OpenInbox`: reveals the landing zone in the project panel,
    /// creating the folder first if it's missing (an empty state, not an
    /// error). The fallback path for a vault whose Inbox Routine — and so
    /// the triage ritual — was removed (spec §10.4).
    fn open_inbox(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let dir = self
            .config
            .as_ref()
            .map(|config| config.dir.clone())
            .unwrap_or_else(|| InboxConfig::default().dir);
        let abs_dir = vault.root.join(&dir);
        let fs = self.project.read(cx).fs().clone();
        let project = self.project.clone();
        cx.spawn(async move |_, cx| {
            fs.create_dir(&abs_dir).await?;
            // The worktree may need a beat to index a freshly created dir;
            // reveal is best-effort either way.
            project.update(cx, |project, cx| {
                if let Some(entry_id) = project
                    .project_path_for_absolute_path(&abs_dir, cx)
                    .and_then(|path| project.entry_for_path(&path, cx))
                    .map(|entry| entry.id)
                {
                    cx.emit(project::Event::RevealInProjectPanel(entry_id));
                }
            });
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
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
                let inbox_config = format!("{VAULT_MARKER_DIR}/{INBOX_CONFIG_FILE}");
                let google_config = format!("{VAULT_MARKER_DIR}/{GOOGLE_CONFIG_FILE}");
                let vault_config = format!("{VAULT_MARKER_DIR}/{VAULT_CONFIG_FILE}");
                if changes.iter().any(|(path, _, _)| {
                    let path = path.as_unix_str();
                    path == inbox_config || path == google_config || path == vault_config
                }) {
                    self.reload(cx);
                    return;
                }
                let dir = self.config.as_ref().map(|config| config.dir.clone());
                if let Some(dir) = dir {
                    let prefix = format!("{dir}/");
                    if changes.iter().any(|(path, _, _)| {
                        let path = path.as_unix_str();
                        path == dir || path.starts_with(&prefix)
                    }) {
                        self.refresh_queue_depth(cx);
                    }
                }
            }
            _ => {}
        }
    }

    /// Recounts the landing zone outside the poll, so the status row tracks
    /// triage emptying the folder without waiting a poll interval.
    fn refresh_queue_depth(&mut self, cx: &mut Context<Self>) {
        let Some((vault, config)) = self.vault.as_ref().zip(self.config.as_ref()) else {
            return;
        };
        let dir = vault.root.join(&config.dir);
        let fs = self.project.read(cx).fs().clone();
        self.depth_task = Some(cx.spawn(async move |this, cx| {
            let depth = count_inbox_notes(&fs, &dir).await;
            this.update(cx, |service, cx| {
                if service.queue_depth != depth {
                    service.queue_depth = depth;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Re-resolves the vault, `.thock/inbox.toml`, and the connection
    /// settings, rebuilding the sources and poll loop when the configuration
    /// actually changed.
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

        let config_path = vault.root.join(VAULT_MARKER_DIR).join(INBOX_CONFIG_FILE);
        // Same synchronous read as `Vault::detect`; the file is tiny.
        let config = match std::fs::read_to_string(&config_path) {
            Err(_) => None,
            Ok(text) => match parse_inbox_config(&text) {
                Ok(config) => Some(config),
                Err(error) => {
                    // A hand-edited file that doesn't parse disables the
                    // network sources, never panics (spec §10.2).
                    log::warn!(
                        "Thock: couldn't parse {}: {error:#}; inbox capture is off",
                        config_path.display()
                    );
                    self.vault = Some(vault);
                    self.clear_sync(SyncState::Failing {
                        error: "inbox.toml could not be read".into(),
                    });
                    cx.notify();
                    return;
                }
            },
        };

        let account = resolve_google_settings(&vault.root, INBOX_CONFIG_FILE).account;
        self.vault = Some(vault);
        match config {
            None => self.clear_sync(SyncState::NoConfig),
            Some(config) if account.is_none() => {
                self.config = Some(config);
                self.account = None;
                self.sources = Vec::new();
                self.poll_task = None;
                self.state = SyncState::NeverConnected;
                self.refresh_queue_depth(cx);
            }
            Some(config) => {
                let unchanged = self.config.as_ref() == Some(&config)
                    && self.account == account
                    && !self.sources.is_empty();
                if !unchanged {
                    self.config = Some(config);
                    // The account (and so every digest) may have changed:
                    // reload the dedup state from disk on the next poll.
                    self.account = account;
                    self.imported = None;
                    match self.build_sources(cx) {
                        Ok(sources) => {
                            self.sources = sources;
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
                self.refresh_queue_depth(cx);
            }
        }
        cx.notify();
    }

    fn clear_sync(&mut self, state: SyncState) {
        self.config = None;
        self.account = None;
        self.sources = Vec::new();
        self.poll_task = None;
        self.imported = None;
        self.state = state;
    }

    /// Gmail left for the unified sync service's label map (spec v15);
    /// Google Tasks is the one transport still living here.
    fn build_sources(&self, cx: &App) -> Result<Vec<Arc<dyn InboxSource>>> {
        let vault = self.vault.as_ref().context("no vault")?;
        let config = self.config.as_ref().context("no inbox config")?;
        self.account.as_ref().context("no account connected")?;
        let settings = resolve_google_settings(&vault.root, INBOX_CONFIG_FILE);
        let client = GoogleClient::resolve(&settings.google)?;
        let mut sources: Vec<Arc<dyn InboxSource>> = Vec::new();
        if config.tasks_enabled {
            sources.push(Arc::new(GoogleTasksSource::new(
                cx.http_client(),
                client,
                config.tasks_list.clone(),
            )));
        }
        Ok(sources)
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
                    0 => "Inbox synced — nothing new".into(),
                    1 => "Inbox synced — 1 new item".into(),
                    n => format!("Inbox synced — {n} new items").into(),
                }),
                SyncOutcome::Held(reason) => Some(format!("Inbox sync held — {reason}").into()),
                SyncOutcome::Failed(error) => Some(format!("Inbox sync failed — {error:#}").into()),
                SyncOutcome::AuthRevoked => {
                    Some("Google sign-in expired — reconnect to sync the inbox".into())
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
                self.state = SyncState::Holding { reason };
                *delay = interval;
                true
            }
            SyncOutcome::Failed(error) => {
                log::warn!("Thock inbox capture failed: {error:#}");
                self.state = SyncState::Failing {
                    error: format!("{error:#}").into(),
                };
                // Offline is just an error: back off, keep trying.
                *delay = (*delay * 2).min(BACKOFF_CEILING);
                true
            }
            SyncOutcome::AuthRevoked => {
                self.state = SyncState::Disconnected;
                self.sources = Vec::new();
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
                match (&service.config, &service.vault, &service.account) {
                    (Some(config), Some(vault), Some(account)) if !service.sources.is_empty() => {
                        Some((
                            service.sources.clone(),
                            config.clone(),
                            vault.clone(),
                            account.clone(),
                            service.project.clone(),
                        ))
                    }
                    _ => None,
                }
            })
            .ok()
            .flatten();
        let Some((sources, config, vault, account, project)) = context else {
            return SyncOutcome::Aborted;
        };
        let fs = project.read_with(cx, |project, _| project.fs().clone());

        let imported = match Self::imported_state(this, &fs, &vault, cx).await {
            Ok(imported) => imported,
            Err(error) => return SyncOutcome::Failed(error),
        };
        // The vault-side record and the collision map, fresh every poll:
        // triage empties the folder between polls, and the scan doubles as
        // the queue-depth count.
        let scan = scan_vault(&fs, &vault, &config).await;
        this.update(cx, |service, cx| {
            if service.queue_depth != scan.depth {
                service.queue_depth = scan.depth;
                cx.notify();
            }
        })
        .ok();

        let mut skip = imported.clone();
        skip.extend(scan.digests.iter().cloned());

        let mut items = Vec::new();
        let mut holding: Option<String> = None;
        let mut failed: Option<anyhow::Error> = None;
        for source in &sources {
            // Errors isolate per source (spec §10.3): one failing transport
            // never blocks the other's capture.
            match source.fetch(&skip, cx).await {
                Err(error) if error.is::<AuthRevoked>() => return SyncOutcome::AuthRevoked,
                Err(error) => failed = failed.or(Some(error)),
                Ok(InboxFetched::Holding(reason)) => holding = holding.or(Some(reason)),
                Ok(InboxFetched::Items(fetched)) => items.extend(fetched),
            }
        }

        let mut captured = 0;
        if !items.is_empty() {
            let captured_at = Local::now().to_rfc3339();
            let plan = plan_inbox_capture(
                &items,
                &account,
                &imported,
                &scan.digests,
                &scan.stems,
                &config.dir,
                &captured_at,
            );
            if !plan.is_empty() {
                captured = plan.files.len();
                if let Err(error) =
                    Self::apply_plan(this, &fs, &vault, plan, &captured_at, cx).await
                {
                    return SyncOutcome::Failed(error);
                }
            }
        }

        match (failed, holding) {
            (Some(error), _) => SyncOutcome::Failed(error),
            (None, Some(reason)) => SyncOutcome::Held(reason.into()),
            (None, None) => SyncOutcome::Synced { captured },
        }
    }

    /// The state-file digests, loaded once per provider start (spec §4.3).
    /// No file costs nothing extra here — the per-poll vault scan is the
    /// rebuild, so deleting state costs a scan, not a duplicate flood.
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
        if let Ok(contents) = fs.load(&state_file_path(vault)).await {
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

    /// Applies a plan in crash-safe order (spec §10.3): note files first,
    /// create-if-missing, then the state append. A crash between the two
    /// leaves notes whose frontmatter repairs the state on the next poll.
    async fn apply_plan(
        this: &WeakEntity<Self>,
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        plan: crate::inbox::InboxPlan,
        captured_at: &str,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let mut written = 0usize;
        for file in &plan.files {
            let path = vault.root.join(&file.rel_path);
            if fs.is_file(&path).await {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs.create_dir(parent).await?;
            }
            fs.atomic_write(path, file.content.clone()).await?;
            written += 1;
        }
        Self::append_state(fs, vault, &plan.newly_imported, captured_at).await?;
        this.update(cx, |service, cx| {
            if let Some(imported) = service.imported.as_mut() {
                imported.extend(
                    plan.newly_imported
                        .iter()
                        .map(|record| record.digest.clone()),
                );
            }
            if written > 0 {
                service.queue_depth += written;
                cx.notify();
            }
        })
        .ok();
        Ok(())
    }

    /// Appends the captured items to `.thock/state/inbox/imported.jsonl`
    /// (spec §4.3).
    async fn append_state(
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        records: &[ImportRecord],
        captured_at: &str,
    ) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let path = state_file_path(vault);
        let mut contents = fs.load(&path).await.unwrap_or_default();
        for record in records {
            let entry = serde_json::json!({
                "digest": record.digest,
                "source": record.source,
                "id": record.external_id,
                "title": record.title,
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
    fn configure_for_test(
        &mut self,
        vault: Vault,
        config: InboxConfig,
        account: String,
        sources: Vec<Arc<dyn InboxSource>>,
        cx: &mut Context<Self>,
    ) {
        self.vault = Some(vault);
        self.config = Some(config);
        self.account = Some(account);
        self.sources = sources;
        self.imported = None;
        self.state = SyncState::Idle;
        self.start_poll(cx);
    }
}

fn state_file_path(vault: &Vault) -> PathBuf {
    vault
        .root
        .join(VAULT_MARKER_DIR)
        .join(STATE_DIR)
        .join(STATE_FILE)
}

/// `*.md` notes in the landing zone: dotfiles and other extensions never
/// count, and a missing folder is an empty state, not an error (VISION §4.6).
async fn inbox_note_paths(fs: &Arc<dyn Fs>, dir: &Path) -> Vec<PathBuf> {
    use futures::StreamExt as _;
    let mut paths = Vec::new();
    let Ok(mut entries) = fs.read_dir(dir).await else {
        return paths;
    };
    while let Some(entry) = entries.next().await {
        let Ok(path) = entry else { continue };
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| name.starts_with('.'))
        {
            continue;
        }
        paths.push(path);
    }
    paths
}

async fn count_inbox_notes(fs: &Arc<dyn Fs>, dir: &Path) -> usize {
    inbox_note_paths(fs, dir).await.len()
}

/// One pass over the landing zone and the triage log: stems for collision
/// handling, `capture:` digests plus log markers as the rebuildable record
/// (spec §4.3), and the queue depth.
async fn scan_vault(fs: &Arc<dyn Fs>, vault: &Vault, config: &InboxConfig) -> VaultScan {
    let mut scan = VaultScan::default();
    for path in inbox_note_paths(fs, &vault.root.join(&config.dir)).await {
        scan.depth += 1;
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            scan.stems.insert(stem.to_string());
        }
        if let Ok(content) = fs.load(&path).await
            && let Some(digest) = inbox_note_digest(&content)
        {
            scan.digests.insert(digest);
        }
    }
    if let Ok(log) = fs.load(&vault.root.join(TRIAGE_LOG_PATH)).await {
        scan.digests.extend(scan_triage_log_markers(&log));
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::{CapturedItem, capture_digest};
    use fs::FakeFs;
    use gpui::TestAppContext;
    use settings::SettingsStore;
    use std::sync::Mutex;

    struct StubSource {
        items: Mutex<Vec<CapturedItem>>,
        skips_seen: Mutex<Vec<HashSet<String>>>,
    }

    impl InboxSource for StubSource {
        fn id(&self) -> &'static str {
            "google-tasks"
        }

        fn fetch(&self, skip: &HashSet<String>, _cx: &AsyncApp) -> Task<Result<InboxFetched>> {
            self.skips_seen.lock().unwrap().push(skip.clone());
            let items = self.items.lock().unwrap().clone();
            Task::ready(Ok(InboxFetched::Items(items)))
        }
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    fn item(id: &str, title: &str) -> CapturedItem {
        CapturedItem {
            source: "google-tasks",
            external_id: id.to_string(),
            title: title.to_string(),
            from: None,
            url: Some("https://example.com/ship-it".to_string()),
            link: None,
            body: None,
            occurred_at: None,
            due: None,
        }
    }

    fn test_vault() -> Vault {
        Vault {
            root: PathBuf::from("/vault"),
            config: crate::vault::VaultConfig::default(),
        }
    }

    #[gpui::test]
    async fn capture_lands_notes_and_dedups_across_polls(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| InboxService::new(project.clone(), cx));
        let source = Arc::new(StubSource {
            items: Mutex::new(vec![item("task-1", "Ship it"), item("task-2", "Call back")]),
            skips_seen: Mutex::new(Vec::new()),
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(
                test_vault(),
                InboxConfig::default(),
                "diego@example.com".to_string(),
                vec![source.clone()],
                cx,
            )
        });
        cx.run_until_parked();

        // Two notes landed, frontmattered, and the state recorded both.
        let notes = inbox_note_paths(&(fs.clone() as Arc<dyn Fs>), Path::new("/vault/inbox")).await;
        assert_eq!(notes.len(), 2);
        let digest = capture_digest("diego@example.com", "google-tasks", "task-1");
        let ship = notes
            .iter()
            .find(|path| path.to_string_lossy().contains("ship-it"))
            .expect("ship-it note");
        let content = fs.load(ship).await.unwrap();
        assert!(
            content.contains(&format!("capture:  {digest}")),
            "{content}"
        );
        assert!(
            content.contains("url:      https://example.com/ship-it"),
            "{content}"
        );
        let state = fs
            .load(Path::new("/vault/.thock/state/inbox/imported.jsonl"))
            .await
            .unwrap();
        assert_eq!(state.lines().count(), 2);
        service.read_with(cx, |service, _| {
            assert!(matches!(service.state(), SyncState::Synced { .. }));
            assert_eq!(service.queue_depth(), 2);
        });

        // The next poll passes the digests to the source and writes nothing.
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();
        let notes = inbox_note_paths(&(fs.clone() as Arc<dyn Fs>), Path::new("/vault/inbox")).await;
        assert_eq!(notes.len(), 2);
        let skips = source.skips_seen.lock().unwrap();
        assert!(skips.last().unwrap().contains(&digest));
    }

    #[gpui::test]
    async fn only_a_manual_sync_announces_completion(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| InboxService::new(project.clone(), cx));
        let announcements = Arc::new(Mutex::new(Vec::new()));
        let _subscription = cx.update({
            let announcements = announcements.clone();
            |cx| {
                cx.subscribe(&service, move |_, event: &ManualSyncFinished, _| {
                    announcements
                        .lock()
                        .unwrap()
                        .push(event.message.to_string());
                })
            }
        });
        let source = Arc::new(StubSource {
            items: Mutex::new(vec![item("task-1", "Ship it")]),
            skips_seen: Mutex::new(Vec::new()),
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(
                test_vault(),
                InboxConfig::default(),
                "diego@example.com".to_string(),
                vec![source.clone()],
                cx,
            )
        });
        cx.run_until_parked();

        // The background poll captured an item but stayed quiet.
        assert_eq!(*announcements.lock().unwrap(), Vec::<String>::new());

        // A manual sync announces what it found.
        source
            .items
            .lock()
            .unwrap()
            .push(item("task-2", "Call back"));
        service.update(cx, |service, cx| service.sync_now(cx));
        cx.run_until_parked();
        assert_eq!(
            *announcements.lock().unwrap(),
            vec!["Inbox synced — 1 new item".to_string()]
        );

        // And only that one sync: the poll loop it restarted stays quiet.
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();
        assert_eq!(announcements.lock().unwrap().len(), 1);
    }

    #[gpui::test]
    async fn state_loss_is_repaired_from_frontmatter_not_duplicated(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| InboxService::new(project.clone(), cx));
        let source = Arc::new(StubSource {
            items: Mutex::new(vec![item("task-1", "Ship it")]),
            skips_seen: Mutex::new(Vec::new()),
        });
        let configure = |service: &Entity<InboxService>, cx: &mut TestAppContext| {
            service.update(cx, |service, cx| {
                service.configure_for_test(
                    test_vault(),
                    InboxConfig::default(),
                    "diego@example.com".to_string(),
                    vec![source.clone()],
                    cx,
                )
            });
        };
        configure(&service, cx);
        cx.run_until_parked();

        let notes = inbox_note_paths(&(fs.clone() as Arc<dyn Fs>), Path::new("/vault/inbox")).await;
        assert_eq!(notes.len(), 1);
        let note_content = fs.load(&notes[0]).await.unwrap();

        // Crash story (spec §4.3): the state file vanishes, the service
        // restarts. The note's frontmatter keeps the capture from
        // duplicating and the state is repaired.
        fs.remove_file(
            Path::new("/vault/.thock/state/inbox/imported.jsonl"),
            Default::default(),
        )
        .await
        .unwrap();
        configure(&service, cx);
        cx.run_until_parked();

        let notes = inbox_note_paths(&(fs.clone() as Arc<dyn Fs>), Path::new("/vault/inbox")).await;
        assert_eq!(notes.len(), 1);
        assert_eq!(fs.load(&notes[0]).await.unwrap(), note_content);
        let state = fs
            .load(Path::new("/vault/.thock/state/inbox/imported.jsonl"))
            .await
            .unwrap();
        assert_eq!(state.lines().count(), 1, "{state}");

        // A triaged item — file deleted, log line written — stays captured
        // through the log's marker even with the state gone again.
        let digest = capture_digest("diego@example.com", "google-tasks", "task-1");
        fs.remove_file(&notes[0], Default::default()).await.unwrap();
        fs.create_dir(Path::new("/vault/archives/inbox"))
            .await
            .unwrap();
        fs.insert_file(
            Path::new("/vault/archives/inbox/triage-log.md"),
            format!("- 2026-08-23 · Ship it → Backlog · Someday <!--inbox:{digest}-->\n")
                .into_bytes(),
        )
        .await;
        fs.remove_file(
            Path::new("/vault/.thock/state/inbox/imported.jsonl"),
            Default::default(),
        )
        .await
        .unwrap();
        configure(&service, cx);
        cx.run_until_parked();
        let notes = inbox_note_paths(&(fs.clone() as Arc<dyn Fs>), Path::new("/vault/inbox")).await;
        assert!(notes.is_empty(), "{notes:?}");

        // A fresh item still captures alongside all that history.
        source
            .items
            .lock()
            .unwrap()
            .push(item("task-9", "Another thing"));
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();
        let notes = inbox_note_paths(&(fs.clone() as Arc<dyn Fs>), Path::new("/vault/inbox")).await;
        assert_eq!(notes.len(), 1);
        assert!(notes[0].to_string_lossy().contains("another-thing"));
    }

    #[gpui::test]
    async fn holding_source_reports_without_blocking_the_other(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        struct HoldingSource;
        impl InboxSource for HoldingSource {
            fn id(&self) -> &'static str {
                "gmail"
            }
            fn fetch(&self, _: &HashSet<String>, _cx: &AsyncApp) -> Task<Result<InboxFetched>> {
                Task::ready(Ok(InboxFetched::Holding(
                    "label \"thock/inbox\" not found in Gmail".to_string(),
                )))
            }
        }

        let service = cx.new(|cx| InboxService::new(project.clone(), cx));
        let source = Arc::new(StubSource {
            items: Mutex::new(vec![item("task-1", "Ship it")]),
            skips_seen: Mutex::new(Vec::new()),
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(
                test_vault(),
                InboxConfig::default(),
                "diego@example.com".to_string(),
                vec![Arc::new(HoldingSource), source],
                cx,
            )
        });
        cx.run_until_parked();

        // The healthy source's capture landed…
        let notes = inbox_note_paths(&(fs.clone() as Arc<dyn Fs>), Path::new("/vault/inbox")).await;
        assert_eq!(notes.len(), 1);
        // …and the holding source's reason is what the row shows.
        service.read_with(cx, |service, _| match service.state() {
            SyncState::Holding { reason } => {
                assert!(reason.contains("thock/inbox"), "{reason}");
            }
            other => panic!("expected holding, got {other:?}"),
        });
    }
}
