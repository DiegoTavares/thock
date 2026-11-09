//! The Inbox capture service (spec `v13-inbox-routine.md` §10.3): one GPUI
//! entity per local project that polls every enabled source, plans the
//! capture, and applies it in crash-safe order — note files first through the
//! project `Fs`, then the dedup state. There is no buffer path and no typing
//! guard: inbox notes are new files nobody has open. It lives independently
//! of the Backlog panel; the panel only displays the status and queue depth
//! this service exposes.
//!
//! For the Inbox view (V41 §8.2) it also keeps the parsed queue, asks Jev for
//! a suggested destination per item while a view is open, and owns the
//! append-only records under `.thock/state/inbox/`: the suggestion cache and
//! the decision log.
//!
//! Once a day, when `[inbox] auto_archive` is on, it also drives the morning
//! run (V56 §7): after the first Gmail poll of the local day it asks Jev about
//! what arrived since the last run, files what clears the bar to Archive the
//! way `e` would, and keeps the run's record in `auto-archive.jsonl`, which
//! the view's Auto-archived group reads.

use anyhow::{Context as _, Result};
use chrono::{DateTime, Local, Utc};
use fs::{Fs, RenameOptions};
use futures::AsyncReadExt as _;
use gpui::TaskExt as _;
use gpui::{
    Action, App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, Global,
    SharedString, Subscription, Task, WeakEntity, Window, actions,
};
use http_client::{AsyncBody, HttpClient, Request, http};
use project::Project;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use util::ResultExt as _;
use workspace::Workspace;

use ui::IconName;

use crate::calendar_service::{ManualSyncFinished, SyncState, show_sync_toast};
use crate::google_auth::{AuthRevoked, GOOGLE_CONFIG_FILE, GoogleClient, resolve_google_settings};
use crate::inbox::{
    INBOX_CONFIG_FILE, ImportRecord, InboxConfig, InboxFetched, InboxItem, InboxSource,
    TRIAGE_LOG_PATH, inbox_note_digest, is_new, note_hash, parse_inbox_config, parse_inbox_note,
    parse_watermark, plan_inbox_capture, scan_triage_log_markers,
};
use crate::inbox_auto_archive::{
    AUTO_ARCHIVE_FILE, ArchivedItem, Due, GroupItem, RunEvent, RunLog, RunStatus, SkipReason,
    candidates, clears_bar, due, effective_threshold, group_items, person_wrote, policy_refreshed,
    run_id, without_auto_archive_keys,
};
use crate::inbox_email::parse_thread;
use crate::inbox_suggest::{
    Criteria, DECISIONS_ENDPOINT, DEFAULT_POLICY, DecisionRecord, DecisionReport, Destination,
    MAX_IN_FLIGHT, OPENROUTER_PROVIDER, Suggestion, SuggestionRecord, VIA_AUTO, build_request,
    flatten_policy, month_cost, parse_criteria, parse_response, policy_hash,
};
use crate::plus;
use crate::tasks_google::GoogleTasksSource;
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

const BACKOFF_CEILING: Duration = Duration::from_secs(60 * 60);

const STATE_DIR: &str = "state/inbox";
const STATE_FILE: &str = "imported.jsonl";
/// The ritual's watermark (V13 §4.4); the view reads it and never writes it.
const LAST_TRIAGE_FILE: &str = "last-triage";
const SUGGESTIONS_FILE: &str = "suggestions.jsonl";
const DECISIONS_FILE: &str = "decisions.jsonl";
/// The editable judgment both the ritual and the suggestion follow.
pub(crate) const POLICY_PATH: &str = "routines/inbox/triage-policy.md";
const REFRESH_POLICY_SKILL: &str = "refresh-inbox-policy";

actions!(
    thock,
    [
        /// Checks Google Tasks for newly captured items and lands them in
        /// the inbox folder now.
        SyncInboxNow,
        /// Shows the inbox folder — where captured items wait — in the
        /// project panel.
        RevealInboxFolder,
        /// Runs the Triage Inbox ritual on what's waiting in the inbox.
        TriageInbox,
        /// Archives what the inbox is sure needs nothing from you, now,
        /// instead of after the morning's first sync, or picks up a run that
        /// stopped.
        RunInboxAutoArchive,
        /// Lets the inbox archive, once a day, what is sure to need nothing
        /// from you. Refreshes the triage policy from your notes first.
        TurnOnInboxAutoArchive,
        /// Stops the inbox archiving things on its own. The policy and what
        /// it learned stay.
        TurnOffInboxAutoArchive,
        /// Rewrites the triage policy and the map of your work from your
        /// notes and your inbox decisions.
        RefreshInboxPolicy
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
        // Gmail's service is created first (see `thock::init`), so its polls
        // can start the morning run.
        if let Some(gmail) = crate::gmail_service::service_for_project(&project, cx) {
            service.update(cx, |service, cx| service.watch_gmail(&gmail, cx));
        }
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
        workspace.register_action(|workspace, _: &RevealInboxFolder, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.reveal_inbox_folder(cx));
            }
        });
        workspace.register_action(|workspace, _: &TriageInbox, window, cx| {
            let vault_root = service_for_project(workspace.project(), cx)
                .and_then(|service| service.read(cx).vault_root());
            dispatch_triage(vault_root, window, cx);
        });
        workspace.register_action(|workspace, _: &RunInboxAutoArchive, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.run_auto_archive_now(cx));
            }
        });
        workspace.register_action(|_, _: &TurnOnInboxAutoArchive, window, cx| {
            run_refresh_skill(Some("enable"), window, cx);
        });
        workspace.register_action(|_, _: &RefreshInboxPolicy, window, cx| {
            run_refresh_skill(None, window, cx);
        });
        workspace.register_action(|workspace, _: &TurnOffInboxAutoArchive, _window, cx| {
            let Some(service) = service_for_project(workspace.project(), cx) else {
                return;
            };
            let workspace = workspace.weak_handle();
            service.update(cx, |service, cx| {
                service.turn_off_auto_archive(workspace, cx)
            });
        });
    })
    .detach();
}

/// The refresh runs through `thock::RunSkill` like any Routine skill, so a
/// vault without the Inbox Routine gets the usual "no enabled skill" note.
fn run_refresh_skill(mode: Option<&str>, window: &mut Window, cx: &mut App) {
    window.dispatch_action(
        crate::agent_panel::RunSkill {
            skill: Some(REFRESH_POLICY_SKILL.to_string()),
            mode: mode.map(str::to_string),
        }
        .boxed_clone(),
        cx,
    );
}

/// `thock::TriageInbox` runs the Triage Inbox ritual (the generic
/// `thock::RunSkill`, per the repo's rule about dynamic content). A vault
/// whose Inbox Routine was removed must not get a dead row (V13 §10.4), so
/// with no `triage-inbox` skill registered this falls back to the Inbox view
/// (V41 §10), where the four quick decisions still work.
fn dispatch_triage(vault_root: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let has_triage_skill = vault_root
        .and_then(|root| match crate::vault::Vault::detect(&root) {
            crate::vault::VaultStatus::Valid(vault) => Some(vault),
            _ => None,
        })
        .is_some_and(|vault| {
            crate::routines::enabled_routine_manifests(&vault)
                .iter()
                .flat_map(|manifest| &manifest.skills)
                .any(|skill| skill.id == "triage-inbox")
        });
    if has_triage_skill {
        window.dispatch_action(
            crate::agent_panel::RunSkill {
                skill: Some("triage-inbox".to_string()),
                mode: None,
            }
            .boxed_clone(),
            cx,
        );
    } else {
        window.dispatch_action(crate::inbox_view::OpenInbox.boxed_clone(), cx);
    }
}

#[derive(Default)]
struct GlobalInboxServices(HashMap<EntityId, Entity<InboxService>>);

impl Global for GlobalInboxServices {}

/// A service for `project`, registered the way `init` would, without a
/// workspace.
#[cfg(test)]
pub(crate) fn new_for_test(project: &Entity<Project>, cx: &mut App) -> Entity<InboxService> {
    let service = cx.new(|cx| InboxService::new(project.clone(), cx));
    cx.default_global::<GlobalInboxServices>()
        .0
        .insert(project.entity_id(), service.clone());
    service
}

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
/// a cached scan would go stale).
#[derive(Default)]
struct VaultScan {
    stems: HashSet<String>,
    digests: HashSet<String>,
}

/// What the service tells the Inbox view (V41 §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxEvent {
    /// The queue was re-read: an item landed, left, or changed.
    ItemsChanged,
    /// A suggestion arrived, failed, or the requests paused.
    SuggestionsChanged,
}

/// One waiting item with what the view needs beside the parsed note.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxEntry {
    pub item: InboxItem,
    /// The sha256 of the note's bytes, the suggestion cache's key.
    pub hash: String,
    /// New since the last triage, by the ritual's own rule.
    pub is_new: bool,
    /// The `captured:` time, else the file's: what the list sorts on and
    /// the morning run's watermark is compared with.
    pub arrived: Option<DateTime<Utc>>,
}

/// An item of the view's Auto-archived group (V56 §8.1): the archived note,
/// read like an inbox row, with what the run said about it.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoArchivedEntry {
    pub entry: InboxEntry,
    pub run: String,
    pub confidence: f64,
    /// The `Archived · auto` line the run wrote to the triage log.
    pub line: String,
}

/// The policy as Jev is given it (V56 §4.4, §7.5).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPolicy {
    pub rows: Vec<String>,
    pub criteria: Criteria,
    pub hash: String,
    /// The refresh skill's `refreshed:` stamp is in the front matter.
    pub stamped: bool,
}

impl LoadedPolicy {
    /// The rows `triage-policy.md` flattens to, or the shipped defaults when
    /// it is missing or has no table; the criteria per key the same way.
    pub fn from_text(text: Option<&str>) -> Self {
        let text = text.unwrap_or_default();
        let mut rows = flatten_policy(text);
        if rows.is_empty() {
            rows = DEFAULT_POLICY.iter().map(|row| row.to_string()).collect();
        }
        let criteria = parse_criteria(text);
        Self {
            hash: policy_hash(&rows, &criteria),
            rows,
            criteria,
            stamped: policy_refreshed(text).is_some(),
        }
    }
}

/// Whether the view may ask for suggestions, and if not, why (V41 §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionAvailability {
    On,
    /// `[inbox] suggestions = false` in the vault's config.
    OffByConfig,
    /// No Thock Plus grant, or a grant whose provider isn't OpenRouter.
    NoGrant,
    /// The allowance is spent for this cycle.
    Exhausted,
}

/// How a request for one item ended.
enum AskOutcome {
    Answered(Suggestion),
    /// A 402: the allowance is spent, so no other request is worth making.
    Paused(SharedString),
    /// Anything else, per item.
    Failed(SharedString),
}

