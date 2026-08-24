//! The Gmail capture service (spec `v9-gmail-backlog-capture.md` §9–§10): one
//! GPUI entity per local project that polls for labeled threads, plans the
//! capture, and applies it in crash-safe order — archive files first, then
//! the backlog append (through the open buffer as one undoable transaction
//! when `backlog.md` is open), then the dedup state. It lives independently
//! of the Backlog panel; the panel only displays the status this service
//! exposes.

use anyhow::{Context as _, Result, anyhow};
use chrono::Local;
use fs::Fs;
use gpui::{
    App, AppContext as _, AsyncApp, Context, DismissEvent, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Global, Subscription, Task, WeakEntity, Window, actions,
};
use language::Buffer;
use picker::{Picker, PickerDelegate};
use project::Project;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ui::prelude::*;
use ui::{Icon, ListItem, ListItemSpacing};
use util::ResultExt as _;
use workspace::{ModalView, Workspace};

use crate::backlog::{DEFAULT_BACKLOG, apply_edits};
use crate::calendar_service::SyncState;
use crate::gmail::{
    GMAIL_CONFIG_FILE, GmailConfig, ImportMode, ImportRecord, MailFetched, MailProvider,
    archive_frontmatter_digest, parse_gmail_config, plan_capture,
};
use crate::gmail_google::GoogleMailProvider;
use crate::google_auth::{
    AuthRevoked, GOOGLE_CONFIG_FILE, GoogleClient, resolve_google_settings,
};
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

/// Same typing guard and backoff as the calendar service (V8 §9 guard 1).
const TYPING_GUARD_QUIET: Duration = Duration::from_secs(2);
const TYPING_GUARD_MAX_TRIES: usize = 15;
const BACKOFF_CEILING: Duration = Duration::from_secs(60 * 60);

const STATE_DIR: &str = "state/gmail";
const STATE_FILE: &str = "imported.jsonl";

actions!(
    thock,
    [
        /// Checks Gmail for newly labeled emails and captures them into the
        /// Backlog now.
        SyncGmailNow,
        /// Chooses whether emails you label are captured as a link to Gmail
        /// or archived into the vault.
        ChooseEmailImportMode,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
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
        workspace.register_action(|workspace, _: &ChooseEmailImportMode, window, cx| {
            let workspace_handle = workspace.weak_handle();
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| {
                    service.choose_import_mode(workspace_handle, window, cx)
                });
            }
        });
    })
    .detach();
}

#[derive(Default)]
struct GlobalGmailServices(HashMap<EntityId, Entity<GmailService>>);

impl Global for GlobalGmailServices {}

