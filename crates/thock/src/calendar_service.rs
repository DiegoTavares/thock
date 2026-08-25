//! The calendar sync service (spec `v8-calendar-sync.md` §9–§10): one GPUI
//! entity per local project that polls the provider, reconciles today's note,
//! and applies the edits — through the open buffer as one undoable
//! transaction when the note is open, through the project `Fs` otherwise. It
//! lives independently of the Day Planner panel; the panel only displays the
//! status this service exposes.

use anyhow::{Context as _, Result, anyhow};
use chrono::Local;
use fs::Fs;
use gpui::{
    Action as _, App, AppContext as _, AsyncApp, Context, DismissEvent, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, Global, SharedString, Subscription, Task, WeakEntity,
    Window, actions,
};
use language::Buffer;
use notifications::status_toast::StatusToast;
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

use crate::calendar::{
    CALENDAR_CONFIG_FILE, CalendarConfig, CalendarProvider, Divergence, Fetched, Reconciled,
    apply_line_edits, parse_calendar_config, reconcile,
};
use crate::calendar_google::{self, CalendarListEntry, GoogleProvider};
use crate::gmail::GMAIL_CONFIG_FILE;
use crate::gmail_service::GmailService;
use crate::inbox_service::InboxService;
use crate::google_auth::{
    self, AuthRevoked, GOOGLE_CONFIG_FILE, GoogleClient, resolve_google_settings,
};
use crate::notes::NoteKind;
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

/// How long a burst of typing defers a buffer apply, and for how long at most
/// (spec §9 guard 1).
const TYPING_GUARD_QUIET: Duration = Duration::from_secs(2);
const TYPING_GUARD_MAX_TRIES: usize = 15;
const BACKOFF_CEILING: Duration = Duration::from_secs(60 * 60);

const STATE_DIR: &str = "state/calendar";
const DIVERGENCE_LOG_FILE: &str = "log.jsonl";

actions!(
    thock,
    [
        /// Links your Google account so today's meetings appear in today's
        /// note, emails you label become Backlog tasks, and things you send
        /// from your phone land in the inbox.
        ConnectGoogleWorkspace,
        /// Chooses which of your calendars appear in today's note.
        ChooseCalendars,
        /// Brings today's meetings in the daily note up to date now.
        SyncCalendarNow,
        /// Stops calendar sync, email capture, and inbox capture, and
        /// forgets the Google sign-in. Everything already in your notes
        /// stays where it is.
        DisconnectGoogleWorkspace,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let service = cx.new(|cx| CalendarService::new(project.clone(), cx));
        cx.subscribe(&service, |workspace, _, event: &ManualSyncFinished, cx| {
            show_sync_toast(workspace, event, cx);
        })
        .detach();
        let project_id = project.entity_id();
        cx.default_global::<GlobalCalendarServices>()
            .0
            .insert(project_id, service);
        cx.on_release(move |_, cx| {
            cx.default_global::<GlobalCalendarServices>()
                .0
                .remove(&project_id);
        })
        .detach();

        workspace.register_action(|workspace, _: &ConnectGoogleWorkspace, window, cx| {
            let workspace_handle = workspace.weak_handle();
            let gmail = crate::gmail_service::service_for_project(workspace.project(), cx);
            let inbox = crate::inbox_service::service_for_project(workspace.project(), cx);
            if let Some(service) = service_for_workspace(workspace, cx) {
                if !service.read(cx).has_vault() {
                    show_no_vault_error(workspace, cx);
                    return;
                }
                service.update(cx, |service, cx| {
                    service.connect(gmail, inbox, workspace_handle, window, cx)
                });
            }
        });
        workspace.register_action(|workspace, _: &ChooseCalendars, window, cx| {
            let workspace_handle = workspace.weak_handle();
            if let Some(service) = service_for_workspace(workspace, cx) {
                if !service.read(cx).has_vault() {
                    show_no_vault_error(workspace, cx);
                    return;
                }
                service.update(cx, |service, cx| {
                    service.choose_calendars(workspace_handle, window, cx)
                });
            }
        });
        workspace.register_action(|workspace, _: &SyncCalendarNow, _window, cx| {
            if let Some(service) = service_for_workspace(workspace, cx) {
                service.update(cx, |service, cx| service.sync_now(cx));
            }
        });
        workspace.register_action(|workspace, _: &DisconnectGoogleWorkspace, _window, cx| {
            let gmail = crate::gmail_service::service_for_project(workspace.project(), cx);
            let inbox = crate::inbox_service::service_for_project(workspace.project(), cx);
            if let Some(service) = service_for_workspace(workspace, cx) {
                service.update(cx, |service, cx| service.disconnect(gmail, inbox, cx));
            }
        });
    })
    .detach();
}