/// The suggestion side of the service, loaded from disk once per start.
#[derive(Default)]
struct Suggestions {
    loaded: bool,
    loading: Option<Task<()>>,
    /// By note hash: the answer and the policy hash it was asked under.
    cached: HashMap<String, (Suggestion, Option<String>)>,
    /// The cache file's text, rewritten whole on each append so concurrent
    /// answers can't drop each other's lines.
    cache_text: String,
    decisions_text: String,
    in_flight: HashMap<String, Task<()>>,
    failed: HashMap<String, SharedString>,
    paused: Option<SharedString>,
    session_cost: f64,
    /// State files to write, in order; one writer drains the queue so a
    /// later snapshot never loses to an earlier one.
    write_queue: std::collections::VecDeque<(&'static str, String)>,
    writing: bool,
    /// Decisions made before the log finished loading; appended once it
    /// has, so the file is never rewritten from a partial copy.
    pending_decisions: Vec<DecisionRecord>,
}

/// The morning run's side of the service (V56 §7), loaded with the cache.
#[derive(Default)]
struct AutoArchive {
    /// `auto-archive.jsonl` as last written; the in-memory text is the
    /// source of truth, like the cache's.
    log_text: String,
    log: RunLog,
    /// A poll or a scan asked for the run; it starts once what it reads is
    /// loaded, if it is due.
    triggered: bool,
    /// `thock::RunInboxAutoArchive` asked, whatever the day.
    manual: bool,
    running: Option<Task<()>>,
    /// Note hashes the running job asks Jev about itself, so the view's
    /// suggestions don't ask a second time.
    asking: HashSet<String>,
    /// The last run's filings nobody corrected, whose notes are still in
    /// the archive, in the order they were filed.
    group: Vec<AutoArchivedEntry>,
    /// Latest-wins, like the queue's re-read.
    group_task: Option<Task<()>>,
    /// The threshold was clamped and that was logged, once per service.
    clamp_logged: bool,
}

/// What moving an inbox note into the archive did (V40 §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved,
    /// Nothing at the source: the ritual or the phone got there first.
    SourceGone,
    /// A file at both: nothing changes, never an overwrite.
    DestinationExists,
}