/// The capture service for `project`, if one is running.
pub fn service_for_project(project: &Entity<Project>, cx: &App) -> Option<Entity<GmailService>> {
    cx.try_global::<GlobalGmailServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

enum SyncOutcome {
    Synced,
    Held(gpui::SharedString),
    Failed(anyhow::Error),
    AuthRevoked,
    /// The service was reconfigured or released mid-sync.
    Aborted,
}

/// What the vault remembers about already-captured threads, loaded once per
/// provider start — the state file when it exists, rebuilt from backlog
/// markers and archive frontmatter when it doesn't (spec §4.3).
#[derive(Default, Clone)]
struct DedupState {
    imported: HashSet<String>,
    /// Archive filename stems on disk → owning thread digest, for the
    /// collision handling of spec §5.3.
    archive_stems: HashMap<String, String>,
}

pub struct GmailService {
    project: Entity<Project>,
    vault: Option<Vault>,
    config: Option<GmailConfig>,
    provider: Option<Arc<dyn MailProvider>>,
    state: SyncState,
    /// The one poll loop. Replacing it on reload cancels the old loop; the
    /// apply work it spawns is awaited inside it, never stored separately.
    poll_task: Option<Task<()>>,
    dedup: Option<DedupState>,
    _subscriptions: Vec<Subscription>,
}

impl GmailService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        let mut this = Self {
            project,
            vault: None,
            config: None,
            provider: None,
            state: SyncState::NoConfig,
            poll_task: None,
            dedup: None,
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
    /// carries a Gmail config (spec §10.3, G5).
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

    /// The workspace sign-out ran: stop polling and forget the provider. The
    /// vault — tasks, archives, state — is never touched.
    pub fn mark_signed_out(&mut self, cx: &mut Context<Self>) {
        self.poll_task = None;
        self.provider = None;
        if self.config.is_some() {
            self.state = SyncState::NeverConnected;
        }
        cx.notify();
    }

    /// Re-reads `.thock/gmail.toml` — the connect flow calls this after
    /// writing the account into it.
    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    /// `thock::SyncGmailNow`: restarts the loop, which checks immediately.
    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.provider.is_some() {
            self.start_poll(cx);
        }
    }

    /// `thock::ChooseEmailImportMode` (spec v9 §6.1): the two-option picker
    /// deciding whether captured emails link to Gmail or are archived into
    /// the vault. Also the connect flow's final step, so the choice is made
    /// deliberately instead of inherited from a default nobody knows exists.
    fn choose_import_mode(
        &mut self,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(vault) = &self.vault else {
            return;
        };
        let vault_root = vault.root.clone();
        let current = self
            .config
            .as_ref()
            .map(|config| config.import)
            .unwrap_or(ImportMode::Title);
        let fs = self.project.read(cx).fs().clone();
        let service = cx.weak_entity();
        // This runs from a workspace action handler (and from the calendar
        // picker's dismissal), so the workspace may be mid-update: defer the
        // modal.
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_modal(window, cx, |window, cx| {
                        let delegate = ImportModePickerDelegate {
                            picker_entity: cx.entity().downgrade(),
                            service,
                            fs,
                            vault_root,
                            selected_index: match current {
                                ImportMode::Title => 0,
                                ImportMode::Full => 1,
                            },
                            current,
                        };
                        ImportModePicker::new(delegate, window, cx)
                    });
                })
                .log_err();
        });
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

    /// Re-resolves the vault and `.thock/gmail.toml`, rebuilding the provider
    /// and poll loop when the configuration actually changed.
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
                    // A hand-edited file that doesn't parse disables capture,
                    // never panics (spec §7).
                    log::warn!(
                        "Thock: couldn't parse {}: {error:#}; email capture is off",
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
                self.provider = None;
                self.poll_task = None;
                self.state = SyncState::NeverConnected;
            }
            Some(config) => {
                let unchanged = self.config.as_ref() == Some(&config) && self.provider.is_some();
                if !unchanged {
                    self.config = Some(config);
                    // The account (and so every digest) may have changed:
                    // reload the dedup state from disk on the next poll.
                    self.dedup = None;
                    match self.build_provider(cx) {
                        Ok(provider) => {
                            self.provider = Some(provider);
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
        self.provider = None;
        self.poll_task = None;
        self.dedup = None;
        self.state = state;
    }

    fn build_provider(&self, cx: &App) -> Result<Arc<dyn MailProvider>> {
        let config = self.config.as_ref().context("no gmail config")?;
        let account = config.account.clone().context("no account connected")?;
        let client = GoogleClient::resolve(&config.google)?;
        Ok(Arc::new(GoogleMailProvider::new(
            cx.http_client(),
            client,
            account,
            config.label.clone(),
        )))
    }

    /// (Re)starts the poll loop: an immediate check, then one tick per
    /// `poll_seconds`, doubling up to an hour on transport errors (§10.2).
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
        let keep_going = match outcome {
            SyncOutcome::Aborted => false,
            SyncOutcome::Synced => {
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
                log::warn!("Thock email capture failed: {error:#}");
                self.state = SyncState::Failing {
                    error: format!("{error:#}").into(),
                };
                // Offline is just an error: back off, keep trying.
                *delay = (*delay * 2).min(BACKOFF_CEILING);
                true
            }
            SyncOutcome::AuthRevoked => {
                self.state = SyncState::Disconnected;
                self.provider = None;
                false
            }
        };
        cx.notify();
        keep_going
    }

    async fn sync_once(this: &WeakEntity<Self>, cx: &mut AsyncApp) -> SyncOutcome {
        let context = this
            .read_with(cx, |service, _| {
                match (&service.provider, &service.config, &service.vault) {
                    (Some(provider), Some(config), Some(vault)) => Some((
                        provider.clone(),
                        config.clone(),
                        vault.clone(),
                        service.project.clone(),
                    )),
                    _ => None,
                }
            })
            .ok()
            .flatten();
        let Some((provider, config, vault, project)) = context else {
            return SyncOutcome::Aborted;
        };
        let fs = project.read_with(cx, |project, _| project.fs().clone());

        let dedup = match Self::dedup_state(this, &fs, &vault, &config, cx).await {
            Ok(dedup) => dedup,
            Err(error) => return SyncOutcome::Failed(error),
        };

        let emails = match provider.fetch_labeled(config.import, &dedup.imported, cx).await {
            Err(error) if error.is::<AuthRevoked>() => return SyncOutcome::AuthRevoked,
            Err(error) => return SyncOutcome::Failed(error),
            Ok(MailFetched::LabelNotFound) => {
                return SyncOutcome::Held(
                    format!("label \"{}\" not found in Gmail", config.label).into(),
                );
            }
            Ok(MailFetched::Emails(emails)) => emails,
        };
        if emails.is_empty() {
            return SyncOutcome::Synced;
        }

        Self::apply_capture(this, &fs, &vault, &config, &project, &emails, dedup, cx).await
    }

    /// The loaded dedup state, loading (or rebuilding, spec §4.3) it first
    /// when this is the provider's first poll.
    async fn dedup_state(
        this: &WeakEntity<Self>,
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        config: &GmailConfig,
        cx: &mut AsyncApp,
    ) -> Result<DedupState> {
        if let Some(dedup) = this.read_with(cx, |service, _| service.dedup.clone())? {
            return Ok(dedup);
        }

        let mut dedup = DedupState::default();
        match fs.load(&state_file_path(vault)).await {
            Ok(contents) => {
                for line in contents.lines() {
                    if let Ok(record) = serde_json::from_str::<serde_json::Value>(line)
                        && let Some(digest) = record.get("digest").and_then(|value| value.as_str())
                    {
                        dedup.imported.insert(digest.to_string());
                    }
                }
            }
            Err(_) => {
                // No state file: the vault is the record. Backlog markers
                // rejoin `imported` lazily through the planner's marker guard;
                // only the archives need scanning here, both for state and
                // for the stem-collision map.
                if let Ok(text) = fs.load(&vault.backlog_path()).await {
                    dedup.imported.extend(crate::gmail::scan_backlog_markers(&text));
                }
            }
        }

        let archive_dir = vault.root.join(&config.archive_dir);
        if let Ok(mut entries) = fs.read_dir(&archive_dir).await {
            use futures::StreamExt as _;
            while let Some(entry) = entries.next().await {
                let Ok(path) = entry else { continue };
                if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                let Ok(content) = fs.load(&path).await else {
                    continue;
                };
                if let Some(digest) = archive_frontmatter_digest(&content) {
                    dedup.imported.insert(digest.clone());
                    dedup.archive_stems.insert(stem.to_string(), digest);
                }
            }
        }

        this.update(cx, |service, _| service.dedup = Some(dedup.clone()))?;
        Ok(dedup)
    }

    async fn apply_capture(
        this: &WeakEntity<Self>,
        fs: &Arc<dyn Fs>,
        vault: &Vault,
        config: &GmailConfig,
        project: &Entity<Project>,
        emails: &[crate::gmail::CapturedEmail],
        dedup: DedupState,
        cx: &mut AsyncApp,
    ) -> SyncOutcome {
        let backlog_path = vault.backlog_path();
        let buffer = project.update(cx, |project, cx| {
            project
                .project_path_for_absolute_path(&backlog_path, cx)
                .and_then(|path| project.get_open_buffer(&path, cx))
        });
        let backlog_text = match &buffer {
            Some(buffer) => buffer.read_with(cx, |buffer, _| buffer.text()),
            None => fs
                .load(&backlog_path)
                .await
                .unwrap_or_else(|_| DEFAULT_BACKLOG.to_string()),
        };

        let captured_at = Local::now().to_rfc3339();
        let plan = plan_capture(
            &backlog_text,
            emails,
            &dedup.imported,
            &dedup.archive_stems,
            config,
            &captured_at,
        );
        if plan.is_empty() {
            return SyncOutcome::Synced;
        }

        // Archives first (spec §9): a crash after this leaves harmless
        // orphans, and the planner reuses them by digest on the next poll.
        for archive in &plan.archives {
            let path = vault.root.join(&archive.rel_path);
            if fs.is_file(&path).await {
                continue;
            }
            let write = async {
                if let Some(parent) = path.parent() {
                    fs.create_dir(parent).await?;
                }
                fs.atomic_write(path.clone(), archive.content.clone()).await
            };
            if let Err(error) = write.await {
                return SyncOutcome::Failed(error);
            }
        }

        let outcome = match buffer {
            Some(buffer) => {
                Self::apply_via_buffer(buffer, emails, config, &dedup, &captured_at, cx).await
            }
            None => {
                Self::apply_via_fs(fs, &backlog_path, emails, config, &dedup, &captured_at).await
            }
        };
        let records = match outcome {
            Ok(records) => records,
            Err(error) => return SyncOutcome::Failed(error),
        };

        // State last (spec §9): a crash before this line re-plans next poll
        // and the marker guard turns it into a state repair, not a duplicate.
        if let Err(error) = Self::append_state(fs, vault, &records, &captured_at).await {
            return SyncOutcome::Failed(error);
        }
        this.update(cx, |service, _| {
            if let Some(dedup) = service.dedup.as_mut() {
                dedup
                    .imported
                    .extend(records.iter().map(|record| record.digest.clone()));
                for archive in &plan.archives {
                    dedup
                        .archive_stems
                        .insert(archive.stem.clone(), archive.digest.clone());
                }
            }
        })
        .ok();
        SyncOutcome::Synced
    }

    /// The not-open path (spec §9): read-modify-write through the project
    /// `Fs`, re-reading (and re-planning) after the fetch. A missing file is
    /// created from the scaffold — the backlog is a core scaffolded file, not
    /// a user gesture like the daily note (spec §13 #6).
    async fn apply_via_fs(
        fs: &Arc<dyn Fs>,
        backlog_path: &Path,
        emails: &[crate::gmail::CapturedEmail],
        config: &GmailConfig,
        dedup: &DedupState,
        captured_at: &str,
    ) -> Result<Vec<ImportRecord>> {
        let text = fs
            .load(backlog_path)
            .await
            .unwrap_or_else(|_| DEFAULT_BACKLOG.to_string());
        let plan = plan_capture(
            &text,
            emails,
            &dedup.imported,
            &dedup.archive_stems,
            config,
            captured_at,
        );
        if let Some(edit) = &plan.backlog_edit {
            fs.atomic_write(
                backlog_path.to_path_buf(),
                apply_edits(&text, vec![edit.clone()]),
            )
            .await?;
        }
        Ok(plan.newly_imported)
    }

    /// The open-buffer path (spec §9): waits for a quiet window (typing
    /// guard), then applies the append as a minimal diff in one finalized
    /// transaction — undoable with one `u`, and it cannot clobber unsaved
    /// keystrokes because it edits the live buffer.
    async fn apply_via_buffer(
        buffer: Entity<Buffer>,
        emails: &[crate::gmail::CapturedEmail],
        config: &GmailConfig,
        dedup: &DedupState,
        captured_at: &str,
        cx: &mut AsyncApp,
    ) -> Result<Vec<ImportRecord>> {
        for _ in 0..TYPING_GUARD_MAX_TRIES {
            let version = buffer.read_with(cx, |buffer, _| buffer.version());
            cx.background_executor().timer(TYPING_GUARD_QUIET).await;
            if buffer.read_with(cx, |buffer, _| buffer.version() == version) {
                break;
            }
        }

        // The buffer can change between computing the diff and applying it;
        // `apply_diff` refuses stale diffs, so just recompute. The planner is
        // deterministic, so a re-plan against fresh text picks the same
        // stems and lines.
        for _ in 0..3 {
            let text = buffer.read_with(cx, |buffer, _| buffer.text());
            let plan = plan_capture(
                &text,
                emails,
                &dedup.imported,
                &dedup.archive_stems,
                config,
                captured_at,
            );
            let Some(edit) = &plan.backlog_edit else {
                return Ok(plan.newly_imported);
            };
            let new_text = apply_edits(&text, vec![edit.clone()]);
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
                return Ok(plan.newly_imported);
            }
        }
        Err(anyhow!("the buffer kept changing while applying captured emails"))
    }

    /// Appends the captured threads to `.thock/state/gmail/imported.jsonl`
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
                "thread": record.thread_id,
                "subject": record.subject,
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
        config: GmailConfig,
        provider: Arc<dyn MailProvider>,
        cx: &mut Context<Self>,
    ) {
        self.vault = Some(vault);
        self.config = Some(config);
        self.provider = Some(provider);
        self.dedup = None;
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

pub struct ImportModePicker {
    picker: Entity<Picker<ImportModePickerDelegate>>,
}

impl ImportModePicker {
    fn new(delegate: ImportModePickerDelegate, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker = cx.new(|cx| Picker::nonsearchable_uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl ModalView for ImportModePicker {}
impl EventEmitter<DismissEvent> for ImportModePicker {}

impl Focusable for ImportModePicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for ImportModePicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ImportModePicker")
            .w(rems(34.))
            .child(self.picker.clone())
    }
}

/// The two capture styles, in `ImportMode` order.
const IMPORT_MODE_OPTIONS: [(ImportMode, &str, &str); 2] = [
    (
        ImportMode::Title,
        "Link to Gmail",
        "Captured tasks link back to the email in Gmail",
    ),
    (
        ImportMode::Full,
        "Archive into the vault",
        "The email's text is saved under archives/emails and linked from the task",
    ),
];

/// Enter picks a capture style and saves it to `.thock/gmail.toml`; escape
/// keeps the current one.
pub struct ImportModePickerDelegate {
    picker_entity: WeakEntity<ImportModePicker>,
    service: WeakEntity<GmailService>,
    fs: Arc<dyn Fs>,
    vault_root: PathBuf,
    selected_index: usize,
    current: ImportMode,
}

impl PickerDelegate for ImportModePickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "choose email import mode"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "How should labeled emails land in the Backlog?".into()
    }

    fn match_count(&self) -> usize {
        IMPORT_MODE_OPTIONS.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = index.min(IMPORT_MODE_OPTIONS.len() - 1);
    }

    fn update_matches(
        &mut self,
        _query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((mode, _, _)) = IMPORT_MODE_OPTIONS.get(self.selected_index) else {
            return;
        };
        let mode = *mode;
        if mode != self.current {
            let fs = self.fs.clone();
            let vault_root = self.vault_root.clone();
            let service = self.service.clone();
            cx.spawn(async move |_, cx| {
                crate::calendar_service::update_config_file(
                    &fs,
                    &vault_root,
                    GMAIL_CONFIG_FILE,
                    move |table| {
                        let value = match mode {
                            ImportMode::Title => "title",
                            ImportMode::Full => "full",
                        };
                        table.insert("import".into(), value.into());
                    },
                )
                .await?;
                service.update(cx, |service, cx| service.reload_config(cx))
            })
            .detach_and_log_err(cx);
        }
        self.picker_entity
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let (mode, title, description) = IMPORT_MODE_OPTIONS.get(index)?;
        let is_current = *mode == self.current;
        Some(
            ListItem::new(index)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .start_slot(
                    Icon::new(if is_current {
                        IconName::Check
                    } else {
                        IconName::Circle
                    })
                    .size(IconSize::Small)
                    .color(if is_current { Color::Accent } else { Color::Muted }),
                )
                .child(
                    v_flex()
                        .child(Label::new(*title))
                        .child(
                            Label::new(*description)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmail::{CapturedEmail, ImportMode, thread_marker_id};
    use chrono::TimeZone as _;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use settings::SettingsStore;
    use std::sync::Mutex;

    struct StubProvider {
        emails: Mutex<Vec<CapturedEmail>>,
        skips_seen: Mutex<Vec<HashSet<String>>>,
    }

    impl MailProvider for StubProvider {
        fn fetch_labeled(
            &self,
            _mode: ImportMode,
            skip: &HashSet<String>,
            _cx: &AsyncApp,
        ) -> Task<Result<MailFetched>> {
            self.skips_seen.lock().unwrap().push(skip.clone());
            let emails = self.emails.lock().unwrap().clone();
            Task::ready(Ok(MailFetched::Emails(emails)))
        }
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    fn email(thread_id: &str, subject: &str, body: Option<&str>) -> CapturedEmail {
        CapturedEmail {
            thread_id: thread_id.to_string(),
            subject: subject.to_string(),
            from: "Ana <ana@example.com>".to_string(),
            date: Local.with_ymd_and_hms(2026, 8, 18, 9, 30, 0).unwrap(),
            body: body.map(str::to_string),
        }
    }

    fn test_config(import: ImportMode) -> GmailConfig {
        GmailConfig {
            account: Some("diego@example.com".to_string()),
            import,
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
    async fn title_capture_creates_backlog_and_dedups(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        let provider = Arc::new(StubProvider {
            emails: Mutex::new(vec![
                email("t-invoice", "Re: Invoice #4821", None),
                email("t-offsite", "Offsite planning", None),
            ]),
            skips_seen: Mutex::new(Vec::new()),
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(
                test_vault(),
                test_config(ImportMode::Title),
                provider.clone(),
                cx,
            )
        });
        cx.run_until_parked();

        // backlog.md was created from the scaffold with both captures in
        // Someday, linked to Gmail and marked.
        let text = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        let digest = thread_marker_id("diego@example.com", "t-invoice");
        assert!(text.starts_with("# Backlog\n"), "{text}");
        assert!(
            text.contains(&format!(
                "- [ ] [Invoice #4821](https://mail.google.com/mail/u/diego@example.com/#all/t-invoice) <!--gmail:{digest}-->"
            )),
            "{text}"
        );
        assert!(text.contains("- [ ] [Offsite planning]("), "{text}");
        service.read_with(cx, |service, _| {
            assert!(
                matches!(service.state(), SyncState::Synced { .. }),
                "unexpected state {:?}",
                service.state()
            );
        });

        // The state file recorded both threads.
        let state = fs
            .load(Path::new("/vault/.thock/state/gmail/imported.jsonl"))
            .await
            .unwrap();
        assert_eq!(state.lines().count(), 2);

        // The next poll passes the digests to the provider and appends
        // nothing new.
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();
        let after = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert_eq!(text, after);
        let skips = provider.skips_seen.lock().unwrap();
        let last_skip = skips.last().unwrap();
        assert!(last_skip.contains(&digest));
    }

    #[gpui::test]
    async fn full_capture_archives_and_survives_state_loss(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| GmailService::new(project.clone(), cx));
        let provider = Arc::new(StubProvider {
            emails: Mutex::new(vec![email("t-invoice", "Invoice #4821", Some("Pay up."))]),
            skips_seen: Mutex::new(Vec::new()),
        });
        service.update(cx, |service, cx| {
            service.configure_for_test(
                test_vault(),
                test_config(ImportMode::Full),
                provider.clone(),
                cx,
            )
        });
        cx.run_until_parked();

        let archive_path = Path::new("/vault/archives/emails/2026-08-18-invoice-4821.md");
        let archive = fs.load(archive_path).await.unwrap();
        assert!(archive.contains("subject: Invoice #4821"), "{archive}");
        assert!(archive.contains("Pay up."), "{archive}");
        let backlog = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert!(
            backlog.contains("- [ ] Invoice #4821 [[2026-08-18-invoice-4821]]"),
            "{backlog}"
        );

        // Crash story: the state file vanishes, the service restarts (dedup
        // reloads from the vault). The archive and the marker keep the
        // capture from duplicating; the state file is repaired.
        fs.remove_file(
            Path::new("/vault/.thock/state/gmail/imported.jsonl"),
            Default::default(),
        )
        .await
        .unwrap();
        service.update(cx, |service, cx| {
            service.configure_for_test(
                test_vault(),
                test_config(ImportMode::Full),
                provider.clone(),
                cx,
            )
        });
        cx.run_until_parked();

        let backlog_after = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert_eq!(backlog, backlog_after);
        assert_eq!(fs.load(archive_path).await.unwrap(), archive);
        // The archive scan already knew the thread, so nothing new was
        // fetched into the state file — but a fresh thread still captures.
        provider
            .emails
            .lock()
            .unwrap()
            .push(email("t-new", "Another thing", Some("body")));
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();
        let backlog_final = fs.load(Path::new("/vault/backlog.md")).await.unwrap();
        assert!(backlog_final.contains("- [ ] Another thing [["), "{backlog_final}");
        assert_eq!(
            backlog_final.matches("Invoice #4821 [[").count(),
            1,
            "{backlog_final}"
        );
    }
}