#[derive(Default)]
struct GlobalCalendarServices(HashMap<EntityId, Entity<CalendarService>>);

impl Global for GlobalCalendarServices {}

/// The sync service for `project`, if one is running.
pub fn service_for_project(project: &Entity<Project>, cx: &App) -> Option<Entity<CalendarService>> {
    cx.try_global::<GlobalCalendarServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

fn service_for_workspace(workspace: &Workspace, cx: &App) -> Option<Entity<CalendarService>> {
    service_for_project(workspace.project(), cx)
}

fn show_no_vault_error(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    workspace.show_error(
        "This workspace isn't a Thock vault, so there is no calendar to connect.".to_string(),
        cx,
    );
}

/// What the Day Planner's status row shows (spec §10.3).
#[derive(Debug, Clone, PartialEq)]
pub enum SyncState {
    /// No `.thock/calendar.toml` in the vault: the feature stays invisible
    /// (G6). The row is hidden.
    NoConfig,
    /// A config exists but no account is connected.
    NeverConnected,
    Connecting,
    /// Connected, waiting for the first sync of the session.
    Idle,
    Synced {
        at: Instant,
    },
    /// Connected but holding — no daily note yet, no planner heading, no
    /// calendars selected.
    Holding {
        reason: SharedString,
    },
    Failing {
        error: SharedString,
    },
    /// The sign-in expired or was revoked (spec §6.4).
    Disconnected,
}

/// A user-triggered `Sync*Now` finished. Only manual syncs emit this — a
/// background poll announcing itself every few minutes would be noise — and
/// subscribers show it as a status toast, like the git push confirmation.
pub struct ManualSyncFinished {
    pub message: SharedString,
    pub icon: IconName,
}

pub(crate) fn show_sync_toast(
    workspace: &mut Workspace,
    event: &ManualSyncFinished,
    cx: &mut Context<Workspace>,
) {
    let icon = event.icon;
    let status_toast = StatusToast::new(event.message.clone(), cx, move |this, _cx| {
        this.icon(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
    });
    workspace.toggle_status_toast(status_toast, cx);
}

enum SyncOutcome {
    Synced { diverged: Vec<Divergence> },
    Held(SharedString),
    Failed(anyhow::Error),
    AuthRevoked,
    /// The service was reconfigured or released mid-sync.
    Aborted,
}

pub struct CalendarService {
    project: Entity<Project>,
    vault: Option<Vault>,
    config: Option<CalendarConfig>,
    provider: Option<Arc<dyn CalendarProvider>>,
    state: SyncState,
    /// The one poll loop. Replacing it on reload cancels the old loop; the
    /// apply work it spawns is awaited inside it, never stored separately.
    poll_task: Option<Task<()>>,
    connect_task: Option<Task<()>>,
    /// Divergences already written to the log, so a frozen line is recorded
    /// once and not on every poll.
    logged_divergences: HashSet<String>,
    /// Set by `sync_now` so the next completed sync announces itself
    /// ([`ManualSyncFinished`]); background polls stay quiet.
    announce_next_sync: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ManualSyncFinished> for CalendarService {}

impl CalendarService {
    fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, Self::handle_project_event);
        let mut this = Self {
            project,
            vault: None,
            config: None,
            provider: None,
            state: SyncState::NoConfig,
            poll_task: None,
            connect_task: None,
            logged_divergences: HashSet::new(),
            announce_next_sync: false,
            _subscriptions: vec![project_subscription],
        };
        this.reload(cx);
        this
    }

    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// Whether this project is a Thock vault; without one there is nothing
    /// for the calendar actions to act on.
    pub fn has_vault(&self) -> bool {
        self.vault.is_some()
    }

    /// Whether the status row should be shown at all: only when the vault
    /// carries a calendar config (spec §10.3, G6).
    pub fn has_config(&self) -> bool {
        !matches!(self.state, SyncState::NoConfig)
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
                let calendar_config = format!("{VAULT_MARKER_DIR}/{CALENDAR_CONFIG_FILE}");
                let google_config = format!("{VAULT_MARKER_DIR}/{GOOGLE_CONFIG_FILE}");
                let vault_config = format!("{VAULT_MARKER_DIR}/{VAULT_CONFIG_FILE}");
                if changes.iter().any(|(path, _, _)| {
                    let path = path.as_unix_str();
                    path == calendar_config || path == google_config || path == vault_config
                }) {
                    self.reload(cx);
                }
            }
            _ => {}
        }
    }

    /// Re-resolves the vault and `.thock/calendar.toml`, rebuilding the
    /// provider and poll loop when the configuration actually changed. The
    /// section is located by config every sync, so most note edits never come
    /// through here.
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

        let config_path = vault
            .root
            .join(VAULT_MARKER_DIR)
            .join(CALENDAR_CONFIG_FILE);
        // Same synchronous read as `Vault::detect`; the file is tiny.
        let config = match std::fs::read_to_string(&config_path) {
            Err(_) => None,
            Ok(text) => match parse_calendar_config(&text, &vault.config.day_planner.heading) {
                Ok(mut config) => {
                    // The account and client override belong to the
                    // connection, resolved across the Google config files
                    // (V13 §7.4) — `google.toml` first, then this file, then
                    // the rest.
                    let settings = resolve_google_settings(&vault.root, CALENDAR_CONFIG_FILE);
                    config.account = settings.account;
                    config.google = settings.google;
                    Some(config)
                }
                Err(error) => {
                    // A hand-edited file that doesn't parse disables the
                    // syncer, never panics (spec §7.1).
                    log::warn!(
                        "Thock: couldn't parse {}: {error:#}; calendar sync is off",
                        config_path.display()
                    );
                    self.vault = Some(vault);
                    self.clear_sync(SyncState::Failing {
                        error: "calendar.toml could not be read".into(),
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
        self.state = state;
    }

    fn build_provider(&self, cx: &App) -> Result<Arc<dyn CalendarProvider>> {
        let config = self.config.as_ref().context("no calendar config")?;
        let client = GoogleClient::resolve(&config.google)?;
        Ok(Arc::new(GoogleProvider::new(
            cx.http_client(),
            client,
            config.calendars.clone(),
            config.filters.clone(),
        )))
    }

    /// (Re)starts the poll loop: an immediate sync, then one tick per
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
        let announcement = std::mem::take(&mut self.announce_next_sync)
            .then(|| match &outcome {
                SyncOutcome::Aborted => None,
                SyncOutcome::Synced { .. } => Some("Calendar synced".into()),
                SyncOutcome::Held(reason) => {
                    Some(format!("Calendar sync held — {reason}").into())
                }
                SyncOutcome::Failed(error) => {
                    Some(format!("Calendar sync failed — {error:#}").into())
                }
                SyncOutcome::AuthRevoked => {
                    Some("Google sign-in expired — reconnect to sync your calendar".into())
                }
            })
            .flatten();
        let keep_going = match outcome {
            SyncOutcome::Aborted => false,
            SyncOutcome::Synced { diverged } => {
                self.state = SyncState::Synced { at: Instant::now() };
                *delay = interval;
                self.log_divergences(diverged, cx);
                true
            }
            SyncOutcome::Held(reason) => {
                self.state = SyncState::Holding { reason };
                *delay = interval;
                true
            }
            SyncOutcome::Failed(error) => {
                log::warn!("Thock calendar sync failed: {error:#}");
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
        if let Some(message) = announcement {
            cx.emit(ManualSyncFinished {
                message,
                icon: IconName::Clock,
            });
        }
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
        if config.calendars.is_empty() {
            return SyncOutcome::Held("no calendars selected".into());
        }

        // Recomputed every tick, so an app left open follows midnight to the
        // new note (§10.2).
        let date = Local::now().date_naive();
        let note_path = vault.note_path(NoteKind::Daily, date);
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let buffer = project.update(cx, |project, cx| {
            project
                .project_path_for_absolute_path(&note_path, cx)
                .and_then(|path| project.get_open_buffer(&path, cx))
        });

        // Existence guard (§9 guard 2): sync never creates the daily note.
        if buffer.is_none() && !fs.is_file(&note_path).await {
            return SyncOutcome::Held("waiting for today's note".into());
        }

        let events = match provider.fetch_day(date, cx).await {
            Err(error) if error.is::<AuthRevoked>() => return SyncOutcome::AuthRevoked,
            Err(error) => return SyncOutcome::Failed(error),
            // Every calendar answered 304: nothing to do (§10.2).
            Ok(Fetched::Unchanged) => return SyncOutcome::Synced { diverged: Vec::new() },
            Ok(Fetched::Events(events)) => events,
        };

        match buffer {
            Some(buffer) => Self::apply_via_buffer(buffer, &events, &config, cx).await,
            None => Self::apply_via_fs(fs, &note_path, &events, &config).await,
        }
    }

    /// The not-open path (§9): read-modify-write through the project `Fs`,
    /// re-reading the file after the fetch.
    async fn apply_via_fs(
        fs: Arc<dyn Fs>,
        note_path: &Path,
        events: &[crate::calendar::CalendarEvent],
        config: &CalendarConfig,
    ) -> SyncOutcome {
        let text = match fs.load(note_path).await {
            Ok(text) => text,
            Err(error) => return SyncOutcome::Failed(error),
        };
        match reconcile(&text, events, config) {
            Reconciled::NoPlannerSection => {
                SyncOutcome::Held("no planner heading in today's note".into())
            }
            Reconciled::Edits { edits, diverged } => {
                if !edits.is_empty()
                    && let Err(error) = fs
                        .atomic_write(note_path.to_path_buf(), apply_line_edits(&text, &edits))
                        .await
                {
                    return SyncOutcome::Failed(error);
                }
                SyncOutcome::Synced { diverged }
            }
        }
    }

    /// The open-buffer path (§9): waits for a quiet window (typing guard),
    /// then applies the reconciliation as a minimal diff in one finalized
    /// transaction — undoable with one `u`, and it cannot clobber unsaved
    /// keystrokes because it edits the live buffer.
    async fn apply_via_buffer(
        buffer: Entity<Buffer>,
        events: &[crate::calendar::CalendarEvent],
        config: &CalendarConfig,
        cx: &mut AsyncApp,
    ) -> SyncOutcome {
        for _ in 0..TYPING_GUARD_MAX_TRIES {
            let version = buffer.read_with(cx, |buffer, _| buffer.version());
            cx.background_executor().timer(TYPING_GUARD_QUIET).await;
            if buffer.read_with(cx, |buffer, _| buffer.version() == version) {
                break;
            }
        }

        // The buffer can change between computing the diff and applying it;
        // `apply_diff` refuses stale diffs, so just recompute.
        for _ in 0..3 {
            let text = buffer.read_with(cx, |buffer, _| buffer.text());
            let (edits, diverged) = match reconcile(&text, events, config) {
                Reconciled::NoPlannerSection => {
                    return SyncOutcome::Held("no planner heading in today's note".into());
                }
                Reconciled::Edits { edits, diverged } => (edits, diverged),
            };
            if edits.is_empty() {
                return SyncOutcome::Synced { diverged };
            }
            let new_text = apply_line_edits(&text, &edits);
            let diff = buffer
                .read_with(cx, |buffer, cx| buffer.diff(new_text, cx))
                .await;
            let applied = buffer.update(cx, |buffer, cx| {
                buffer.start_transaction();
                let applied = buffer.apply_diff(diff, cx).is_some();
                buffer.end_transaction(cx);
                // Not grouped with the user's own edit history entry (§9).
                buffer.finalize_last_transaction();
                applied
            });
            if applied {
                return SyncOutcome::Synced { diverged };
            }
        }
        SyncOutcome::Failed(anyhow!("the buffer kept changing while applying calendar edits"))
    }

    /// Records newly-diverged lines in `.thock/state/calendar/log.jsonl`
    /// (spec §8.4) so the reason a line froze is inspectable.
    fn log_divergences(&mut self, diverged: Vec<Divergence>, cx: &mut Context<Self>) {
        let fresh: Vec<Divergence> = diverged
            .into_iter()
            .filter(|divergence| self.logged_divergences.insert(divergence.id.clone()))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let Some(vault) = &self.vault else {
            return;
        };
        let log_path = vault
            .root
            .join(VAULT_MARKER_DIR)
            .join(STATE_DIR)
            .join(DIVERGENCE_LOG_FILE);
        let fs = self.project.read(cx).fs().clone();
        cx.background_spawn(async move {
            let mut contents = fs.load(&log_path).await.unwrap_or_default();
            for divergence in fresh {
                let entry = serde_json::json!({
                    "at": Local::now().to_rfc3339(),
                    "reason": "title-diverged",
                    "id": divergence.id,
                    "event_title": divergence.event_title,
                    "line": divergence.line,
                });
                contents.push_str(&entry.to_string());
                contents.push('\n');
            }
            if let Some(parent) = log_path.parent() {
                fs.create_dir(parent).await?;
            }
            fs.atomic_write(log_path, contents).await
        })
        .detach_and_log_err(cx);
    }

    /// `thock::ConnectGoogleWorkspace` (V9 §6.1; V13 §7.3 adds Tasks): one
    /// OAuth round in the system browser granting Calendar + Gmail + Tasks,
    /// then the calendar picker. Writes the account into `.thock/google.toml`
    /// and reloads the Gmail and Inbox services alongside this one.
    fn connect(
        &mut self,
        gmail: Option<Entity<GmailService>>,
        inbox: Option<Entity<InboxService>>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(vault) = &self.vault else {
            return;
        };
        let vault_root = vault.root.clone();
        let overrides = resolve_google_settings(&vault_root, CALENDAR_CONFIG_FILE).google;
        let existing_calendars = self
            .config
            .as_ref()
            .map(|config| config.calendars.clone())
            .unwrap_or_default();
        let http = cx.http_client();
        let fs = self.project.read(cx).fs().clone();
        self.state = SyncState::Connecting;
        cx.notify();
        if let Some(gmail) = &gmail {
            gmail.update(cx, |gmail, cx| gmail.mark_connecting(cx));
        }
        if let Some(inbox) = &inbox {
            inbox.update(cx, |inbox, cx| inbox.mark_connecting(cx));
        }

        self.connect_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let client = GoogleClient::resolve(&overrides)?;
                let connected = google_auth::connect_workspace(http, client, cx).await?;
                let email = connected.email.clone();
                let selected = if existing_calendars.is_empty() {
                    vec![email.clone()]
                } else {
                    existing_calendars
                };
                // The account belongs to the connection, so it lands in
                // `google.toml` and nowhere else (V13 §7.4) — the connect
                // flow stopped writing the per-feature duplicates.
                update_config_file(&fs, &vault_root, GOOGLE_CONFIG_FILE, move |table| {
                    table.insert("schema".into(), 1.into());
                    table.insert("account".into(), email.into());
                })
                .await?;
                update_config_file(&fs, &vault_root, CALENDAR_CONFIG_FILE, move |table| {
                    table.insert("schema".into(), 1.into());
                    if !table.contains_key("calendars") {
                        table.insert(
                            "calendars".into(),
                            toml::Value::Array(
                                selected.into_iter().map(toml::Value::String).collect(),
                            ),
                        );
                    }
                })
                .await?;
                // Connecting the Workspace arms email capture with its
                // defaults; an absent Gmail label costs nothing (spec v9 §12
                // Q2), and deleting gmail.toml turns the feature back off.
                update_config_file(&fs, &vault_root, GMAIL_CONFIG_FILE, move |table| {
                    table.insert("schema".into(), 1.into());
                })
                .await?;
                // Same for inbox capture (V13 §10.2): the file's existence
                // arms the Google Tasks and thock/inbox transports, and
                // deleting it turns them off while inbox/ stays a folder.
                update_config_file(&fs, &vault_root, crate::inbox::INBOX_CONFIG_FILE, |table| {
                    table.insert("schema".into(), 1.into());
                })
                .await?;
                anyhow::Ok(connected)
            }
            .await;

            match result {
                Ok(connected) => {
                    this.update_in(cx, |service, window, cx| {
                        service.reload(cx);
                        service.open_picker(workspace, connected.calendars, true, window, cx);
                    })
                    .log_err();
                    if let Some(gmail) = &gmail {
                        gmail.update(cx, |gmail, cx| gmail.reload_config(cx));
                    }
                    if let Some(inbox) = &inbox {
                        inbox.update(cx, |inbox, cx| inbox.reload_config(cx));
                    }
                }
                Err(error) => {
                    log::warn!("Thock: connecting Google Workspace failed: {error:#}");
                    this.update(cx, |service, cx| {
                        service.state = SyncState::Failing {
                            error: format!("{error:#}").into(),
                        };
                        cx.notify();
                    })
                    .log_err();
                    if let Some(gmail) = &gmail {
                        gmail.update(cx, |gmail, cx| gmail.reload_config(cx));
                    }
                    if let Some(inbox) = &inbox {
                        inbox.update(cx, |inbox, cx| inbox.reload_config(cx));
                    }
                }
            }
        }));
    }

    /// `thock::ChooseCalendars` (spec §6.3): reopens the picker without
    /// re-authenticating.
    fn choose_calendars(
        &mut self,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vault.is_none() {
            return;
        }
        let overrides = self
            .config
            .as_ref()
            .map(|config| config.google.clone())
            .unwrap_or_default();
        let http = cx.http_client();
        self.connect_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let client = GoogleClient::resolve(&overrides)?;
                let (_, refresh_token) = google_auth::read_refresh_token_allowing_legacy(cx)
                    .await?
                    .context(
                        "no Google account is connected yet — run “connect google workspace” first",
                    )?;
                let tokens =
                    google_auth::refresh_access_token(&http, &client, &refresh_token).await?;
                calendar_google::list_calendars(&http, &tokens.access_token).await
            }
            .await;

            match result {
                Ok(calendars) => {
                    this.update_in(cx, |service, window, cx| {
                        service.open_picker(workspace, calendars, false, window, cx);
                    })
                    .log_err();
                }
                Err(error) => {
                    log::warn!("Thock: listing calendars failed: {error:#}");
                    this.update(cx, |service, cx| {
                        service.state = if error.is::<AuthRevoked>() {
                            SyncState::Disconnected
                        } else {
                            SyncState::Failing {
                                error: format!("{error:#}").into(),
                            }
                        };
                        cx.notify();
                    })
                    .log_err();
                }
            }
        }));
    }

    fn open_picker(
        &mut self,
        workspace: WeakEntity<Workspace>,
        entries: Vec<CalendarListEntry>,
        offer_email_import: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(vault) = &self.vault else {
            return;
        };
        let vault_root = vault.root.clone();
        let selected: HashSet<String> = self
            .config
            .as_ref()
            .map(|config| config.calendars.iter().cloned().collect())
            .unwrap_or_default();
        let fs = self.project.read(cx).fs().clone();
        let service = cx.weak_entity();
        workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    let delegate = CalendarPickerDelegate {
                        picker_entity: cx.entity().downgrade(),
                        service,
                        fs,
                        vault_root,
                        matches: (0..entries.len()).collect(),
                        entries,
                        selected,
                        selected_index: 0,
                        offer_email_import,
                    };
                    CalendarPicker::new(delegate, window, cx)
                });
            })
            .log_err();
    }

    /// `thock::SyncCalendarNow`: restarts the loop, which syncs immediately.
    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.provider.is_some() {
            self.announce_next_sync = true;
            self.start_poll(cx);
        }
    }

    /// `thock::DisconnectGoogleWorkspace` (spec §6.4): deletes the keychain
    /// entries and stops calendar sync, email capture, and inbox capture.
    /// Never touches the vault.
    fn disconnect(
        &mut self,
        gmail: Option<Entity<GmailService>>,
        inbox: Option<Entity<InboxService>>,
        cx: &mut Context<Self>,
    ) {
        self.poll_task = None;
        self.provider = None;
        if self.config.is_some() {
            self.state = SyncState::NeverConnected;
        }
        cx.notify();
        if let Some(gmail) = gmail {
            gmail.update(cx, |gmail, cx| gmail.mark_signed_out(cx));
        }
        if let Some(inbox) = inbox {
            inbox.update(cx, |inbox, cx| inbox.mark_signed_out(cx));
        }
        cx.spawn(async move |_, cx| google_auth::delete_refresh_token(cx).await)
            .detach_and_log_err(cx);
    }

    #[cfg(test)]
    fn configure_for_test(
        &mut self,
        vault: Vault,
        config: CalendarConfig,
        provider: Arc<dyn CalendarProvider>,
        cx: &mut Context<Self>,
    ) {
        self.vault = Some(vault);
        self.config = Some(config);
        self.provider = Some(provider);
        self.state = SyncState::Idle;
        self.start_poll(cx);
    }
}