/// Renames `from` to `to` through the project `Fs` under V40 §6.1's rules:
/// parents created, never over an existing file, a missing source is a
/// no-op. The desk's `move_file` applier and the Inbox view share it.
pub(crate) async fn move_note(fs: &Arc<dyn Fs>, from: &Path, to: &Path) -> Result<MoveOutcome> {
    if !fs.is_file(from).await {
        return Ok(MoveOutcome::SourceGone);
    }
    if fs.is_file(to).await {
        return Ok(MoveOutcome::DestinationExists);
    }
    fs.rename(
        from,
        to,
        RenameOptions {
            overwrite: false,
            ignore_if_exists: false,
            create_parents: true,
        },
    )
    .await?;
    Ok(MoveOutcome::Moved)
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
    /// The waiting items, newest first (V41 §8.2).
    items: Vec<InboxEntry>,
    /// Latest-wins re-read of the landing zone; cancel-by-replace is correct
    /// because only the newest listing matters.
    items_task: Option<Task<()>>,
    /// The queue has been read at least once since the vault loaded.
    items_loaded: bool,
    /// Inbox views currently open; suggestions are asked only while one is.
    open_views: usize,
    suggestions: Suggestions,
    /// `triage-policy.md` as Jev reads it; `None` until it has been read.
    policy: Option<LoadedPolicy>,
    policy_task: Option<Task<()>>,
    auto_archive: AutoArchive,
    gmail: Option<WeakEntity<crate::gmail_service::GmailService>>,
    #[cfg(test)]
    gmail_polling_for_test: Option<bool>,
    /// Set by `sync_now` so the next completed sync announces itself
    /// ([`ManualSyncFinished`]); background polls stay quiet.
    announce_next_sync: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ManualSyncFinished> for InboxService {}
impl EventEmitter<InboxEvent> for InboxService {}

impl InboxService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        // A grant arriving after the view opened (the keychain read is
        // asynchronous at startup) starts the requests.
        let entitlement_subscription =
            cx.observe_global::<plus::CurrentEntitlement>(|this, cx| this.pump_suggestions(cx));
        let mut this = Self {
            project,
            vault: None,
            config: None,
            account: None,
            sources: Vec::new(),
            state: SyncState::NoConfig,
            poll_task: None,
            imported: None,
            items: Vec::new(),
            items_task: None,
            items_loaded: false,
            open_views: 0,
            suggestions: Suggestions::default(),
            policy: None,
            policy_task: None,
            auto_archive: AutoArchive::default(),
            gmail: None,
            #[cfg(test)]
            gmail_polling_for_test: None,
            announce_next_sync: false,
            _subscriptions: vec![project_subscription, entitlement_subscription],
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
        self.items.len()
    }

    /// The waiting items, newest first.
    pub fn items(&self) -> &[InboxEntry] {
        &self.items
    }

    pub fn vault(&self) -> Option<&Vault> {
        self.vault.as_ref()
    }

    pub fn vault_root(&self) -> Option<PathBuf> {
        self.vault.as_ref().map(|vault| vault.root.clone())
    }

    /// The vault-relative landing folder: `.thock/inbox.toml`'s `dir`, or the
    /// default when the file is missing (the folder still works as one).
    pub fn inbox_dir(&self) -> String {
        self.config
            .as_ref()
            .map(|config| config.dir.clone())
            .unwrap_or_else(|| InboxConfig::default().dir)
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

    /// `thock::RevealInboxFolder`: reveals the landing zone in the project
    /// panel, creating the folder first if it's missing (an empty state, not
    /// an error).
    fn reveal_inbox_folder(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let abs_dir = vault.root.join(self.inbox_dir());
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
                let dir = self.inbox_dir();
                let prefix = format!("{dir}/");
                let watermark = format!("{VAULT_MARKER_DIR}/{STATE_DIR}/{LAST_TRIAGE_FILE}");
                if changes.iter().any(|(path, _, _)| {
                    let path = path.as_unix_str();
                    path == dir || path.starts_with(&prefix) || path == watermark
                }) {
                    self.refresh_items(cx);
                }
                if changes
                    .iter()
                    .any(|(path, _, _)| path.as_unix_str() == POLICY_PATH)
                {
                    self.load_policy(cx);
                }
                let archive_prefix = format!("{}/", thock_sync_core::INBOX_ARCHIVE_DIR);
                if changes
                    .iter()
                    .any(|(path, _, _)| path.as_unix_str().starts_with(&archive_prefix))
                {
                    self.refresh_group(cx);
                }
            }
            _ => {}
        }
    }

    /// Re-reads the landing zone outside the poll, so the view and the
    /// status row track the folder without waiting a poll interval.
    fn refresh_items(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            if !self.items.is_empty() {
                self.items.clear();
                cx.emit(InboxEvent::ItemsChanged);
                cx.notify();
            }
            return;
        };
        let dir = self.inbox_dir();
        let fs = self.project.read(cx).fs().clone();
        self.items_task = Some(cx.spawn(async move |this, cx| {
            let entries = load_items(&fs, &vault, &dir).await;
            this.update(cx, |service, cx| {
                if service.items != entries {
                    service.items = entries;
                    cx.emit(InboxEvent::ItemsChanged);
                    cx.notify();
                }
                service.items_loaded = true;
                // Without Gmail polling, the day's first scan of the queue
                // is what starts the morning run (§7.3).
                if !service.gmail_polling(cx) {
                    service.auto_archive.triggered = true;
                }
                service.try_auto_archive(cx);
                service.pump_suggestions(cx);
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
            self.refresh_items(cx);
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
                    self.refresh_items(cx);
                    cx.notify();
                    return;
                }
            },
        };

        let account = resolve_google_settings(&vault.root, INBOX_CONFIG_FILE).account;
        if self.vault.as_ref() != Some(&vault) {
            // A different vault has its own cache and log.
            self.suggestions = Suggestions::default();
            self.auto_archive = AutoArchive::default();
            self.policy = None;
            self.items_loaded = false;
        }
        self.vault = Some(vault);
        self.load_policy(cx);
        self.note_auto_archive_setting(cx);
        // The cache and decision log feed the sync popover's Inbox row and
        // every decision the view records, grant or not, so they load as
        // soon as there is a vault to read them from.
        self.load_suggestion_state(cx);
        match config {
            None => self.clear_sync(SyncState::NoConfig),
            Some(config) if account.is_none() => {
                self.config = Some(config);
                self.account = None;
                self.sources = Vec::new();
                self.poll_task = None;
                self.state = SyncState::NeverConnected;
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
            }
        }
        self.refresh_items(cx);
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
                self.state = SyncState::Holding {
                    reason: reason.into(),
                };
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
        // triage empties the folder between polls.
        let scan = scan_vault(&fs, &vault, &config).await;
        this.update(cx, |service, cx| service.refresh_items(cx))
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
        if let Ok(contents) = fs.load(&state_path(vault, STATE_FILE)).await {
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
                service.refresh_items(cx);
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
        let path = state_path(vault, STATE_FILE);
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
        write_state(fs, path, contents).await
    }

    // ----- Suggestions (V41 §7) -----

    /// An Inbox view opened: suggestions are asked while at least one is.
    pub fn view_opened(&mut self, cx: &mut Context<Self>) {
        self.open_views += 1;
        self.load_suggestion_state(cx);
        self.pump_suggestions(cx);
    }

    pub fn view_closed(&mut self, cx: &mut Context<Self>) {
        self.open_views = self.open_views.saturating_sub(1);
        if self.open_views == 0 {
            // Answers already in flight land and are cached; nothing new is
            // asked for items nobody is looking at (decision 3).
            cx.notify();
        }
    }

    pub fn suggestion_availability(&self, cx: &App) -> SuggestionAvailability {
        if self
            .vault
            .as_ref()
            .is_some_and(|vault| !vault.config.inbox.suggestions)
        {
            return SuggestionAvailability::OffByConfig;
        }
        match plus::current_entitlement(cx) {
            None => SuggestionAvailability::NoGrant,
            Some(entitlement) if entitlement.is_exhausted() => SuggestionAvailability::Exhausted,
            Some(entitlement) => match &entitlement.gateway {
                Some(gateway) if gateway.provider == OPENROUTER_PROVIDER => {
                    SuggestionAvailability::On
                }
                _ => SuggestionAvailability::NoGrant,
            },
        }
    }

    fn gateway_key(&self, cx: &App) -> Option<String> {
        if self.suggestion_availability(cx) != SuggestionAvailability::On {
            return None;
        }
        plus::current_entitlement(cx)
            .and_then(|entitlement| entitlement.gateway.as_ref())
            .map(|gateway| gateway.api_key.clone())
    }

    /// The key the morning run asks with. `[inbox] suggestions = false`
    /// hides the view's chips but doesn't stop a run the person turned on
    /// (§5.2); a spent allowance or another provider does.
    fn auto_archive_key(cx: &App) -> Option<String> {
        let entitlement = plus::current_entitlement(cx)?;
        if entitlement.is_exhausted() {
            return None;
        }
        entitlement
            .gateway
            .as_ref()
            .filter(|gateway| gateway.provider == OPENROUTER_PROVIDER)
            .map(|gateway| gateway.api_key.clone())
    }

    /// The cached answer for a note's bytes under the current policy,
    /// whatever its tier. An answer asked under another policy is stale
    /// (V56 §7.5).
    pub fn suggestion_for(&self, hash: &str) -> Option<&Suggestion> {
        let policy = &self.policy.as_ref()?.hash;
        self.suggestions
            .cached
            .get(hash)
            .filter(|(_, asked_under)| asked_under.as_ref() == Some(policy))
            .map(|(suggestion, _)| suggestion)
    }

    /// The last answer recorded for a note's bytes under any policy: what
    /// the Auto-archived group shows the run decided on.
    pub fn recorded_suggestion_for(&self, hash: &str) -> Option<&Suggestion> {
        self.suggestions
            .cached
            .get(hash)
            .map(|(suggestion, _)| suggestion)
    }

    /// Why an item has no answer, for the chip's tooltip.
    pub fn suggestion_failure(&self, hash: &str) -> Option<&SharedString> {
        self.suggestions.failed.get(hash)
    }

    pub fn is_asking(&self, hash: &str) -> bool {
        self.suggestions.in_flight.contains_key(hash)
    }

    /// Set while the allowance is spent; the header shows one line.
    pub fn suggestions_paused(&self) -> Option<&SharedString> {
        self.suggestions.paused.as_ref()
    }

    pub fn any_suggestion_failed(&self) -> bool {
        !self.suggestions.failed.is_empty()
    }

    /// USD spent on suggestions since the app started.
    pub fn session_cost(&self) -> f64 {
        self.suggestions.session_cost
    }

    /// USD the cache file records for the current month.
    pub fn month_cost(&self) -> f64 {
        month_cost(
            &self.suggestions.cache_text,
            &Local::now().format("%Y-%m").to_string(),
        )
    }

    pub fn decision_report(&self) -> DecisionReport {
        DecisionReport::from_log(&self.suggestions.decisions_text)
    }

    /// `r`: asks again for these hashes, dropping what was cached or failed.
    pub fn request_suggestions(&mut self, hashes: &[String], cx: &mut Context<Self>) {
        for hash in hashes {
            self.suggestions.cached.remove(hash);
            self.suggestions.failed.remove(hash);
        }
        self.suggestions.paused = None;
        self.pump_suggestions(cx);
        cx.notify();
    }

    /// *Retry* on the paused line: forgets the pause and every failure.
    pub fn retry_suggestions(&mut self, cx: &mut Context<Self>) {
        self.suggestions.paused = None;
        self.suggestions.failed.clear();
        self.pump_suggestions(cx);
        cx.notify();
    }

    /// Asks for every uncached item while a view is open, newest first, at
    /// most [`MAX_IN_FLIGHT`] at a time (§7.3). Safe to call often.
    fn pump_suggestions(&mut self, cx: &mut Context<Self>) {
        if self.open_views == 0 || self.suggestions.paused.is_some() {
            return;
        }
        let Some(api_key) = self.gateway_key(cx) else {
            return;
        };
        if self.vault.is_none() {
            return;
        }
        if !self.suggestions.loaded {
            self.load_suggestion_state(cx);
            return;
        }
        let Some(policy) = self.policy.clone() else {
            self.load_policy(cx);
            return;
        };
        let free = MAX_IN_FLIGHT.saturating_sub(self.suggestions.in_flight.len());
        let candidates: Vec<InboxEntry> = self
            .items
            .iter()
            .filter(|entry| {
                self.suggestion_for(&entry.hash).is_none()
                    && !self.suggestions.in_flight.contains_key(&entry.hash)
                    && !self.auto_archive.asking.contains(&entry.hash)
                    && !self.suggestions.failed.contains_key(&entry.hash)
            })
            .take(free)
            .cloned()
            .collect();
        if candidates.is_empty() {
            return;
        }
        let http = cx.http_client();
        for entry in candidates {
            let hash = entry.hash.clone();
            let task = cx.spawn({
                let http = http.clone();
                let api_key = api_key.clone();
                let hash = hash.clone();
                let policy = policy.clone();
                async move |this, cx| {
                    let request = build_request(
                        &entry.item,
                        &policy.rows,
                        &policy.criteria,
                        Local::now().date_naive(),
                    )
                    .to_string();
                    let outcome = ask_jev(&http, &api_key, request, cx).await;
                    this.update(cx, |service, cx| {
                        service.finish_suggestion(
                            &hash,
                            &entry.item.stem,
                            &policy.hash,
                            outcome,
                            cx,
                        )
                    })
                    .ok();
                }
            });
            self.suggestions.in_flight.insert(hash, task);
        }
    }

    fn finish_suggestion(
        &mut self,
        hash: &str,
        stem: &str,
        policy: &str,
        outcome: AskOutcome,
        cx: &mut Context<Self>,
    ) {
        self.suggestions.in_flight.remove(hash);
        match outcome {
            AskOutcome::Answered(suggestion) => {
                self.cache_answer(hash, stem, policy, suggestion, cx)
            }
            AskOutcome::Paused(reason) => self.suggestions.paused = Some(reason),
            AskOutcome::Failed(reason) => {
                self.suggestions.failed.insert(hash.to_string(), reason);
            }
        }
        cx.emit(InboxEvent::SuggestionsChanged);
        cx.notify();
        self.pump_suggestions(cx);
    }

    /// Records an answer in the cache file and in memory, and counts its
    /// cost.
    fn cache_answer(
        &mut self,
        hash: &str,
        stem: &str,
        policy: &str,
        suggestion: Suggestion,
        cx: &mut Context<Self>,
    ) {
        self.suggestions.session_cost += suggestion.cost;
        let record =
            SuggestionRecord::new(hash, stem, &suggestion, &Utc::now().to_rfc3339(), policy);
        match serde_json::to_string(&record) {
            Ok(line) => {
                self.suggestions.cache_text.push_str(&line);
                self.suggestions.cache_text.push('\n');
                self.persist(SUGGESTIONS_FILE, self.suggestions.cache_text.clone(), cx);
            }
            Err(error) => log::warn!("Thock: couldn't record a suggestion: {error:#}"),
        }
        self.suggestions
            .cached
            .insert(hash.to_string(), (suggestion, Some(policy.to_string())));
    }

    /// Loads the cache and the decision log once; `pump_suggestions` runs
    /// again when they are in, and decisions recorded meanwhile land.
    fn load_suggestion_state(&mut self, cx: &mut Context<Self>) {
        if self.suggestions.loaded || self.suggestions.loading.is_some() {
            return;
        }
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.project.read(cx).fs().clone();
        self.suggestions.loading = Some(cx.spawn(async move |this, cx| {
            let cache_text = fs
                .load(&state_path(&vault, SUGGESTIONS_FILE))
                .await
                .unwrap_or_default();
            let decisions_text = fs
                .load(&state_path(&vault, DECISIONS_FILE))
                .await
                .unwrap_or_default();
            let auto_archive_text = fs
                .load(&state_path(&vault, AUTO_ARCHIVE_FILE))
                .await
                .unwrap_or_default();
            this.update(cx, |service, cx| {
                // The newest record for a note wins, so a re-ask under a new
                // policy replaces the stale answer.
                let cached = cache_text
                    .lines()
                    .filter_map(|line| serde_json::from_str::<SuggestionRecord>(line).ok())
                    .map(|record| (record.hash.clone(), (record.suggestion(), record.policy)))
                    .collect();
                service.suggestions.cached = cached;
                service.suggestions.cache_text = cache_text;
                service.suggestions.decisions_text = decisions_text;
                service.suggestions.loaded = true;
                service.suggestions.loading = None;
                service.auto_archive.log = RunLog::parse(&auto_archive_text);
                service.auto_archive.log_text = auto_archive_text;
                for record in std::mem::take(&mut service.suggestions.pending_decisions) {
                    service.record_decision(&record, cx);
                }
                service.note_auto_archive_setting(cx);
                service.refresh_group(cx);
                cx.emit(InboxEvent::SuggestionsChanged);
                cx.notify();
                service.try_auto_archive(cx);
                service.pump_suggestions(cx);
            })
            .ok();
        }));
    }

    /// Appends a filing to `.thock/state/inbox/decisions.jsonl` (§7.6),
    /// after the writes landed.
    pub fn record_decision(&mut self, record: &DecisionRecord, cx: &mut Context<Self>) {
        if !self.suggestions.loaded {
            self.suggestions.pending_decisions.push(record.clone());
            self.load_suggestion_state(cx);
            return;
        }
        let Ok(line) = serde_json::to_string(record) else {
            return;
        };
        self.suggestions.decisions_text.push_str(&line);
        self.suggestions.decisions_text.push('\n');
        self.persist(DECISIONS_FILE, self.suggestions.decisions_text.clone(), cx);
        if record.corrects.is_some() {
            self.refresh_group(cx);
        }
    }

    /// Marks the decision for `stem` made at `at` undone (§6.2).
    pub fn mark_decision_undone(&mut self, stem: &str, at: &str, cx: &mut Context<Self>) {
        self.mark_undone_where(|record| record.stem == stem && record.at == at, cx);
    }

    /// Marks the morning run's filing of `stem` undone: the group's `u`
    /// (V56 §8.2).
    pub fn mark_automatic_undone(&mut self, stem: &str, run: &str, cx: &mut Context<Self>) {
        self.mark_undone_where(
            |record| {
                record.stem == stem && record.via == VIA_AUTO && record.run.as_deref() == Some(run)
            },
            cx,
        );
        self.refresh_group(cx);
    }

    fn mark_undone_where(
        &mut self,
        matches: impl Fn(&DecisionRecord) -> bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(pending) = self
            .suggestions
            .pending_decisions
            .iter_mut()
            .rev()
            .find(|record| matches(record))
        {
            pending.undone = true;
            return;
        }
        let mut lines: Vec<String> = self
            .suggestions
            .decisions_text
            .lines()
            .map(str::to_string)
            .collect();
        let target = lines.iter().rposition(|line| {
            serde_json::from_str::<DecisionRecord>(line).is_ok_and(|record| matches(&record))
        });
        let Some(index) = target else {
            return;
        };
        let Ok(mut record) = serde_json::from_str::<DecisionRecord>(&lines[index]) else {
            return;
        };
        record.undone = true;
        let Ok(line) = serde_json::to_string(&record) else {
            return;
        };
        lines[index] = line;
        let mut text = lines.join("\n");
        text.push('\n');
        self.suggestions.decisions_text = text.clone();
        self.persist(DECISIONS_FILE, text, cx);
    }

    /// Writes a whole state file; the in-memory text is the source of truth
    /// and writes land in the order they were queued.
    fn persist(&mut self, file: &'static str, text: String, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        self.suggestions.write_queue.push_back((file, text));
        if self.suggestions.writing {
            return;
        }
        self.suggestions.writing = true;
        let fs = self.project.read(cx).fs().clone();
        cx.spawn(async move |this, cx| {
            loop {
                let next = this
                    .update(cx, |service, _| service.suggestions.write_queue.pop_front())
                    .ok()
                    .flatten();
                let Some((file, text)) = next else {
                    this.update(cx, |service, _| service.suggestions.writing = false)
                        .ok();
                    return;
                };
                write_state(&fs, state_path(&vault, file), text)
                    .await
                    .with_context(|| format!("writing .thock/{STATE_DIR}/{file}"))
                    .log_err();
            }
        })
        .detach();
    }

    // ----- The policy (V56 §4.4, §7.5) -----

    /// Re-reads `triage-policy.md`. A changed policy makes every cached
    /// answer stale, so the view's chips and the next run re-ask.
    fn load_policy(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.project.read(cx).fs().clone();
        self.policy_task = Some(cx.spawn(async move |this, cx| {
            let text = fs.load(&vault.root.join(POLICY_PATH)).await.ok();
            let policy = LoadedPolicy::from_text(text.as_deref());
            this.update(cx, |service, cx| {
                if service.policy.as_ref() == Some(&policy) {
                    return;
                }
                service.policy = Some(policy);
                cx.emit(InboxEvent::SuggestionsChanged);
                cx.notify();
                service.try_auto_archive(cx);
                service.pump_suggestions(cx);
            })
            .ok();
        }));
    }

    // ----- The morning run (V56 §7) -----

    /// Starts the morning run on Gmail's polls. Called once, at creation.
    pub fn watch_gmail(
        &mut self,
        gmail: &Entity<crate::gmail_service::GmailService>,
        cx: &mut Context<Self>,
    ) {
        self.gmail = Some(gmail.downgrade());
        let subscription = cx.subscribe(
            gmail,
            |service, _, _: &crate::gmail_service::GmailPollFinished, cx| {
                service.gmail_poll_finished(cx)
            },
        );
        self._subscriptions.push(subscription);
    }

    /// A Gmail poll landed what it found. The run waits for the scan that
    /// follows, so nothing else that finishes first (the policy, the cache)
    /// starts it on a listing from before the poll's notes.
    fn gmail_poll_finished(&mut self, cx: &mut Context<Self>) {
        // The poll's own file events already re-read the queue.
        if !self.auto_archive_on() {
            return;
        }
        self.auto_archive.triggered = true;
        self.items_loaded = false;
        self.refresh_items(cx);
    }

    /// Whether Gmail polls are what start the run.
    fn gmail_polling(&self, cx: &App) -> bool {
        #[cfg(test)]
        if let Some(polling) = self.gmail_polling_for_test {
            return polling;
        }
        self.gmail
            .as_ref()
            .and_then(|gmail| gmail.upgrade())
            .is_some_and(|gmail| gmail.read(cx).is_polling())
    }

    fn auto_archive_on(&self) -> bool {
        self.vault
            .as_ref()
            .is_some_and(|vault| vault.config.inbox.auto_archive)
    }

    /// Where auto-archive stands (§9).
    pub fn auto_archive_status(&self, cx: &App) -> RunStatus {
        let stamped = self.policy.as_ref().is_some_and(|policy| policy.stamped);
        crate::inbox_auto_archive::status(
            self.auto_archive_on(),
            stamped,
            Self::auto_archive_key(cx).is_some(),
            &self.auto_archive.log,
            |moment| moment.with_timezone(&Local).date_naive(),
        )
    }

    /// The day the group's run finished, locally, for its header: the latest
    /// run that archived anything (§8.1).
    pub fn auto_archive_day(&self) -> Option<chrono::NaiveDate> {
        let run = crate::inbox_auto_archive::group_run(&self.auto_archive.log)?;
        let moment = run
            .finished
            .as_ref()
            .map_or(run.started_at, |finished| finished.at);
        Some(moment.with_timezone(&Local).date_naive())
    }

    /// The view's Auto-archived group (§8.1).
    pub fn auto_archived(&self) -> &[AutoArchivedEntry] {
        &self.auto_archive.group
    }

    /// Records the moment `[inbox] auto_archive` is seen turned on or off,
    /// so the first run looks only at what arrives after it was turned on.
    /// A vault that never turns it on never gets the file (G1).
    fn note_auto_archive_setting(&mut self, cx: &mut Context<Self>) {
        if !self.suggestions.loaded {
            return;
        }
        let on = self.auto_archive_on();
        if on == self.auto_archive.log.is_enabled() {
            return;
        }
        let at = Utc::now();
        let event = if on {
            RunEvent::Enabled { at }
        } else {
            RunEvent::Disabled { at }
        };
        self.append_run_event(event, cx);
    }

    fn append_run_event(&mut self, event: RunEvent, cx: &mut Context<Self>) {
        match event.to_json_line() {
            Ok(line) => {
                self.auto_archive.log_text.push_str(&line);
                self.auto_archive.log_text.push('\n');
                self.persist(AUTO_ARCHIVE_FILE, self.auto_archive.log_text.clone(), cx);
            }
            Err(error) => log::warn!("Thock: couldn't record the auto-archive run: {error:#}"),
        }
        self.auto_archive.log.apply(event);
        cx.notify();
    }

    /// `thock::RunInboxAutoArchive`: runs now, or picks up a stopped run.
    pub fn run_auto_archive_now(&mut self, cx: &mut Context<Self>) {
        self.auto_archive.triggered = true;
        self.auto_archive.manual = true;
        self.try_auto_archive(cx);
    }

    /// Starts the run when one was asked for, everything it reads is in,
    /// and it is due (§7.3). Safe to call often.
    fn try_auto_archive(&mut self, cx: &mut Context<Self>) {
        if !self.auto_archive.triggered || self.auto_archive.running.is_some() {
            return;
        }
        let Some(vault) = self.vault.clone() else {
            return;
        };
        if !vault.config.inbox.auto_archive {
            self.auto_archive.triggered = false;
            self.auto_archive.manual = false;
            return;
        }
        if !self.suggestions.loaded || !self.items_loaded {
            return;
        }
        let Some(policy) = self.policy.clone() else {
            return;
        };
        // Never on the shipped defaults (G2), and never without a grant:
        // both are shown as the group's status, nothing is written.
        let Some(api_key) = Self::auto_archive_key(cx).filter(|_| policy.stamped) else {
            self.auto_archive.triggered = false;
            self.auto_archive.manual = false;
            cx.notify();
            return;
        };
        let manual = std::mem::take(&mut self.auto_archive.manual);
        self.auto_archive.triggered = false;
        let now = Utc::now();
        let (run, watermark, skip, fresh) =
            match due(&self.auto_archive.log, &now.with_timezone(&Local), manual) {
                Due::No => return,
                Due::Resume {
                    run,
                    watermark,
                    skip,
                } => (run, watermark, skip, false),
                Due::Fresh { watermark, skip } => (run_id(now), watermark, skip, true),
            };
        let configured = vault.config.inbox.auto_archive_threshold;
        let threshold = effective_threshold(configured);
        if configured.is_some_and(|configured| configured != threshold)
            && !std::mem::replace(&mut self.auto_archive.clamp_logged, true)
        {
            log::warn!(
                "Thock: [inbox] auto_archive_threshold {configured:?} is outside 0.85–1.0; using {threshold}"
            );
        }
        let stems = candidates(
            self.items
                .iter()
                .map(|entry| (entry.item.stem.as_str(), entry.arrived)),
            watermark,
            &skip,
        );
        let entries: Vec<InboxEntry> = stems
            .iter()
            .filter_map(|stem| {
                self.items
                    .iter()
                    .find(|entry| &entry.item.stem == stem)
                    .cloned()
            })
            .collect();
        if fresh {
            self.append_run_event(
                RunEvent::Started {
                    run: run.clone(),
                    at: now,
                    watermark,
                    policy: policy.hash.clone(),
                    threshold,
                    candidates: entries.len(),
                },
                cx,
            );
        }
        let account = self
            .gmail
            .as_ref()
            .and_then(|gmail| gmail.upgrade())
            .and_then(|gmail| gmail.read(cx).account().map(str::to_string));
        let account = account.or_else(|| self.account.clone());
        let mut names = vault.config.inbox.names.clone();
        if let Some(account) = &account {
            names.extend(crate::inbox_auto_archive::account_names(
                self.items
                    .iter()
                    .filter_map(|entry| entry.item.from.as_deref()),
                account,
            ));
        }
        let cached: HashMap<String, Suggestion> = entries
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.hash.clone(),
                    self.suggestion_for(&entry.hash)?.clone(),
                ))
            })
            .collect();
        self.auto_archive.asking = entries
            .iter()
            .filter(|entry| !cached.contains_key(&entry.hash))
            .map(|entry| entry.hash.clone())
            .collect();
        let job = RunJob {
            run,
            vault,
            policy,
            threshold,
            account,
            names,
            entries,
            cached,
            api_key,
        };
        let project = self.project.clone();
        let http = cx.http_client();
        self.auto_archive.running = Some(cx.spawn(async move |this, cx| {
            let outcome = Self::auto_archive_run(&this, &project, &http, &job, cx).await;
            this.update(cx, |service, cx| {
                service.finish_auto_archive(&job.run, outcome, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// One run over its candidates, newest first: the guardrail, then Jev at
    /// most [`MAX_IN_FLIGHT`] at a time, then each filing in order. A 402,
    /// or auto-archive being turned off, stops it where it is.
    async fn auto_archive_run(
        this: &WeakEntity<Self>,
        project: &Entity<Project>,
        http: &Arc<dyn HttpClient>,
        job: &RunJob,
        cx: &mut AsyncApp,
    ) -> RunOutcome {
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let today = Local::now().date_naive();
        let mut stopped: Option<String> = None;
        for chunk in job.entries.chunks(MAX_IN_FLIGHT) {
            let asks = chunk.iter().map(|entry| {
                let cached = job.cached.get(&entry.hash).cloned();
                let replied = entry.item.source.as_deref() == Some("gmail")
                    && person_wrote(
                        &parse_thread(&entry.item.body, entry.item.from.as_deref()),
                        job.account.as_deref(),
                        &job.names,
                    );
                let request =
                    build_request(&entry.item, &job.policy.rows, &job.policy.criteria, today)
                        .to_string();
                let cx: &AsyncApp = cx;
                async move {
                    if replied {
                        return None;
                    }
                    Some(match cached {
                        Some(suggestion) => (AskOutcome::Answered(suggestion), true),
                        None => (ask_jev(http, &job.api_key, request, cx).await, false),
                    })
                }
            });
            let answers = futures::future::join_all(asks).await;
            for (entry, answer) in chunk.iter().zip(answers) {
                // An answer is paid for even when the run stops before it,
                // so it is cached either way.
                if let Some((AskOutcome::Answered(suggestion), false)) = &answer {
                    this.update(cx, |service, cx| {
                        service.cache_answer(
                            &entry.hash,
                            &entry.item.stem,
                            &job.policy.hash,
                            suggestion.clone(),
                            cx,
                        )
                    })
                    .ok();
                }
                if stopped.is_some() {
                    continue;
                }
                // Turning auto-archive off stops a run between filings.
                match this.read_with(cx, |service, _| service.auto_archive_on()) {
                    Ok(true) => {}
                    Ok(false) => {
                        stopped = Some(TURNED_OFF.to_string());
                        continue;
                    }
                    Err(_) => return RunOutcome::Released,
                }
                let event = match answer {
                    None => skipped(job, entry, SkipReason::Replied),
                    Some((AskOutcome::Paused(reason), _)) => {
                        stopped = Some(reason.to_string());
                        continue;
                    }
                    Some((AskOutcome::Failed(reason), _)) => {
                        log::warn!("Thock: auto-archive couldn't ask about an item: {reason}");
                        skipped(job, entry, SkipReason::Error)
                    }
                    Some((AskOutcome::Answered(suggestion), _)) => {
                        if clears_bar(&suggestion, job.threshold) {
                            Self::file_automatically(
                                this,
                                project,
                                &fs,
                                job,
                                entry,
                                &suggestion,
                                cx,
                            )
                            .await
                        } else {
                            skipped(job, entry, SkipReason::Below)
                        }
                    }
                };
                if this
                    .update(cx, |service, cx| service.append_run_event(event, cx))
                    .is_err()
                {
                    return RunOutcome::Released;
                }
            }
            if stopped.is_some() {
                break;
            }
        }
        RunOutcome::Done { stopped }
    }

    /// The filing `e` does (§7.1), with the automatic log line, then the
    /// decision record. A failure leaves the item where it was.
    async fn file_automatically(
        this: &WeakEntity<Self>,
        project: &Entity<Project>,
        fs: &Arc<dyn Fs>,
        job: &RunJob,
        entry: &InboxEntry,
        suggestion: &Suggestion,
        cx: &mut AsyncApp,
    ) -> RunEvent {
        match crate::inbox_view::archive_automatically(project, fs, &job.vault, &entry.item, cx)
            .await
        {
            Ok(Some(filed)) => {
                let record = DecisionRecord {
                    stem: entry.item.stem.clone(),
                    suggested: Some(suggestion.choice.clone()),
                    confidence: Some(suggestion.confidence),
                    chosen: Destination::Archive.key().to_string(),
                    via: VIA_AUTO.to_string(),
                    at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    undone: false,
                    run: Some(job.run.clone()),
                    corrects: None,
                };
                this.update(cx, |service, cx| service.record_decision(&record, cx))
                    .ok();
                RunEvent::Archived {
                    run: job.run.clone(),
                    stem: entry.item.stem.clone(),
                    path: filed.path,
                    confidence: suggestion.confidence,
                    line: filed.line,
                }
            }
            Ok(None) => skipped(job, entry, SkipReason::Error),
            Err(error) => {
                log::warn!(
                    "Thock: auto-archive couldn't file {}: {error:#}",
                    entry.item.stem
                );
                skipped(job, entry, SkipReason::Error)
            }
        }
    }

    fn finish_auto_archive(&mut self, run: &str, outcome: RunOutcome, cx: &mut Context<Self>) {
        self.auto_archive.running = None;
        self.auto_archive.asking.clear();
        let RunOutcome::Done { stopped } = outcome else {
            return;
        };
        let (archived, skipped) = self
            .auto_archive
            .log
            .runs
            .iter()
            .rev()
            .find(|candidate| candidate.id == run)
            .map_or((0, 0), |run| (run.archived.len(), run.skipped.len()));
        self.append_run_event(
            RunEvent::Finished {
                run: run.to_string(),
                at: Utc::now(),
                archived,
                skipped,
                stopped,
            },
            cx,
        );
        self.refresh_items(cx);
        self.refresh_group(cx);
        if archived > 0
            && let Some(gmail) = crate::gmail_service::service_for_project(&self.project, cx)
        {
            gmail.update(cx, |gmail, cx| gmail.reconcile_soon(cx));
        }
        cx.emit(InboxEvent::SuggestionsChanged);
        cx.notify();
    }

    /// Re-reads the group: the last run's filings nobody corrected whose
    /// notes are still in the archive, read like inbox rows.
    fn refresh_group(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let items: Vec<GroupItem> = crate::inbox_auto_archive::group_run(&self.auto_archive.log)
            .map(|run| group_items(run, &self.suggestions.decisions_text))
            .unwrap_or_default();
        let fs = self.project.read(cx).fs().clone();
        self.auto_archive.group_task = Some(cx.spawn(async move |this, cx| {
            let mut group = Vec::new();
            for GroupItem { run, item } in items {
                if let Some(entry) = load_archived(&fs, &vault, &item).await {
                    group.push(AutoArchivedEntry {
                        entry,
                        run,
                        confidence: item.confidence,
                        line: item.line,
                    });
                }
            }
            this.update(cx, |service, cx| {
                if service.auto_archive.group != group {
                    service.auto_archive.group = group;
                    cx.emit(InboxEvent::ItemsChanged);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// `thock::TurnOffInboxAutoArchive` (§5.3): the two keys leave `[inbox]`
    /// and nothing else in the config changes.
    pub fn turn_off_auto_archive(
        &mut self,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let fs = self.project.read(cx).fs().clone();
        cx.spawn(async move |this, cx| {
            let path = vault.root.join(VAULT_MARKER_DIR).join(VAULT_CONFIG_FILE);
            let result = async {
                let text = fs.load(&path).await.context("reading .thock/config.toml")?;
                let edited = without_auto_archive_keys(&text)?;
                if edited != text {
                    fs.atomic_write(path.clone(), edited)
                        .await
                        .context("writing .thock/config.toml")?;
                }
                anyhow::Ok(())
            }
            .await;
            let message = match &result {
                Ok(()) => {
                    "Auto-archive is off. The policy and the map of your work stay.".to_string()
                }
                Err(error) => format!("Couldn't turn auto-archive off: {error:#}"),
            };
            if result.is_ok() {
                this.update(cx, |service, cx| service.reload(cx)).ok();
            }
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_toast(
                        workspace::Toast::new(
                            workspace::notifications::NotificationId::unique::<AutoArchiveToast>(),
                            message,
                        )
                        .autohide(),
                        cx,
                    );
                })
                .ok();
        })
        .detach();
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

    #[cfg(test)]
    pub(crate) fn replace_run_log_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.auto_archive.log = RunLog::parse(text);
        self.auto_archive.log_text = text.to_string();
        self.refresh_group(cx);
    }

    #[cfg(test)]
    pub(crate) fn set_account_for_test(&mut self, account: &str) {
        self.account = Some(account.to_string());
    }

    #[cfg(test)]
    pub(crate) fn set_vault_for_test(&mut self, vault: Vault, cx: &mut Context<Self>) {
        self.vault = Some(vault);
        self.suggestions.loaded = false;
        self.load_policy(cx);
        self.load_suggestion_state(cx);
        self.refresh_items(cx);
    }
}

/// The toast turning auto-archive off shows.
struct AutoArchiveToast;

/// Why a run that was going when auto-archive was turned off stopped.
const TURNED_OFF: &str = "auto-archive was turned off";

/// What a run needs, captured when it starts.
struct RunJob {
    run: String,
    vault: Vault,
    policy: LoadedPolicy,
    threshold: f64,
    account: Option<String>,
    names: Vec<String>,
    /// Newest first.
    entries: Vec<InboxEntry>,
    /// Answers already asked under this policy, by note hash.
    cached: HashMap<String, Suggestion>,
    api_key: String,
}

enum RunOutcome {
    Done {
        stopped: Option<String>,
    },
    /// The service went away mid-run; the next start resumes it.
    Released,
}

fn skipped(job: &RunJob, entry: &InboxEntry, reason: SkipReason) -> RunEvent {
    RunEvent::Skipped {
        run: job.run.clone(),
        stem: entry.item.stem.clone(),
        reason,
    }
}

/// A filed note read back from the archive as a row. `None` once the note
/// has left it (sent back, or moved by hand).
async fn load_archived(fs: &Arc<dyn Fs>, vault: &Vault, item: &ArchivedItem) -> Option<InboxEntry> {
    let content = fs.load(&vault.root.join(&item.path)).await.ok()?;
    let item = parse_inbox_note(&item.path, &content);
    let arrived = item
        .captured
        .as_deref()
        .and_then(|captured| DateTime::parse_from_rfc3339(captured).ok())
        .map(|captured| captured.with_timezone(&Utc));
    Some(InboxEntry {
        hash: note_hash(&content),
        is_new: false,
        arrived,
        item,
    })
}

fn state_path(vault: &Vault, file: &str) -> PathBuf {
    vault.root.join(VAULT_MARKER_DIR).join(STATE_DIR).join(file)
}

async fn write_state(fs: &Arc<dyn Fs>, path: PathBuf, contents: String) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs.create_dir(parent).await?;
    }
    fs.atomic_write(path, contents).await
}

/// One decision request (§7.3): a 429 or 5xx retries with backoff, a 402
/// pauses, anything else fails this item only.
async fn ask_jev(
    http: &Arc<dyn HttpClient>,
    api_key: &str,
    body: String,
    cx: &AsyncApp,
) -> AskOutcome {
    let mut delay = Duration::from_secs(1);
    let mut last_transient: Option<String> = None;
    for attempt in 0..3 {
        if attempt > 0 {
            cx.background_executor().timer(delay).await;
            delay *= 2;
        }
        let request = Request::builder()
            .method(http::Method::POST)
            .uri(DECISIONS_ENDPOINT)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(AsyncBody::from(body.clone().into_bytes()));
        let request = match request {
            Ok(request) => request,
            Err(error) => return AskOutcome::Failed(format!("{error}").into()),
        };
        let mut response = match http.send(request).await {
            Ok(response) => response,
            Err(error) => {
                last_transient = Some(format!("{error:#}"));
                continue;
            }
        };
        let mut text = String::new();
        if let Err(error) = response.body_mut().read_to_string(&mut text).await {
            last_transient = Some(format!("{error:#}"));
            continue;
        }
        let status = response.status();
        if status.is_success() {
            return match parse_response(&text) {
                Ok(suggestion) => AskOutcome::Answered(suggestion),
                Err(error) => AskOutcome::Failed(format!("{error:#}").into()),
            };
        }
        if status == http::StatusCode::PAYMENT_REQUIRED {
            return AskOutcome::Paused("Thock Plus allowance spent for this cycle".into());
        }
        if status == http::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            last_transient = Some(format!("the suggestion service answered {status}"));
            continue;
        }
        return AskOutcome::Failed(format!("the suggestion service answered {status}").into());
    }
    AskOutcome::Failed(
        last_transient
            .unwrap_or_else(|| "the suggestion service didn't answer".to_string())
            .into(),
    )
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

/// Reads the queue the way the view shows it (V41 §8.2): every note parsed,
/// split by the ritual's watermark, newest first.
async fn load_items(fs: &Arc<dyn Fs>, vault: &Vault, dir: &str) -> Vec<InboxEntry> {
    let watermark = match fs.load(&state_path(vault, LAST_TRIAGE_FILE)).await {
        Ok(text) => parse_watermark(&text),
        Err(_) => None,
    };
    let mut entries = Vec::new();
    for path in inbox_note_paths(fs, &vault.root.join(dir)).await {
        let Ok(content) = fs.load(&path).await else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let modified = match fs.metadata(&path).await {
            Ok(Some(metadata)) => metadata
                .mtime
                .to_seconds_and_nanos_for_persistence()
                .and_then(|(seconds, nanos)| DateTime::from_timestamp(seconds as i64, nanos)),
            _ => None,
        };
        let item = parse_inbox_note(&format!("{dir}/{name}"), &content);
        let moment = item
            .captured
            .as_deref()
            .and_then(|captured| DateTime::parse_from_rfc3339(captured).ok())
            .map(|captured| captured.with_timezone(&Utc))
            .or(modified);
        entries.push((
            moment,
            InboxEntry {
                hash: note_hash(&content),
                is_new: is_new(&item, modified, watermark),
                arrived: moment,
                item,
            },
        ));
    }
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.item.stem.cmp(&a.1.item.stem))
    });
    entries.into_iter().map(|(_, entry)| entry).collect()
}

/// One pass over the landing zone and the triage log: stems for collision
/// handling, `capture:` digests plus log markers as the rebuildable record
/// (spec §4.3).
async fn scan_vault(fs: &Arc<dyn Fs>, vault: &Vault, config: &InboxConfig) -> VaultScan {
    let mut scan = VaultScan::default();
    for path in inbox_note_paths(fs, &vault.root.join(&config.dir)).await {
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
    use crate::inbox_suggest::Destination;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};
    use settings::SettingsStore;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            let titles: Vec<&str> = service
                .items()
                .iter()
                .map(|entry| entry.item.title.as_str())
                .collect();
            assert_eq!(titles.len(), 2);
            assert!(titles.contains(&"Ship it") && titles.contains(&"Call back"));
            assert!(service.items().iter().all(|entry| entry.is_new));
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
                assert!(reason.summary().contains("thock/inbox"), "{reason}");
            }
            other => panic!("expected holding, got {other:?}"),
        });
    }

    // ----- Items and suggestions (V41) -----

    fn note(title: &str, captured: &str, digest: &str) -> String {
        format!(
            "---\nsource:   google-tasks\ncapture:  {digest}\ncaptured: {captured}\ntitle:    {title}\n---\n\n# {title}\n"
        )
    }

    fn entitlement(exhausted: bool) -> plus::Entitlement {
        serde_json::from_str(
            &serde_json::json!({
                "user_id": "u1",
                "status": if exhausted { "exhausted" } else { "active" },
                "plan_id": "dev",
                "plan_name": "Thock Plus (dev)",
                "allowance_units": 500,
                "used_units": if exhausted { 500 } else { 10 },
                "remaining_units": if exhausted { 0 } else { 490 },
                "gateway": {
                    "provider": "openrouter",
                    "api_key": "sk-or-v1-test",
                    "models": {"default": "google/gemini-2.5-flash"}
                }
            })
            .to_string(),
        )
        .unwrap()
    }

    fn jev_answer(choice: &str, confidence: f64) -> String {
        serde_json::json!({
            "model": "typesafe/jev-1.13",
            "answers": {"destination": {"choice": choice, "probabilities": {choice: confidence}, "confidence": confidence}},
            "usage": {"cost": 0.00002}
        })
        .to_string()
    }

    struct Probe {
        requests: Arc<AtomicUsize>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    /// A fake decisions endpoint that answers `status`/`body` after a short
    /// delay, counting how many requests overlap.
    fn install_fake_jev(cx: &mut TestAppContext, status: u16, body: String) -> Probe {
        let probe = Probe {
            requests: Arc::new(AtomicUsize::new(0)),
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
        };
        let requests = probe.requests.clone();
        let in_flight = probe.in_flight.clone();
        let max_in_flight = probe.max_in_flight.clone();
        let executor = cx.executor();
        let client = FakeHttpClient::create(move |request| {
            assert_eq!(request.uri().to_string(), DECISIONS_ENDPOINT);
            assert_eq!(
                request.headers().get("Authorization").unwrap(),
                "Bearer sk-or-v1-test"
            );
            requests.fetch_add(1, Ordering::SeqCst);
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
            let in_flight = in_flight.clone();
            let executor = executor.clone();
            let body = body.clone();
            async move {
                executor.timer(Duration::from_millis(50)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body.into_bytes()))
                    .unwrap())
            }
        });
        cx.update(|cx| cx.set_http_client(client));
        probe
    }

    async fn service_with_notes(
        cx: &mut TestAppContext,
        notes: usize,
    ) -> (Arc<FakeFs>, Entity<InboxService>) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault/inbox")).await.unwrap();
        for index in 0..notes {
            fs.insert_file(
                Path::new(&format!(
                    "/vault/inbox/2026-10-0{}-0900-item-{index}.md",
                    index + 1
                )),
                note(
                    &format!("Item {index}"),
                    &format!("2026-10-0{}T09:00:00Z", index + 1),
                    &format!("d{index:011}"),
                )
                .into_bytes(),
            )
            .await;
        }
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = cx.new(|cx| InboxService::new(project.clone(), cx));
        service.update(cx, |service, cx| {
            service.set_vault_for_test(test_vault(), cx)
        });
        cx.run_until_parked();
        (fs, service)
    }

    #[gpui::test]
    async fn items_are_newest_first_and_split_by_the_watermark(cx: &mut TestAppContext) {
        let (fs, service) = service_with_notes(cx, 3).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let _subscription = cx.update({
            let events = events.clone();
            |cx| {
                cx.subscribe(&service, move |_, event: &InboxEvent, _| {
                    events.lock().unwrap().push(*event);
                })
            }
        });
        service.read_with(cx, |service, _| {
            let titles: Vec<&str> = service
                .items()
                .iter()
                .map(|entry| entry.item.title.as_str())
                .collect();
            assert_eq!(titles, ["Item 2", "Item 1", "Item 0"]);
            assert!(service.items().iter().all(|entry| entry.is_new));
        });

        // The ritual finishes: everything before its watermark is carried
        // over, and the service re-reads without a poll.
        fs.create_dir(Path::new("/vault/.thock/state/inbox"))
            .await
            .unwrap();
        fs.insert_file(
            Path::new("/vault/.thock/state/inbox/last-triage"),
            b"2026-10-02T12:00:00Z\n".to_vec(),
        )
        .await;
        cx.run_until_parked();
        service.read_with(cx, |service, _| {
            let is_new: Vec<bool> = service.items().iter().map(|entry| entry.is_new).collect();
            assert_eq!(is_new, [true, false, false]);
        });

        // A note leaving the folder updates the queue and announces it.
        fs.remove_file(
            Path::new("/vault/inbox/2026-10-03-0900-item-2.md"),
            Default::default(),
        )
        .await
        .unwrap();
        cx.run_until_parked();
        service.read_with(cx, |service, _| {
            assert_eq!(service.queue_depth(), 2);
            assert_eq!(service.items()[0].item.title, "Item 1");
        });
        assert!(events.lock().unwrap().contains(&InboxEvent::ItemsChanged));
    }

    #[gpui::test]
    async fn suggestions_are_asked_only_while_a_view_is_open_four_at_a_time(
        cx: &mut TestAppContext,
    ) {
        let (fs, service) = service_with_notes(cx, 6).await;
        let probe = install_fake_jev(cx, 200, jev_answer("someday", 0.91));

        // No grant yet: nothing is asked.
        service.update(cx, |service, cx| service.view_opened(cx));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 0);
        service.read_with(cx, |service, cx| {
            assert_eq!(
                service.suggestion_availability(cx),
                SuggestionAvailability::NoGrant
            );
        });

        // The grant arrives: the open view starts the requests, newest
        // first, four at a time.
        cx.update(|cx| plus::set_current_entitlement(Some(entitlement(false)), cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 6);
        assert_eq!(probe.max_in_flight.load(Ordering::SeqCst), MAX_IN_FLIGHT);
        service.read_with(cx, |service, cx| {
            assert_eq!(
                service.suggestion_availability(cx),
                SuggestionAvailability::On
            );
            for entry in service.items() {
                let suggestion = service.suggestion_for(&entry.hash).expect("cached");
                assert_eq!(suggestion.destination, Some(Destination::Someday));
            }
            assert!((service.session_cost() - 6.0 * 0.00002).abs() < 1e-9);
            assert!(service.month_cost() > 0.0);
        });
        let cache = fs
            .load(Path::new("/vault/.thock/state/inbox/suggestions.jsonl"))
            .await
            .unwrap();
        assert_eq!(cache.lines().count(), 6, "{cache}");

        // Cached hashes are never asked twice, even from a fresh service.
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        let fresh = cx.new(|cx| InboxService::new(project, cx));
        fresh.update(cx, |service, cx| {
            service.set_vault_for_test(test_vault(), cx);
            service.view_opened(cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 6);
        fresh.read_with(cx, |service, _| {
            assert_eq!(service.items().len(), 6);
            assert!(
                service
                    .items()
                    .iter()
                    .all(|entry| service.suggestion_for(&entry.hash).is_some())
            );
        });

        // An edited note is a new hash and is asked once more, by the one
        // service whose view is still open.
        service.update(cx, |service, cx| service.view_closed(cx));
        fs.insert_file(
            Path::new("/vault/inbox/2026-10-01-0900-item-0.md"),
            note("Item 0 edited", "2026-10-01T09:00:00Z", "d00000000000").into_bytes(),
        )
        .await;
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 7);

        // With every view closed, a new item is not asked about.
        fresh.update(cx, |service, cx| service.view_closed(cx));
        fs.insert_file(
            Path::new("/vault/inbox/2026-10-09-0900-item-9.md"),
            note("Item 9", "2026-10-09T09:00:00Z", "d00000000009").into_bytes(),
        )
        .await;
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 7);
    }

    #[gpui::test]
    async fn a_402_pauses_without_retry_and_retry_resumes(cx: &mut TestAppContext) {
        let (_fs, service) = service_with_notes(cx, 2).await;
        let probe = install_fake_jev(cx, 402, r#"{"error":"allowance spent"}"#.to_string());
        cx.update(|cx| plus::set_current_entitlement(Some(entitlement(false)), cx));
        service.update(cx, |service, cx| service.view_opened(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();
        // Both items were in flight when the first 402 came back; neither
        // is retried.
        assert_eq!(probe.requests.load(Ordering::SeqCst), 2);
        service.read_with(cx, |service, _| {
            assert!(service.suggestions_paused().is_some());
            assert!(
                service
                    .items()
                    .iter()
                    .all(|entry| service.suggestion_for(&entry.hash).is_none())
            );
        });

        // Retry asks again.
        service.update(cx, |service, cx| service.retry_suggestions(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 4);

        // An exhausted entitlement never asks at all.
        cx.update(|cx| plus::set_current_entitlement(Some(entitlement(true)), cx));
        service.update(cx, |service, cx| service.retry_suggestions(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 4);
        service.read_with(cx, |service, cx| {
            assert_eq!(
                service.suggestion_availability(cx),
                SuggestionAvailability::Exhausted
            );
        });
    }

    #[gpui::test]
    async fn a_server_error_retries_then_marks_the_item(cx: &mut TestAppContext) {
        let (_fs, service) = service_with_notes(cx, 1).await;
        let probe = install_fake_jev(cx, 503, String::new());
        cx.update(|cx| plus::set_current_entitlement(Some(entitlement(false)), cx));
        service.update(cx, |service, cx| service.view_opened(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 3);
        service.read_with(cx, |service, _| {
            assert!(service.suggestions_paused().is_none());
            let hash = &service.items()[0].hash;
            assert!(service.suggestion_for(hash).is_none());
            assert!(
                service
                    .suggestion_failure(hash)
                    .is_some_and(|reason| reason.contains("503")),
                "{:?}",
                service.suggestion_failure(hash)
            );
            assert!(service.any_suggestion_failed());
        });
    }

    #[gpui::test]
    async fn config_can_turn_suggestions_off(cx: &mut TestAppContext) {
        let (_fs, service) = service_with_notes(cx, 1).await;
        let probe = install_fake_jev(cx, 200, jev_answer("soon", 0.7));
        cx.update(|cx| plus::set_current_entitlement(Some(entitlement(false)), cx));
        let mut vault = test_vault();
        vault.config.inbox.suggestions = false;
        service.update(cx, |service, cx| {
            service.set_vault_for_test(vault, cx);
            service.view_opened(cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(probe.requests.load(Ordering::SeqCst), 0);
        service.read_with(cx, |service, cx| {
            assert_eq!(
                service.suggestion_availability(cx),
                SuggestionAvailability::OffByConfig
            );
        });
    }

    #[gpui::test]
    async fn decisions_are_logged_and_undone_in_place(cx: &mut TestAppContext) {
        let (fs, service) = service_with_notes(cx, 1).await;
        cx.update(|cx| plus::set_current_entitlement(Some(entitlement(false)), cx));
        service.update(cx, |service, cx| service.view_opened(cx));
        cx.run_until_parked();
        let record = DecisionRecord {
            stem: "2026-10-01-0900-item-0".to_string(),
            suggested: Some("someday".to_string()),
            confidence: Some(0.91),
            chosen: "someday".to_string(),
            via: "space".to_string(),
            at: "2026-10-10T09:14:40Z".to_string(),
            undone: false,
            run: None,
            corrects: None,
        };
        service.update(cx, |service, cx| {
            service.record_decision(&record, cx);
            service.record_decision(
                &DecisionRecord {
                    chosen: "today".to_string(),
                    via: "t".to_string(),
                    at: "2026-10-10T09:15:00Z".to_string(),
                    ..record.clone()
                },
                cx,
            );
        });
        cx.run_until_parked();
        let log = fs
            .load(Path::new("/vault/.thock/state/inbox/decisions.jsonl"))
            .await
            .unwrap();
        assert_eq!(log.lines().count(), 2, "{log}");
        assert!(log.contains(r#""via":"space""#), "{log}");
        service.read_with(cx, |service, _| {
            assert_eq!(
                service.decision_report(),
                DecisionReport {
                    decisions: 2,
                    taken: 1,
                    ..DecisionReport::default()
                }
            );
        });

        service.update(cx, |service, cx| {
            service.mark_decision_undone(&record.stem, &record.at, cx);
        });
        cx.run_until_parked();
        let log = fs
            .load(Path::new("/vault/.thock/state/inbox/decisions.jsonl"))
            .await
            .unwrap();
        let first: DecisionRecord = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert!(first.undone);
        let second: DecisionRecord = serde_json::from_str(log.lines().nth(1).unwrap()).unwrap();
        assert!(!second.undone);
        service.read_with(cx, |service, _| {
            assert_eq!(
                service.decision_report(),
                DecisionReport {
                    decisions: 1,
                    taken: 0,
                    ..DecisionReport::default()
                }
            );
        });
    }

    #[gpui::test]
    async fn decisions_append_to_the_existing_log_without_a_grant(cx: &mut TestAppContext) {
        let (fs, _service) = service_with_notes(cx, 1).await;
        let earlier = r#"{"stem":"old","suggested":null,"confidence":null,"chosen":"today","via":"t","at":"2026-10-01T09:00:00Z"}"#;
        fs.create_dir(Path::new("/vault/.thock/state/inbox"))
            .await
            .unwrap();
        fs.insert_file(
            Path::new("/vault/.thock/state/inbox/decisions.jsonl"),
            format!("{earlier}\n").into_bytes(),
        )
        .await;
        fs.insert_file(
            Path::new("/vault/.thock/state/inbox/suggestions.jsonl"),
            format!(
                "{}\n",
                serde_json::to_string(&SuggestionRecord::new(
                    "h",
                    "s",
                    &crate::inbox_suggest::parse_response(&jev_answer("soon", 0.9)).unwrap(),
                    "2026-10-01T09:00:00Z",
                    &LoadedPolicy::from_text(None).hash,
                ))
                .unwrap()
            )
            .into_bytes(),
        )
        .await;
        // A fresh service, no grant at all, and a decision recorded before
        // the log has been read from disk.
        cx.update(|cx| plus::set_current_entitlement(None, cx));
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        let fresh = cx.new(|cx| InboxService::new(project, cx));
        fresh.update(cx, |service, cx| {
            service.set_vault_for_test(test_vault(), cx);
            service.view_opened(cx);
            service.record_decision(
                &DecisionRecord {
                    stem: "new".to_string(),
                    suggested: None,
                    confidence: None,
                    chosen: "archive".to_string(),
                    via: "e".to_string(),
                    at: "2026-10-10T09:00:00Z".to_string(),
                    undone: false,
                    run: None,
                    corrects: None,
                },
                cx,
            );
        });
        cx.run_until_parked();
        let log = fs
            .load(Path::new("/vault/.thock/state/inbox/decisions.jsonl"))
            .await
            .unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert_eq!(lines[0], earlier);
        assert!(lines[1].contains(r#""stem":"new""#), "{log}");
        // The popover's figures read the files whether or not there is a grant.
        fresh.read_with(cx, |service, _| {
            assert_eq!(
                service.decision_report().decisions,
                0,
                "neither had a suggestion"
            );
            assert!(service.month_cost() >= 0.0);
            assert!(service.suggestion_for("h").is_some());
        });
    }

    #[gpui::test]
    async fn move_note_never_overwrites_and_never_resurrects(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            serde_json::json!({
                "inbox": { "a.md": "# A\n", "b.md": "# B\n" },
                "archives": { "inbox": { "b.md": "older\n" } },
            }),
        )
        .await;
        let fs: Arc<dyn Fs> = fs;
        assert_eq!(
            move_note(
                &fs,
                Path::new("/vault/inbox/a.md"),
                Path::new("/vault/archives/inbox/a.md")
            )
            .await
            .unwrap(),
            MoveOutcome::Moved
        );
        assert!(!fs.is_file(Path::new("/vault/inbox/a.md")).await);
        assert_eq!(
            fs.load(Path::new("/vault/archives/inbox/a.md"))
                .await
                .unwrap(),
            "# A\n"
        );
        assert_eq!(
            move_note(
                &fs,
                Path::new("/vault/inbox/a.md"),
                Path::new("/vault/archives/inbox/a.md")
            )
            .await
            .unwrap(),
            MoveOutcome::SourceGone
        );
        assert_eq!(
            move_note(
                &fs,
                Path::new("/vault/inbox/b.md"),
                Path::new("/vault/archives/inbox/b.md")
            )
            .await
            .unwrap(),
            MoveOutcome::DestinationExists
        );
        assert_eq!(
            fs.load(Path::new("/vault/inbox/b.md")).await.unwrap(),
            "# B\n"
        );
        assert_eq!(
            fs.load(Path::new("/vault/archives/inbox/b.md"))
                .await
                .unwrap(),
            "older\n"
        );
        // Parents are created for a fresh archive folder.
        assert_eq!(
            move_note(
                &fs,
                Path::new("/vault/inbox/b.md"),
                Path::new("/vault/elsewhere/deep/b.md")
            )
            .await
            .unwrap(),
            MoveOutcome::Moved
        );
    }

    const STAMPED_POLICY: &str = "---\nrefreshed: 2026-11-01\n---\n# Triage policy\n\n## Proposals\n\n\
        | If it looks like… | Propose |\n| --- | --- |\n| A ticket about licences | Archive |\n";

    /// A vault with auto-archive on since 2026-01-01, a stamped policy (or
    /// `policy`), these notes in the inbox, and a fake Jev answering by
    /// title: `(status, choice, confidence)`.
    async fn auto_archive_service(
        cx: &mut TestAppContext,
        notes: &[(&str, String)],
        policy: &str,
        answer: fn(&str) -> (u16, &'static str, f64),
    ) -> (Arc<FakeFs>, Entity<InboxService>, Arc<AtomicUsize>) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        auto_archive_service_on(cx, fs, notes, policy, answer, false).await
    }

    /// The same on `fs`, with an Inbox view open when `view_open`.
    async fn auto_archive_service_on(
        cx: &mut TestAppContext,
        fs: Arc<FakeFs>,
        notes: &[(&str, String)],
        policy: &str,
        answer: fn(&str) -> (u16, &'static str, f64),
        view_open: bool,
    ) -> (Arc<FakeFs>, Entity<InboxService>, Arc<AtomicUsize>) {
        fs.create_dir(Path::new("/vault/inbox")).await.unwrap();
        fs.create_dir(Path::new("/vault/routines/inbox"))
            .await
            .unwrap();
        fs.create_dir(Path::new("/vault/.thock/state/inbox"))
            .await
            .unwrap();
        fs.insert_file(
            Path::new("/vault/routines/inbox/triage-policy.md"),
            policy.as_bytes().to_vec(),
        )
        .await;
        fs.insert_file(
            Path::new("/vault/.thock/state/inbox/auto-archive.jsonl"),
            b"{\"event\":\"enabled\",\"at\":\"2026-01-01T00:00:00Z\"}\n".to_vec(),
        )
        .await;
        for (name, content) in notes {
            fs.insert_file(
                Path::new(&format!("/vault/inbox/{name}.md")),
                content.as_bytes().to_vec(),
            )
            .await;
        }
        let requests = Arc::new(AtomicUsize::new(0));
        let client = FakeHttpClient::create({
            let requests = requests.clone();
            move |request| {
                requests.fetch_add(1, Ordering::SeqCst);
                async move {
                    let mut body = String::new();
                    request.into_body().read_to_string(&mut body).await?;
                    let value: serde_json::Value = serde_json::from_str(&body)?;
                    let title = value["state"]["item"]["title"].as_str().unwrap_or_default();
                    let (status, choice, confidence) = answer(title);
                    Ok(Response::builder()
                        .status(status)
                        .body(AsyncBody::from(jev_answer(choice, confidence).into_bytes()))
                        .unwrap())
                }
            }
        });
        cx.update(|cx| {
            cx.set_http_client(client);
            plus::set_current_entitlement(Some(entitlement(false)), cx);
        });
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = cx.new(|cx| InboxService::new(project.clone(), cx));
        let mut vault = test_vault();
        vault.config.inbox.auto_archive = true;
        service.update(cx, |service, cx| {
            service.set_account_for_test("diego@example.com");
            if view_open {
                service.open_views += 1;
            }
            service.set_vault_for_test(vault, cx);
        });
        cx.run_until_parked();
        (fs, service, requests)
    }

    #[gpui::test]
    async fn an_open_view_does_not_ask_about_what_the_run_asks(cx: &mut TestAppContext) {
        init_test(cx);
        let notes = [
            (
                "2026-11-07-0900-a",
                note("A", "2026-11-07T09:00:00Z", "a2a2a2a2a2a2"),
            ),
            (
                "2026-11-07-0800-b",
                note("B", "2026-11-07T08:00:00Z", "b2b2b2b2b2b2"),
            ),
            (
                "2026-11-07-0700-c",
                note("C", "2026-11-07T07:00:00Z", "c2c2c2c2c2c2"),
            ),
        ];
        let fs = FakeFs::new(cx.executor());
        let (_, service, requests) = auto_archive_service_on(
            cx,
            fs,
            &notes,
            STAMPED_POLICY,
            |_| (200, "unsure", 0.9),
            true,
        )
        .await;
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        service.read_with(cx, |service, _| {
            assert!(service.auto_archive.asking.is_empty());
            for (_, content) in &notes {
                assert!(service.suggestion_for(&note_hash(content)).is_some());
            }
        });
    }

    fn run_log(text: &str) -> RunLog {
        RunLog::parse(text)
    }

    #[gpui::test]
    async fn the_morning_run_files_what_clears_the_bar_once_a_day(cx: &mut TestAppContext) {
        let replied = "---\nsource:   gmail\ncapture:  r0r0r0r0r0r0\ncaptured: 2026-11-07T09:00:00Z\n\
                       title:    Licence renewal\nfrom:     Ana <ana@example.com>\n---\n\n# Licence renewal\n\n\
                       ## Ana <ana@example.com> — 2026-11-07 09:00\n\nRenewing?\n\n\
                       ## Diego Tavares <diego@example.com> — 2026-11-07 10:00\n\nOn it.\n";
        let notes = [
            (
                "2026-11-07-0800-maya",
                note("Maya licence", "2026-11-07T08:00:00Z", "a0a0a0a0a0a0"),
            ),
            (
                "2026-11-07-0700-houdini",
                note("Houdini licence", "2026-11-07T07:00:00Z", "b0b0b0b0b0b0"),
            ),
            (
                "2026-11-07-0600-review",
                note("Review the plan", "2026-11-07T06:00:00Z", "c0c0c0c0c0c0"),
            ),
            ("2026-11-07-0900-renewal", replied.to_string()),
            (
                "2025-12-01-0900-old",
                note("Old licence", "2025-12-01T09:00:00Z", "e0e0e0e0e0e0"),
            ),
        ];
        let (fs, service, requests) =
            auto_archive_service(cx, &notes, STAMPED_POLICY, |title| match title {
                "Maya licence" => (200, "archive", 0.97),
                "Houdini licence" => (200, "archive", 0.94),
                _ => (200, "unsure", 0.99),
            })
            .await;

        // Asked about the three new items the person didn't write in; never
        // about what came before auto-archive was on.
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        assert!(
            fs.is_file(Path::new("/vault/archives/inbox/2026-11-07-0800-maya.md"))
                .await
        );
        for kept in [
            "2026-11-07-0700-houdini",
            "2026-11-07-0600-review",
            "2026-11-07-0900-renewal",
            "2025-12-01-0900-old",
        ] {
            assert!(
                fs.is_file(Path::new(&format!("/vault/inbox/{kept}.md")))
                    .await,
                "{kept}"
            );
        }
        let log = fs
            .load(Path::new(&format!("/vault/{TRIAGE_LOG_PATH}")))
            .await
            .unwrap();
        assert!(
            log.ends_with("· Maya licence → Archived · auto <!--inbox:a0a0a0a0a0a0-->\n"),
            "{log}"
        );
        let decisions = fs
            .load(Path::new("/vault/.thock/state/inbox/decisions.jsonl"))
            .await
            .unwrap();
        let record: DecisionRecord =
            serde_json::from_str(decisions.lines().next().unwrap()).unwrap();
        assert_eq!(record.via, "auto");
        assert_eq!(record.chosen, "archive");
        assert_eq!(record.suggested.as_deref(), Some("archive"));
        assert!(record.run.is_some());

        let text = fs
            .load(Path::new("/vault/.thock/state/inbox/auto-archive.jsonl"))
            .await
            .unwrap();
        let runs = run_log(&text);
        let run = runs.latest().expect("a run");
        assert_eq!(run.candidates, 4);
        assert_eq!(run.archived.len(), 1);
        let mut skipped = run.skipped.clone();
        skipped.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            skipped,
            [
                ("2026-11-07-0600-review".to_string(), SkipReason::Below),
                ("2026-11-07-0700-houdini".to_string(), SkipReason::Below),
                ("2026-11-07-0900-renewal".to_string(), SkipReason::Replied),
            ]
        );
        assert!(
            run.finished
                .as_ref()
                .is_some_and(|finished| finished.stopped.is_none())
        );
        service.read_with(cx, |service, cx| {
            assert_eq!(service.auto_archived().len(), 1);
            assert!(matches!(
                service.auto_archive_status(cx),
                RunStatus::Ran { archived: 1, .. }
            ));
        });

        // Another scan the same day asks nothing.
        service.update(cx, |service, cx| {
            service.auto_archive.triggered = true;
            service.refresh_items(cx);
        });
        cx.run_until_parked();
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        let again = fs
            .load(Path::new("/vault/.thock/state/inbox/auto-archive.jsonl"))
            .await
            .unwrap();
        assert_eq!(run_log(&again).runs.len(), 1);
    }

    #[gpui::test]
    async fn a_402_stops_the_run_and_leaves_the_rest(cx: &mut TestAppContext) {
        let notes = [
            (
                "2026-11-07-0900-a",
                note("A", "2026-11-07T09:00:00Z", "a1a1a1a1a1a1"),
            ),
            (
                "2026-11-07-0800-b",
                note("B", "2026-11-07T08:00:00Z", "b1b1b1b1b1b1"),
            ),
            (
                "2026-11-07-0700-c",
                note("C", "2026-11-07T07:00:00Z", "c1c1c1c1c1c1"),
            ),
        ];
        let (fs, service, _) =
            auto_archive_service(cx, &notes, STAMPED_POLICY, |title| match title {
                "B" => (402, "archive", 0.99),
                _ => (200, "archive", 0.99),
            })
            .await;
        assert!(
            fs.is_file(Path::new("/vault/archives/inbox/2026-11-07-0900-a.md"))
                .await
        );
        assert!(
            fs.is_file(Path::new("/vault/inbox/2026-11-07-0800-b.md"))
                .await
        );
        assert!(
            fs.is_file(Path::new("/vault/inbox/2026-11-07-0700-c.md"))
                .await,
            "untouched"
        );
        let text = fs
            .load(Path::new("/vault/.thock/state/inbox/auto-archive.jsonl"))
            .await
            .unwrap();
        let runs = run_log(&text);
        let run = runs.latest().unwrap();
        assert_eq!(run.recorded(), ["2026-11-07-0900-a".to_string()].into());
        assert!(run.stopped().is_some());
        service.read_with(cx, |service, cx| {
            assert_eq!(
                service.auto_archive_status(cx),
                RunStatus::Stopped {
                    processed: 1,
                    candidates: 3,
                    reason: "Thock Plus allowance spent for this cycle".to_string(),
                    retryable: true,
                }
            );
        });
        // With the allowance spent Retry couldn't work, so it isn't offered.
        cx.update(|cx| plus::set_current_entitlement(Some(entitlement(true)), cx));
        service.read_with(cx, |service, cx| {
            let status = service.auto_archive_status(cx);
            assert!(
                matches!(
                    status,
                    RunStatus::Stopped {
                        retryable: false,
                        ..
                    }
                ),
                "{status:?}"
            );
        });
    }

    #[gpui::test]
    async fn without_a_refreshed_policy_nothing_runs(cx: &mut TestAppContext) {
        let notes = [(
            "2026-11-07-0900-a",
            note("A", "2026-11-07T09:00:00Z", "a2a2a2a2a2a2"),
        )];
        let (fs, service, requests) = auto_archive_service(
            cx,
            &notes,
            include_str!("../assets/routines/inbox/triage-policy.md"),
            |_| (200, "archive", 0.99),
        )
        .await;
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert!(
            fs.is_file(Path::new("/vault/inbox/2026-11-07-0900-a.md"))
                .await
        );
        let text = fs
            .load(Path::new("/vault/.thock/state/inbox/auto-archive.jsonl"))
            .await
            .unwrap();
        assert!(run_log(&text).runs.is_empty(), "{text}");
        service.read_with(cx, |service, cx| {
            assert_eq!(service.auto_archive_status(cx), RunStatus::WaitingForPolicy);
        });
    }

    #[gpui::test]
    async fn turning_off_removes_only_the_keys_it_added(cx: &mut TestAppContext) {
        let (fs, service, _) =
            auto_archive_service(cx, &[], STAMPED_POLICY, |_| (200, "unsure", 0.5)).await;
        fs.insert_file(
            Path::new("/vault/.thock/config.toml"),
            b"schema = 1\n\n[inbox]\nsuggestions = false\nauto_archive = true\nauto_archive_threshold = 0.9\n".to_vec(),
        )
        .await;
        service.update(cx, |service, cx| {
            service.turn_off_auto_archive(WeakEntity::new_invalid(), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new("/vault/.thock/config.toml"))
                .await
                .unwrap(),
            "schema = 1\n\n[inbox]\nsuggestions = false\n"
        );
    }

    #[gpui::test]
    async fn a_poll_triggered_run_waits_for_the_scan_after_the_poll(cx: &mut TestAppContext) {
        let notes = [(
            "2026-11-07-0800-a",
            note("A", "2026-11-07T08:00:00Z", "a3a3a3a3a3a3"),
        )];
        // Not stamped yet: the first scan's trigger runs nothing.
        let (fs, service, requests) = auto_archive_service(
            cx,
            &notes,
            include_str!("../assets/routines/inbox/triage-policy.md"),
            |_| (200, "archive", 0.99),
        )
        .await;
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        service.update(cx, |service, _| service.gmail_polling_for_test = Some(true));
        fs.insert_file(
            Path::new("/vault/routines/inbox/triage-policy.md"),
            STAMPED_POLICY.as_bytes().to_vec(),
        )
        .await;
        cx.run_until_parked();
        assert_eq!(requests.load(Ordering::SeqCst), 0, "no poll yet");

        // The poll lands B; the policy reload finishes before the scan does.
        fs.insert_file(
            Path::new("/vault/inbox/2026-11-07-0900-b.md"),
            note("B", "2026-11-07T09:00:00Z", "b3b3b3b3b3b3").into_bytes(),
        )
        .await;
        service.update(cx, |service, cx| {
            service.gmail_poll_finished(cx);
            service.policy = None;
            service.load_policy(cx);
        });
        cx.run_until_parked();
        assert!(
            fs.is_file(Path::new("/vault/archives/inbox/2026-11-07-0900-b.md"))
                .await
        );
        assert!(
            fs.is_file(Path::new("/vault/archives/inbox/2026-11-07-0800-a.md"))
                .await
        );
        let text = fs
            .load(Path::new("/vault/.thock/state/inbox/auto-archive.jsonl"))
            .await
            .unwrap();
        assert_eq!(run_log(&text).latest().unwrap().candidates, 2, "{text}");
    }

    #[gpui::test]
    async fn a_failed_log_write_puts_the_note_back(cx: &mut TestAppContext) {
        let notes = [(
            "2026-11-07-0800-a",
            note("A", "2026-11-07T08:00:00Z", "a4a4a4a4a4a4"),
        )];
        init_test(cx);
        // A folder where the triage log should be: the log write fails after
        // the move.
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new(&format!("/vault/{TRIAGE_LOG_PATH}")))
            .await
            .unwrap();
        let (fs, _service, _) = auto_archive_service_on(
            cx,
            fs,
            &notes,
            STAMPED_POLICY,
            |_| (200, "archive", 0.99),
            false,
        )
        .await;
        assert!(
            fs.is_file(Path::new("/vault/inbox/2026-11-07-0800-a.md"))
                .await
        );
        assert!(
            !fs.is_file(Path::new("/vault/archives/inbox/2026-11-07-0800-a.md"))
                .await
        );
        let text = fs
            .load(Path::new("/vault/.thock/state/inbox/auto-archive.jsonl"))
            .await
            .unwrap();
        let runs = run_log(&text);
        let run = runs.latest().unwrap();
        assert!(run.archived.is_empty());
        assert_eq!(
            run.skipped,
            [("2026-11-07-0800-a".to_string(), SkipReason::Error)]
        );
        assert!(
            fs.load(Path::new("/vault/.thock/state/inbox/decisions.jsonl"))
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }
}