/// Rewrites a `.thock/<file_name>` TOML with `mutate` applied to its
/// top-level table, preserving any fields this build doesn't know about.
/// Comments are not preserved (same trade-off as the vault config rewrites).
pub(crate) async fn update_config_file(
    fs: &Arc<dyn Fs>,
    vault_root: &Path,
    file_name: &str,
    mutate: impl FnOnce(&mut toml::Table),
) -> Result<()> {
    let path = vault_root.join(VAULT_MARKER_DIR).join(file_name);
    let existing = fs.load(&path).await.unwrap_or_default();
    let mut table: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parsing {}", path.display()))?;
    mutate(&mut table);
    let serialized = toml::to_string_pretty(&table).context("serializing calendar.toml")?;
    fs.atomic_write(path, serialized).await
}

pub struct CalendarPicker {
    picker: Entity<Picker<CalendarPickerDelegate>>,
}

impl CalendarPicker {
    fn new(delegate: CalendarPickerDelegate, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl ModalView for CalendarPicker {}
impl EventEmitter<DismissEvent> for CalendarPicker {}

impl Focusable for CalendarPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for CalendarPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("CalendarPicker")
            .w(rems(34.))
            .child(self.picker.clone())
    }
}

/// The calendar picker (spec §6.3): every calendar from `calendarList.list`,
/// enter toggles the selection, escape closes and saves it to
/// `.thock/calendar.toml`.
pub struct CalendarPickerDelegate {
    picker_entity: WeakEntity<CalendarPicker>,
    service: WeakEntity<CalendarService>,
    fs: Arc<dyn Fs>,
    vault_root: PathBuf,
    entries: Vec<CalendarListEntry>,
    matches: Vec<usize>,
    selected: HashSet<String>,
    selected_index: usize,
    /// Set by the connect flow (spec v9 §6.1): after this picker saves, the
    /// email import-mode picker opens, so the capture style is a deliberate
    /// choice instead of an invisible default.
    offer_email_import: bool,
}

impl PickerDelegate for CalendarPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "choose calendars"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Toggle calendars with enter; escape saves…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
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
        self.selected_index = index;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.to_lowercase();
        self.matches = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                query.is_empty()
                    || entry.summary.to_lowercase().contains(&query)
                    || entry.id.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected_index = self
            .selected_index
            .min(self.matches.len().saturating_sub(1));
        cx.notify();
        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self
            .matches
            .get(self.selected_index)
            .and_then(|&index| self.entries.get(index))
        else {
            return;
        };
        if !self.selected.remove(&entry.id) {
            self.selected.insert(entry.id.clone());
        }
        cx.notify();
    }

    fn dismissed(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if self.offer_email_import {
            // Deferred internally, so it runs after this modal is gone.
            window.dispatch_action(
                crate::gmail_service::ChooseEmailImportMode.boxed_clone(),
                cx,
            );
        }
        let calendars: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| self.selected.contains(&entry.id))
            .map(|entry| entry.id.clone())
            .collect();
        let fs = self.fs.clone();
        let vault_root = self.vault_root.clone();
        let service = self.service.clone();
        cx.spawn(async move |_, cx| {
            update_config_file(&fs, &vault_root, CALENDAR_CONFIG_FILE, move |table| {
                table.insert(
                    "calendars".into(),
                    toml::Value::Array(calendars.into_iter().map(toml::Value::String).collect()),
                );
            })
            .await?;
            service.update(cx, |service, cx| service.reload(cx))
        })
        .detach_and_log_err(cx);
        self.picker_entity
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = self.entries.get(*self.matches.get(index)?)?;
        let chosen = self.selected.contains(&entry.id);
        let name = if entry.summary.is_empty() {
            entry.id.clone()
        } else {
            entry.summary.clone()
        };
        let mut item = ListItem::new(index)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .start_slot(
                Icon::new(if chosen {
                    IconName::Check
                } else {
                    IconName::Circle
                })
                .size(IconSize::Small)
                .color(if chosen { Color::Accent } else { Color::Muted }),
            )
            .child(Label::new(name));
        if entry.primary {
            item = item.end_slot(Label::new("Primary").size(LabelSize::XSmall).color(Color::Muted));
        }
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::CalendarEvent;
    use crate::vault::VaultConfig;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use settings::SettingsStore;
    use std::sync::Mutex;

    struct StubProvider {
        events: Mutex<Vec<CalendarEvent>>,
    }

    impl CalendarProvider for StubProvider {
        fn fetch_day(
            &self,
            _date: chrono::NaiveDate,
            _cx: &AsyncApp,
        ) -> Task<Result<Fetched>> {
            let events = self.events.lock().unwrap().clone();
            Task::ready(Ok(Fetched::Events(events)))
        }
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    #[gpui::test]
    async fn full_sync_into_a_temp_vault(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let today = Local::now().date_naive();
        let note_path = PathBuf::from(format!("/vault/daily/{}.md", today.format("%Y-%m-%d")));
        fs.create_dir(Path::new("/vault/daily")).await.unwrap();
        fs.insert_file(
            &note_path,
            b"# Monday\n\n## Day planner\n\n- [ ] Workout\n".to_vec(),
        )
        .await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let service = cx.new(|cx| CalendarService::new(project.clone(), cx));
        let provider = Arc::new(StubProvider {
            events: Mutex::new(vec![CalendarEvent {
                id: "aaaaaaaaaaaa".to_string(),
                title: "Standup".to_string(),
                time: Some((600, 630)),
            }]),
        });
        let vault = Vault {
            root: PathBuf::from("/vault"),
            config: VaultConfig::default(),
        };
        let mut config = CalendarConfig::with_planner_heading("Day planner");
        config.account = Some("diego@example.com".to_string());
        config.calendars = vec!["primary".to_string()];
        service.update(cx, |service, cx| {
            service.configure_for_test(vault, config, provider.clone(), cx)
        });
        cx.run_until_parked();

        // The first sync created the section inside the planner section and
        // inserted the meeting.
        let text = fs.load(&note_path).await.unwrap();
        assert_eq!(
            text,
            "# Monday\n\n## Day planner\n\n- [ ] Workout\n\n### Calendar\n\n\
             - [ ] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n"
        );
        service.read_with(cx, |service, _| {
            assert!(
                matches!(service.state(), SyncState::Synced { .. }),
                "unexpected state {:?}",
                service.state()
            );
        });

        // The user ticks the meeting off; the meeting also moves. The next
        // poll corrects the time and keeps the checkmark (G2).
        let ticked = text.replace(
            "- [ ] 10:00 - 10:30 Standup",
            "- [x] 10:00 - 10:30 Standup",
        );
        fs.atomic_write(note_path.clone(), ticked).await.unwrap();
        *provider.events.lock().unwrap() = vec![CalendarEvent {
            id: "aaaaaaaaaaaa".to_string(),
            title: "Standup".to_string(),
            time: Some((660, 690)),
        }];
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();

        let text = fs.load(&note_path).await.unwrap();
        assert!(
            text.contains("- [x] 11:00 - 11:30 Standup <!--gcal:aaaaaaaaaaaa-->"),
            "checkbox lost or time not corrected:\n{text}"
        );

        // A vanished event is marked, never deleted (spec §8.3).
        provider.events.lock().unwrap().clear();
        cx.executor().advance_clock(Duration::from_secs(301));
        cx.run_until_parked();
        let text = fs.load(&note_path).await.unwrap();
        assert!(
            text.contains("- [x] 11:00 - 11:30 ~~Standup~~ (cancelled) <!--gcal:aaaaaaaaaaaa-->"),
            "cancellation not marked:\n{text}"
        );
    }

    #[gpui::test]
    async fn sync_edits_an_open_note_through_its_buffer(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let today = Local::now().date_naive();
        let note_path = PathBuf::from(format!("/vault/daily/{}.md", today.format("%Y-%m-%d")));
        let original = "# Monday\n\n## Day planner\n\n- [ ] Workout\n";
        fs.create_dir(Path::new("/vault/daily")).await.unwrap();
        fs.insert_file(&note_path, original.as_bytes().to_vec()).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();

        let buffer = project
            .update(cx, |project, cx| project.open_local_buffer(&note_path, cx))
            .await
            .unwrap();

        let service = cx.new(|cx| CalendarService::new(project.clone(), cx));
        let provider = Arc::new(StubProvider {
            events: Mutex::new(vec![CalendarEvent {
                id: "aaaaaaaaaaaa".to_string(),
                title: "Standup".to_string(),
                time: Some((600, 630)),
            }]),
        });
        let vault = Vault {
            root: PathBuf::from("/vault"),
            config: VaultConfig::default(),
        };
        let mut config = CalendarConfig::with_planner_heading("Day planner");
        config.account = Some("diego@example.com".to_string());
        config.calendars = vec!["primary".to_string()];
        service.update(cx, |service, cx| {
            service.configure_for_test(vault, config, provider.clone(), cx)
        });
        cx.run_until_parked();
        // The buffer path waits out the typing guard before applying.
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();

        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert_eq!(
            text,
            "# Monday\n\n## Day planner\n\n- [ ] Workout\n\n### Calendar\n\n\
             - [ ] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n"
        );
        // The service edited the live buffer, not the file; autosave persists
        // the change later, so the buffer is dirty and the disk untouched.
        assert_eq!(fs.load(&note_path).await.unwrap(), original);
        buffer.read_with(cx, |buffer, _| assert!(buffer.is_dirty()));
        service.read_with(cx, |service, _| {
            assert!(
                matches!(service.state(), SyncState::Synced { .. }),
                "unexpected state {:?}",
                service.state()
            );
        });
    }
}
