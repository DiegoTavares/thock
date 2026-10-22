//! The reminders index service (spec `thock/specs/v45-reminders.md` §6.1,
//! §6.2): one entity per vault, shared by every window on it. It follows
//! file events, keeps `reminders.md` in step with the notes through the pure
//! batch step in `reminder_index.rs`, and is the one place desk readers get
//! reminders from and desk writers edit them through.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use chrono::{NaiveDate, NaiveDateTime};
use fs::Fs;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, Global, SharedString,
    Subscription, Task, TaskExt as _, WeakEntity, actions,
};
use language::{Buffer, Point};
use project::Project;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use thock_sync_core::{ReminderConfig, id_from_bytes};
use util::ResultExt as _;
use workspace::Workspace;

use crate::reminder_day::ReminderDaySync;
use crate::reminder_index::{
    BatchInput, BatchState, Change, HomeEdit, IndexedReminder, ScanLine, apply_changes,
    insert_loose_line, plan_batch, read_index, scan_note,
};
use crate::reminder_notifier::ReminderNotifier;
use crate::reminders::{
    DayChanged, REMINDERS_CONFIG_FILE, ReminderClock, ReminderDay, ReminderSettings,
    is_reminder_note, load_reminder_settings,
};
use crate::vault::{VAULT_CONFIG_FILE, VAULT_MARKER_DIR, Vault, VaultStatus};

actions!(
    thock,
    [
        /// Rebuild the reminders index from your notes.
        RebuildReminders,
        /// Try again to save the reminder changes that failed.
        RetryReminders,
    ]
);

/// File events are gathered this long before a batch runs.
const BATCH_DELAY: Duration = Duration::from_millis(500);
/// A note open in an editor is processed only once it has been this quiet.
const TYPING_QUIET: Duration = Duration::from_secs(2);
/// How many batches may follow each other on the service's own writes before
/// it stops and waits for the next event; the step settles in two or three.
const MAX_SETTLE_ROUNDS: usize = 8;
const CACHE_DIR: &str = "cache/reminders";
const SCAN_FILE: &str = "scan.json";
/// `scan.json`'s format; a cache of another version is ignored and rebuilt.
const SCAN_VERSION: u32 = 2;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        if !project.read(cx).is_local() {
            return;
        }
        let workspace_handle = workspace.weak_handle();
        attach_workspace_project(&project, workspace_handle.clone(), cx);
        cx.subscribe(&project, move |_, project, event, cx| {
            if matches!(event, project::Event::WorktreeAdded(_)) {
                attach_workspace_project(&project, workspace_handle.clone(), cx);
            }
        })
        .detach();
        let project_id = project.entity_id();
        cx.on_release(move |_, cx| {
            cx.default_global::<GlobalReminderServices>()
                .by_project
                .remove(&project_id);
        })
        .detach();

        workspace.register_action(|workspace, _: &RebuildReminders, _window, cx| {
            match service_for_project(workspace.project(), cx) {
                Some(service) => service.update(cx, |service, cx| service.rebuild(cx)),
                None => workspace.show_error(
                    "This workspace isn't a Thock vault, so it has no reminders.".to_string(),
                    cx,
                ),
            }
        });
        workspace.register_action(|workspace, _: &RetryReminders, _window, cx| {
            if let Some(service) = service_for_project(workspace.project(), cx) {
                service.update(cx, |service, cx| service.retry(cx));
            }
        });
    })
    .detach();
}

fn attach_workspace_project(
    project: &Entity<Project>,
    workspace: WeakEntity<Workspace>,
    cx: &mut App,
) {
    if service_for_project(project, cx).is_some() {
        return;
    }
    let vault = project
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .and_then(|root| match Vault::detect(&root) {
            VaultStatus::Valid(vault) => Some(vault),
            _ => None,
        });
    if let Some(vault) = vault {
        ReminderService::attach(vault, project, Some(workspace), cx);
    }
}

#[derive(Default)]
struct GlobalReminderServices {
    /// The services by canonical vault root, so two windows share one.
    by_vault: HashMap<PathBuf, WeakEntity<ReminderService>>,
    /// The strong handles, one per project on the vault.
    by_project: HashMap<EntityId, Entity<ReminderService>>,
}

impl Global for GlobalReminderServices {}

/// Forgets every service, as closing every window would.
#[cfg(test)]
pub(crate) fn clear_services_for_test(cx: &mut App) {
    cx.default_global::<GlobalReminderServices>()
        .by_project
        .clear();
}

/// The reminders service of the vault `project` is on, if it is one.
pub fn service_for_project(project: &Entity<Project>, cx: &App) -> Option<Entity<ReminderService>> {
    cx.try_global::<GlobalReminderServices>()?
        .by_project
        .get(&project.entity_id())
        .cloned()
}

/// Calls `apply` each time `.thock/reminders.toml` changes, and returns the
/// vault's reminder settings as its service has read them so far. Returns
/// `None` and subscribes to nothing when `project` has no service yet, or the
/// service hasn't read the file.
pub fn observe_settings<T: 'static>(
    project: &Entity<Project>,
    cx: &mut Context<T>,
    apply: impl Fn(&mut T, &ReminderSettings, &mut Context<T>) + 'static,
) -> Option<ReminderSettings> {
    let service = service_for_project(project, cx)?;
    cx.subscribe(&service, move |this, service, event, cx| {
        if *event == ReminderEvent::SettingsChanged {
            let settings = service.read(cx).settings().clone();
            apply(this, &settings, cx);
        }
    })
    .detach();
    let service = service.read(cx);
    service.settings_loaded.then(|| service.settings().clone())
}

/// Whether the last writes went through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderStatus {
    Idle,
    /// A write failed; `RetryReminders` runs the batch again.
    Failing {
        error: SharedString,
    },
}

/// What desk readers listen for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderEvent {
    /// The reminders read from the index changed: a new one, a new state, a
    /// new due or text. Read them again with `reminders()`.
    IndexChanged,
    /// A daily or other in-scope note appeared, by a file event or through
    /// `note_created`. The path is vault-relative.
    NoteCreated { path: String },
    /// An in-scope note other than the index changed on disk. The path is
    /// vault-relative.
    NoteChanged { path: String },
    /// The local date moved.
    DayChanged { today: NaiveDate },
    /// `.thock/reminders.toml` changed and `settings()` holds the new values.
    SettingsChanged,
    /// Something asked to show the Reminders tab: the notifier's
    /// *Open Reminders*, or a click on a notification's body.
    ShowReminders,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ScanEntry {
    /// The file's modification time when it was read, as seconds and nanos.
    mtime: Option<(u64, u32)>,
    lines: Vec<ScanLine>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScanCache {
    version: u32,
    files: BTreeMap<String, ScanEntry>,
}

pub struct ReminderService {
    vault: Vault,
    settings: ReminderSettings,
    settings_loaded: bool,
    /// Why `.thock/reminders.toml` last failed to load, until it loads.
    settings_error: Option<String>,
    projects: Vec<WeakEntity<Project>>,
    workspaces: Vec<WeakEntity<Workspace>>,
    scan: BTreeMap<String, ScanEntry>,
    /// Whether `scan` was filled from `scan.json` or a full scan yet.
    scan_loaded: bool,
    batch_state: BatchState,
    /// Notes to re-read in the next batch, vault-relative.
    dirty: BTreeSet<String>,
    /// Notes the worktree reported without saying they changed (its first
    /// scan): re-read only when their mtime differs from the scan's.
    unverified: BTreeSet<String>,
    /// What the last batch wrote: their file events don't restart the
    /// settle count, or the service's own writes could keep it going.
    recent_writes: HashSet<String>,
    /// Day sections the reconciler just wrote, as the scan should see them,
    /// so the next batch never takes the reconciler's own ticks for a
    /// person's tick on a copy.
    reconciled: BTreeMap<String, Vec<ScanLine>>,
    /// The linked notes the last batch read, by mtime, so an unchanged one
    /// isn't read again.
    home_texts: HashMap<String, CachedText>,
    /// Home-line edits whose write failed, applied again on `retry`.
    failed_edits: Vec<FailedEdit>,
    /// The last home-line edit that had nothing to change, until the next
    /// edit goes through or `retry`.
    edit_error: Option<SharedString>,
    /// The last `write_note` that failed, until `retry`.
    write_error: Option<SharedString>,
    /// The last batch's first error.
    batch_error: Option<SharedString>,
    /// Walk the vault and re-read every note whose mtime changed (startup).
    full_scan: bool,
    /// Ignore `scan.json` and re-read every note (`RebuildReminders`).
    rebuild: bool,
    /// Run the batch step even with nothing dirty (a grace ended, the day
    /// changed).
    needs_plan: bool,
    settle_rounds: usize,
    running: bool,
    /// Whether a batch has finished, so `reminders` reflects the index.
    loaded: bool,
    reminders: Vec<IndexedReminder>,
    notifier: Option<Entity<ReminderNotifier>>,
    day_sync: Option<Entity<ReminderDaySync>>,
    status: ReminderStatus,
    batch_task: Option<Task<()>>,
    recheck_task: Option<Task<()>>,
    /// Re-reads notes whose new token waits for a cursor to leave its line.
    cursor_task: Option<Task<()>>,
    /// Notes waiting on `cursor_task`, gathered across batches.
    at_cursor: BTreeSet<String>,
    cursor_wait: bool,
    /// Notes read by batches, for the startup test.
    #[cfg(test)]
    reads: usize,
    /// Linked notes read from disk to read the index, for the cache test.
    #[cfg(test)]
    home_loads: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ReminderEvent> for ReminderService {}

#[derive(Debug, Clone)]
struct FailedEdit {
    id: String,
    edit: HomeEdit,
    error: SharedString,
}

#[derive(Debug, Clone)]
struct CachedText {
    mtime: (u64, u32),
    text: String,
}

/// One batch's inputs, taken from the service.
struct Job {
    fs: Arc<dyn Fs>,
    vault: Vault,
    /// The last settings that parsed (the defaults before any did).
    settings: ReminderSettings,
    config_needed: bool,
    projects: Vec<Entity<Project>>,
    workspaces: Vec<Entity<Workspace>>,
    dirty: BTreeSet<String>,
    unverified: BTreeSet<String>,
    full_scan: bool,
    rebuild: bool,
    scan: BTreeMap<String, ScanEntry>,
    scan_loaded: bool,
    home_texts: HashMap<String, CachedText>,
    state: BatchState,
    now: NaiveDateTime,
}

struct JobResult {
    settings: Option<Result<ReminderSettings>>,
    scan: BTreeMap<String, ScanEntry>,
    state: BatchState,
    /// Paths the batch wrote, to re-read next.
    written: Vec<String>,
    /// Paths deferred because their buffer was being typed in.
    deferred: Vec<String>,
    /// Paths with a token that got no id because a cursor sits on its line.
    at_cursor: Vec<String>,
    home_texts: HashMap<String, CachedText>,
    errors: Vec<String>,
    recheck_at: Option<NaiveDateTime>,
    /// `None` when the index couldn't be read: keep the last reading.
    reminders: Option<Vec<IndexedReminder>>,
    #[cfg(test)]
    reads: usize,
    #[cfg(test)]
    home_loads: usize,
}

impl ReminderService {
    /// The vault's service, created on first use and shared by every project
    /// on the same vault root. `workspace` lets the service see editors'
    /// cursors.
    pub fn attach(
        vault: Vault,
        project: &Entity<Project>,
        workspace: Option<WeakEntity<Workspace>>,
        cx: &mut App,
    ) -> Entity<Self> {
        let key = std::fs::canonicalize(&vault.root).unwrap_or_else(|_| vault.root.clone());
        let existing = cx
            .default_global::<GlobalReminderServices>()
            .by_vault
            .get(&key)
            .and_then(WeakEntity::upgrade);
        let service = match existing {
            Some(service) => {
                service.update(cx, |service, cx| {
                    service.add_project(project, workspace, cx)
                });
                service
            }
            None => {
                let service = cx.new(|cx| {
                    let mut service = Self::new(vault, cx);
                    service.add_project(project, workspace, cx);
                    service
                });
                cx.default_global::<GlobalReminderServices>()
                    .by_vault
                    .insert(key, service.downgrade());
                let notifier = ReminderNotifier::new(&service, cx);
                let day_sync = ReminderDaySync::new(&service, cx);
                service.update(cx, |service, _| {
                    service.notifier = Some(notifier);
                    service.day_sync = Some(day_sync);
                });
                service
            }
        };
        cx.default_global::<GlobalReminderServices>()
            .by_project
            .insert(project.entity_id(), service.clone());
        service
    }

    fn new(vault: Vault, cx: &mut Context<Self>) -> Self {
        let day = ReminderDay::global(cx);
        let day_subscription = cx.subscribe(&day, |this, _, event: &DayChanged, cx| {
            this.needs_plan = true;
            cx.emit(ReminderEvent::DayChanged { today: event.today });
            this.schedule(cx);
        });
        let mut this = Self {
            vault,
            settings: ReminderSettings::default(),
            settings_loaded: false,
            settings_error: None,
            projects: Vec::new(),
            workspaces: Vec::new(),
            scan: BTreeMap::new(),
            scan_loaded: false,
            batch_state: BatchState::default(),
            dirty: BTreeSet::new(),
            unverified: BTreeSet::new(),
            recent_writes: HashSet::new(),
            reconciled: BTreeMap::new(),
            home_texts: HashMap::new(),
            failed_edits: Vec::new(),
            edit_error: None,
            write_error: None,
            batch_error: None,
            full_scan: true,
            rebuild: false,
            needs_plan: true,
            settle_rounds: 0,
            running: false,
            loaded: false,
            reminders: Vec::new(),
            notifier: None,
            day_sync: None,
            status: ReminderStatus::Idle,
            batch_task: None,
            recheck_task: None,
            cursor_task: None,
            at_cursor: BTreeSet::new(),
            cursor_wait: false,
            #[cfg(test)]
            reads: 0,
            #[cfg(test)]
            home_loads: 0,
            _subscriptions: vec![day_subscription],
        };
        this.schedule(cx);
        this
    }

    fn add_project(
        &mut self,
        project: &Entity<Project>,
        workspace: Option<WeakEntity<Workspace>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = workspace {
            self.workspaces.push(workspace);
        }
        if self
            .projects
            .iter()
            .any(|known| known.entity_id() == project.entity_id())
        {
            return;
        }
        self.projects.push(project.downgrade());
        self._subscriptions
            .push(cx.subscribe(project, Self::handle_project_event));
    }

    // -----------------------------------------------------------------------
    // Reading.

    /// Every reminder in the index, in file order, read through its home
    /// line. Views sort; this keeps arrival order.
    pub fn reminders(&self) -> &[IndexedReminder] {
        &self.reminders
    }

    pub fn reminder(&self, id: &str) -> Option<&IndexedReminder> {
        self.reminders.iter().find(|reminder| reminder.id == id)
    }

    pub fn settings(&self) -> &ReminderSettings {
        &self.settings
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    /// How notes are read: the vault's `[daily]` table and the day headings.
    pub fn config(&self) -> ReminderConfig {
        self.settings.reminder_config(&self.vault)
    }

    /// The index's absolute path.
    pub fn index_abs_path(&self) -> PathBuf {
        self.vault.root.join(&self.settings.index)
    }

    /// The absolute path of a vault-relative one, such as a reminder's home.
    pub fn abs_path(&self, relative: &str) -> PathBuf {
        self.vault.root.join(relative)
    }

    /// The day reconciler that keeps each daily note's reminders section.
    pub fn day_sync(&self) -> Option<&Entity<ReminderDaySync>> {
        self.day_sync.as_ref()
    }

    pub fn status(&self) -> &ReminderStatus {
        &self.status
    }

    /// Whether the first batch has finished, so `reminders` reflects the
    /// index rather than an empty start.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The vault's `Fs`, while one of its windows is open.
    pub fn fs(&self, cx: &App) -> Option<Arc<dyn Fs>> {
        self.projects
            .iter()
            .find_map(WeakEntity::upgrade)
            .map(|project| project.read(cx).fs().clone())
    }

    // -----------------------------------------------------------------------
    // Triggers.

    fn handle_project_event(
        &mut self,
        _: Entity<Project>,
        event: &project::Event,
        cx: &mut Context<Self>,
    ) {
        let project::Event::WorktreeUpdatedEntries(_, changes) = event else {
            return;
        };
        let settings_path = format!("{VAULT_MARKER_DIR}/{REMINDERS_CONFIG_FILE}");
        let vault_config = format!("{VAULT_MARKER_DIR}/{VAULT_CONFIG_FILE}");
        let mut touched = false;
        let mut restart = false;
        for (path, _, change) in changes.iter() {
            let path = path.as_unix_str();
            if path == settings_path || path == vault_config {
                if path == vault_config
                    && let VaultStatus::Valid(vault) = Vault::detect(&self.vault.root)
                {
                    self.vault = vault;
                }
                self.settings_loaded = false;
                self.full_scan = true;
                touched = true;
                restart = true;
            } else if path == self.settings.index || is_reminder_note(&self.vault, path) {
                match change {
                    project::PathChange::Added => {
                        cx.emit(ReminderEvent::NoteCreated {
                            path: path.to_string(),
                        });
                        self.dirty.insert(path.to_string());
                    }
                    // The worktree's first scan reports every file; the scan
                    // cache says which of them changed while Thock was closed.
                    project::PathChange::Loaded => {
                        self.unverified.insert(path.to_string());
                    }
                    project::PathChange::AddedOrUpdated => {
                        if path != self.settings.index {
                            cx.emit(ReminderEvent::NoteChanged {
                                path: path.to_string(),
                            });
                        }
                        self.unverified.insert(path.to_string());
                    }
                    project::PathChange::Updated | project::PathChange::Removed => {
                        if path != self.settings.index {
                            cx.emit(ReminderEvent::NoteChanged {
                                path: path.to_string(),
                            });
                        }
                        self.dirty.insert(path.to_string());
                    }
                }
                touched = true;
                restart |= !self.recent_writes.contains(path);
            }
        }
        if touched {
            if restart {
                self.settle_rounds = 0;
            }
            self.schedule(cx);
        }
    }

    /// A note was just created (by `notes::ensure_note_at` or a ritual):
    /// read it now and tell listeners. `path` is vault-relative.
    pub fn note_created(&mut self, path: &str, cx: &mut Context<Self>) {
        self.dirty.insert(path.to_string());
        cx.emit(ReminderEvent::NoteCreated {
            path: path.to_string(),
        });
        self.schedule(cx);
    }

    /// Forgets the scan cache and reads every note again.
    pub fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rebuild = true;
        self.full_scan = true;
        self.settle_rounds = 0;
        self.schedule(cx);
    }

    /// Runs the batch again after a failed write, and the home-line edits
    /// that failed.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.full_scan = true;
        self.needs_plan = true;
        self.settle_rounds = 0;
        self.edit_error = None;
        self.write_error = None;
        for failed in std::mem::take(&mut self.failed_edits) {
            // A failure is recorded again by `edit_reminder` itself.
            self.edit_reminder(&failed.id, failed.edit, cx)
                .detach_and_log_err(cx);
        }
        self.refresh_status(cx);
        self.schedule(cx);
        // The day sections' failed writes too, once this update is done: the
        // day sync reads this service.
        if let Some(day_sync) = self.day_sync.clone() {
            cx.defer(move |cx| day_sync.update(cx, |day_sync, cx| day_sync.retry(cx)));
        }
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.running = true;
        self.batch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BATCH_DELAY).await;
                let Ok(Some(job)) = this.update(cx, |this, cx| this.take_job(cx)) else {
                    break;
                };
                let result = Self::run_batch(job, cx).await;
                if this
                    .update(cx, |this, cx| this.finish_batch(result, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn take_job(&mut self, cx: &mut Context<Self>) -> Option<Job> {
        let has_work = !self.dirty.is_empty()
            || !self.unverified.is_empty()
            || self.full_scan
            || self.needs_plan;
        let projects: Vec<Entity<Project>> = self
            .projects
            .iter()
            .filter_map(WeakEntity::upgrade)
            .collect();
        let Some(fs) = projects
            .first()
            .map(|project| project.read(cx).fs().clone())
        else {
            self.running = false;
            return None;
        };
        if !has_work || self.settle_rounds >= MAX_SETTLE_ROUNDS {
            self.running = false;
            self.recent_writes.clear();
            return None;
        }
        self.settle_rounds += 1;
        for (path, lines) in std::mem::take(&mut self.reconciled) {
            let entry = self.scan.entry(path).or_default();
            entry.lines = lines;
            entry.mtime = None;
        }
        self.workspaces
            .retain(|workspace| workspace.upgrade().is_some());
        Some(Job {
            fs,
            vault: self.vault.clone(),
            settings: self.settings.clone(),
            config_needed: !self.settings_loaded,
            projects,
            workspaces: self
                .workspaces
                .iter()
                .filter_map(WeakEntity::upgrade)
                .collect(),
            dirty: std::mem::take(&mut self.dirty),
            unverified: std::mem::take(&mut self.unverified),
            full_scan: std::mem::take(&mut self.full_scan),
            rebuild: std::mem::take(&mut self.rebuild),
            scan: self.scan.clone(),
            scan_loaded: self.scan_loaded,
            home_texts: self.home_texts.clone(),
            state: self.batch_state.clone(),
            now: {
                self.needs_plan = false;
                ReminderClock::global(cx).now()
            },
        })
    }

    fn finish_batch(&mut self, result: JobResult, cx: &mut Context<Self>) {
        if let Some(settings) = result.settings {
            self.settings_loaded = true;
            match settings {
                Ok(settings) => {
                    self.settings_error = None;
                    if self.settings != settings {
                        self.settings = settings;
                        cx.emit(ReminderEvent::SettingsChanged);
                    }
                }
                // The last good settings stay, and the error stays on the
                // status row until the file parses again.
                Err(error) => {
                    log::warn!("Thock reminders: {error:#}; keeping the last good settings");
                    self.settings_error = Some(format!("{error:#}"));
                }
            }
        }
        self.scan = result.scan;
        self.scan_loaded = true;
        self.batch_state = result.state;
        self.home_texts = result.home_texts;
        self.recent_writes = result.written.iter().cloned().collect();
        #[cfg(test)]
        {
            self.reads += result.reads;
            self.home_loads += result.home_loads;
        }
        self.dirty.extend(result.written);
        self.at_cursor.extend(result.at_cursor);
        if !self.at_cursor.is_empty() && !self.cursor_wait {
            self.cursor_wait = true;
            self.cursor_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TYPING_QUIET).await;
                this.update(cx, |this, cx| {
                    this.cursor_wait = false;
                    let paths = std::mem::take(&mut this.at_cursor);
                    this.dirty.extend(paths);
                    this.settle_rounds = 0;
                    this.schedule(cx);
                })
                .log_err();
            }));
        }
        let deferred = !result.deferred.is_empty();
        self.dirty.extend(result.deferred);
        if deferred {
            self.settle_rounds = 0;
        }
        self.batch_error = result.errors.first().cloned().map(SharedString::from);
        self.refresh_status(cx);
        if let Some(at) = result.recheck_at {
            let wait = ReminderClock::global(cx).duration_until(at) + Duration::from_millis(1);
            self.recheck_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |this, cx| {
                    this.needs_plan = true;
                    this.settle_rounds = 0;
                    this.schedule(cx);
                })
                .log_err();
            }));
        }
        if let Some(reminders) = result.reminders {
            let first = !self.loaded;
            self.loaded = true;
            if reminders != self.reminders || first {
                self.reminders = reminders;
                cx.emit(ReminderEvent::IndexChanged);
            }
        }
        cx.notify();
    }

    /// The status row: the batch's error, a settings file that won't
    /// parse, a home-line edit that failed, or a failed write, first first.
    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let error = self
            .batch_error
            .clone()
            .or_else(|| self.settings_error.clone().map(SharedString::from))
            .or_else(|| self.failed_edits.first().map(|failed| failed.error.clone()))
            .or_else(|| self.edit_error.clone())
            .or_else(|| self.write_error.clone());
        let status = match error {
            Some(error) => ReminderStatus::Failing { error },
            None => ReminderStatus::Idle,
        };
        if status != self.status {
            self.status = status;
            cx.notify();
        }
    }

    // -----------------------------------------------------------------------
    // The batch.

    async fn run_batch(mut job: Job, cx: &mut AsyncApp) -> JobResult {
        let mut errors = Vec::new();
        let settings_result = if job.config_needed {
            Some(load_reminder_settings(&job.fs, &job.vault.root).await)
        } else {
            None
        };
        let settings = match &settings_result {
            Some(Ok(settings)) => settings.clone(),
            _ => job.settings.clone(),
        };
        let config = settings.reminder_config(&job.vault);
        let index_path = settings.index.clone();
        let cache_path = job
            .vault
            .root
            .join(VAULT_MARKER_DIR)
            .join(CACHE_DIR)
            .join(SCAN_FILE);

        if job.rebuild {
            job.scan.clear();
        } else if !job.scan_loaded
            && let Ok(text) = job.fs.load(&cache_path).await
            && let Some(cache) = serde_json::from_str::<ScanCache>(&text).log_err()
            && cache.version == SCAN_VERSION
        {
            job.scan = cache.files;
        }
        let scan_before = job.scan.clone();

        let mut dirty = job.dirty.clone();
        for path in &job.unverified {
            if dirty.contains(path) {
                continue;
            }
            let mtime = mtime_of(&job.fs, &job.vault.root.join(path)).await;
            let known = job.scan.get(path).and_then(|entry| entry.mtime);
            if mtime.is_none() || known != mtime {
                dirty.insert(path.clone());
            }
        }
        if job.full_scan {
            let listed = list_notes(&job.projects, &job.vault, &index_path, cx);
            for path in &listed {
                let mtime = mtime_of(&job.fs, &job.vault.root.join(path)).await;
                let known = job.scan.get(path).and_then(|entry| entry.mtime);
                if mtime.is_none() || known != mtime {
                    dirty.insert(path.clone());
                }
            }
            for path in job.scan.keys() {
                if !listed.contains(path) {
                    dirty.insert(path.clone());
                }
            }
        }
        dirty.insert(index_path.clone());

        // Read what changed: an open buffer once it has been quiet, the disk
        // otherwise.
        let mut texts: BTreeMap<String, String> = BTreeMap::new();
        let mut buffers: HashMap<String, Entity<Buffer>> = HashMap::new();
        let mut mtimes: HashMap<String, Option<(u64, u32)>> = HashMap::new();
        let mut deferred = Vec::new();
        let mut versions = Vec::new();
        for path in &dirty {
            let abs = job.vault.root.join(path);
            if let Some(buffer) = open_buffer(&job.projects, &abs, cx) {
                versions.push((
                    path.clone(),
                    buffer.read_with(cx, |buffer, _| buffer.version()),
                ));
                buffers.insert(path.clone(), buffer);
            }
        }
        if !versions.is_empty() && !job.full_scan {
            cx.background_executor().timer(TYPING_QUIET).await;
        }
        for (path, version) in versions {
            if let Some(buffer) = buffers.get(&path)
                && buffer.read_with(cx, |buffer, _| buffer.version() != version)
                && path != index_path
            {
                buffers.remove(&path);
                deferred.push(path.clone());
                dirty.remove(&path);
            }
        }
        #[cfg(test)]
        let mut reads = 0;
        let mut unreadable: HashSet<String> = HashSet::new();
        for path in &dirty {
            let abs = job.vault.root.join(path);
            let mtime = mtime_of(&job.fs, &abs).await;
            mtimes.insert(path.clone(), mtime);
            let text = match buffers.get(path) {
                Some(buffer) => {
                    let (text, unsaved) =
                        buffer.read_with(cx, |buffer, _| (buffer.text(), buffer.is_dirty()));
                    // Ids read from unsaved text are not what the file on
                    // disk holds, so the next startup reads it again.
                    if unsaved {
                        mtimes.insert(path.clone(), None);
                    }
                    Some(text)
                }
                None if mtime.is_some() => match job.fs.load(&abs).await {
                    Ok(text) => Some(text),
                    Err(error) => {
                        // An unreadable note is not a deleted one: its scan
                        // entry stays, so its ids are never marked removed.
                        errors.push(format!("{path}: {error:#}"));
                        unreadable.insert(path.clone());
                        None
                    }
                },
                None => None,
            };
            #[cfg(test)]
            {
                reads += 1;
            }
            if let Some(text) = text {
                texts.insert(path.clone(), text);
            }
        }

        let previous: BTreeMap<String, Vec<ScanLine>> = dirty
            .iter()
            .filter_map(|path| Some((path.clone(), job.scan.get(path)?.lines.clone())))
            .collect();
        for path in &dirty {
            match texts.get(path) {
                Some(text) => {
                    job.scan.insert(
                        path.clone(),
                        ScanEntry {
                            mtime: mtimes.get(path).copied().flatten(),
                            lines: scan_note(text, path, &config),
                        },
                    );
                }
                None if unreadable.contains(path) => {}
                None => {
                    job.scan.remove(path);
                }
            }
        }
        let skip_rows = cursor_rows(&job.workspaces, &buffers, cx);
        let at_cursor: Vec<String> = skip_rows
            .iter()
            .filter(|(path, rows)| {
                job.scan.get(*path).is_some_and(|entry| {
                    entry
                        .lines
                        .iter()
                        .any(|line| line.has_day && line.id.is_none() && rows.contains(&line.row))
                })
            })
            .map(|(path, _)| path.clone())
            .collect();
        let scan_lines: BTreeMap<String, Vec<ScanLine>> = job
            .scan
            .iter()
            .map(|(path, entry)| (path.clone(), entry.lines.clone()))
            .collect();
        let index_text = texts.get(&index_path).cloned();
        let mut rng = rand::rng();
        // Without the index's text the step would rebuild it from nothing.
        let plan = if unreadable.contains(&index_path) {
            Default::default()
        } else {
            plan_batch(
                &BatchInput {
                    index_path: &index_path,
                    index_text: index_text.as_deref(),
                    scan: &scan_lines,
                    previous: &previous,
                    changed: &texts,
                    skip_rows: &skip_rows,
                    now: job.now,
                },
                &mut job.state,
                &settings,
                &config,
                &mut || id_from_bytes(rng.random()),
            )
        };

        let mut written = Vec::new();
        for file in &plan.changes {
            let abs = job.vault.root.join(&file.path);
            let changes = file.changes.clone();
            let path = file.path.clone();
            let settings = settings.clone();
            let config = config.clone();
            let outcome = write_text(
                &job.fs,
                &job.projects,
                &abs,
                move |text| {
                    let edited = apply_changes(text, &path, &changes, &settings, &config);
                    (Some(edited.as_str()) != text).then_some(edited)
                },
                cx,
            )
            .await;
            match outcome {
                Ok(true) => written.push(file.path.clone()),
                Ok(false) => {}
                Err(error) => {
                    log::warn!("Thock reminders: writing {}: {error:#}", file.path);
                    errors.push(format!("{}: {error:#}", file.path));
                }
            }
        }

        // Read the index the way every desk reader sees it. An unreadable one
        // keeps the reminders read last time.
        let index_abs = job.vault.root.join(&index_path);
        let mut home_texts = HashMap::new();
        #[cfg(test)]
        let mut home_loads = 0;
        let reminders = match read_text(&job.fs, &job.projects, &index_abs, cx).await {
            Err(error) => {
                errors.push(format!("{index_path}: {error:#}"));
                None
            }
            Ok(index_now) => {
                let index_now = index_now.unwrap_or_default();
                let mut linked = HashMap::new();
                for entry in
                    crate::reminder_index::parse_index(&index_now, &index_path, &settings, &config)
                {
                    if let crate::reminder_index::EntryKind::Link { home } = entry.kind
                        && !linked.contains_key(&home)
                    {
                        let text = match texts.get(&home) {
                            Some(text) if !written.contains(&home) => Ok(Some(text.clone())),
                            _ => read_home(
                                &job.fs,
                                &job.projects,
                                &job.vault.root.join(&home),
                                job.home_texts.get(&home),
                                cx,
                            )
                            .await
                            .map(|read| {
                                read.map(|read| {
                                    #[cfg(test)]
                                    if read.loaded {
                                        home_loads += 1;
                                    }
                                    if let Some(cache) = read.cache {
                                        home_texts.insert(home.clone(), cache);
                                    }
                                    read.text
                                })
                            }),
                        };
                        match text {
                            Ok(Some(text)) => {
                                linked.insert(home, text);
                            }
                            Ok(None) => {}
                            Err(error) => errors.push(format!("{home}: {error:#}")),
                        }
                    }
                }
                Some(read_index(
                    &index_path,
                    &index_now,
                    &linked,
                    &settings,
                    &config,
                ))
            }
        };

        let cache = ScanCache {
            version: SCAN_VERSION,
            files: job.scan.clone(),
        };
        if job.scan != scan_before
            && let Some(text) = serde_json::to_string(&cache).log_err()
        {
            let write = async {
                if let Some(parent) = cache_path.parent() {
                    job.fs.create_dir(parent).await?;
                }
                job.fs.atomic_write(cache_path.clone(), text).await
            };
            write.await.log_err();
        }

        JobResult {
            settings: settings_result,
            scan: job.scan,
            state: job.state,
            written,
            deferred,
            at_cursor,
            home_texts,
            errors,
            recheck_at: plan.recheck_at,
            reminders,
            #[cfg(test)]
            reads,
            #[cfg(test)]
            home_loads,
        }
    }

    // -----------------------------------------------------------------------
    // Writing.

    /// Edits the home line of reminder `id`: close, reopen, rewrite or drop
    /// it with the sync-core edits. The index follows in the next batch.
    pub fn edit_reminder(
        &mut self,
        id: &str,
        edit: HomeEdit,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let Some(reminder) = self.reminder(id) else {
            return Task::ready(Err(anyhow!("no reminder {id} in the index")));
        };
        let home = reminder.home.clone();
        let change = Change::Home {
            id: id.to_string(),
            edit,
        };
        let settings = self.settings.clone();
        let config = self.config();
        let task = self.write_note(
            &home,
            {
                let home = home.clone();
                move |text| {
                    let text = text?;
                    let edited = apply_changes(Some(text), &home, &[change], &settings, &config);
                    (edited != text).then_some(edited)
                }
            },
            cx,
        );
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                this.edit_reminder_done(&id, edit, &home, &outcome, cx)
            })?;
            match outcome {
                Ok(true) => Ok(()),
                Ok(false) => Err(anyhow!(
                    "the reminder's line in {home} could not be changed"
                )),
                Err(error) => Err(error),
            }
        })
    }

    /// Keeps a failed home-line edit for `retry` and on the status row until
    /// it goes through.
    fn edit_reminder_done(
        &mut self,
        id: &str,
        edit: HomeEdit,
        home: &str,
        outcome: &Result<bool>,
        cx: &mut Context<Self>,
    ) {
        self.failed_edits.retain(|failed| failed.id != id);
        match outcome {
            Ok(true) => self.edit_error = None,
            Ok(false) => {
                self.edit_error =
                    Some(format!("{home}: the reminder's line could not be changed").into());
            }
            Err(error) => self.failed_edits.push(FailedEdit {
                id: id.to_string(),
                edit,
                error: format!("{home}: {error:#}").into(),
            }),
        }
        self.refresh_status(cx);
    }

    /// Adds an empty task, `- [ ] `, at the end of the index's Open section
    /// (creating the index when it is missing) and returns the index's path
    /// and the new line's row, so the caller can put the cursor there.
    pub fn new_loose_line(&mut self, cx: &mut Context<Self>) -> Task<Result<(PathBuf, u32)>> {
        let settings = self.settings.clone();
        let path = self.index_abs_path();
        let row = Arc::new(std::sync::Mutex::new(0usize));
        let task = self.write_note(
            &self.settings.index.clone(),
            {
                let row = row.clone();
                move |text| {
                    let (edited, new_row) = insert_loose_line(text, &settings);
                    if let Ok(mut row) = row.lock() {
                        *row = new_row;
                    }
                    Some(edited)
                }
            },
            cx,
        );
        cx.spawn(async move |_, _| {
            task.await?;
            let row = row.lock().map(|row| *row).unwrap_or(0);
            Ok((path, u32::try_from(row).unwrap_or(u32::MAX)))
        })
    }

    /// Rewrites the vault-relative note `path` with `edit`, which gets its
    /// current text (`None` when missing) and returns the new one, or `None`
    /// to leave it. An open buffer is edited as one undoable transaction;
    /// otherwise the file is written through `Fs`. Resolves to whether
    /// anything was written. The notes are read again in the next batch.
    pub fn write_note(
        &mut self,
        path: &str,
        edit: impl FnOnce(Option<&str>) -> Option<String> + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        self.write_note_and_scan(path, edit, false, cx)
    }

    /// `write_note` for the day reconciler: the scan takes the note as the
    /// reconciler wrote it, so the mirrors it ticked or unticked are never
    /// read back as a person's tick on a copy (§5.3).
    pub fn write_day_section(
        &mut self,
        path: &str,
        edit: impl FnOnce(Option<&str>) -> Option<String> + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        self.write_note_and_scan(path, edit, true, cx)
    }

    fn write_note_and_scan(
        &mut self,
        path: &str,
        edit: impl FnOnce(Option<&str>) -> Option<String> + 'static,
        rescan: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        let projects: Vec<Entity<Project>> = self
            .projects
            .iter()
            .filter_map(WeakEntity::upgrade)
            .collect();
        let Some(fs) = projects
            .first()
            .map(|project| project.read(cx).fs().clone())
        else {
            return Task::ready(Err(anyhow!("the vault's window is closed")));
        };
        let abs = self.vault.root.join(path);
        let path = path.to_string();
        let written_text = Arc::new(std::sync::Mutex::new(None::<String>));
        let edit = {
            let written_text = written_text.clone();
            move |text: Option<&str>| {
                let edited = edit(text);
                if rescan && let Ok(mut written) = written_text.lock() {
                    *written = edited.clone();
                }
                edited
            }
        };
        cx.spawn(async move |this, cx| {
            let outcome = write_text(&fs, &projects, &abs, edit, cx).await;
            this.update(cx, |this, cx| match &outcome {
                Ok(wrote) => {
                    if *wrote
                        && let Some(text) =
                            written_text.lock().ok().and_then(|mut text| text.take())
                    {
                        let lines = scan_note(&text, &path, &this.config());
                        this.reconciled.insert(path.clone(), lines);
                    }
                    this.dirty.insert(path.clone());
                    this.settle_rounds = 0;
                    this.schedule(cx);
                }
                Err(error) => {
                    this.write_error = Some(format!("{path}: {error:#}").into());
                    this.refresh_status(cx);
                }
            })?;
            outcome
        })
    }

    #[cfg(test)]
    fn reads(&self) -> usize {
        self.reads
    }
}

/// Every in-scope note of the vault, from the worktree, vault-relative.
fn list_notes(
    projects: &[Entity<Project>],
    vault: &Vault,
    index_path: &str,
    cx: &mut AsyncApp,
) -> BTreeSet<String> {
    let Some(project) = projects.first() else {
        return BTreeSet::new();
    };
    project.read_with(cx, |project, cx| {
        let Some(worktree) = project.visible_worktrees(cx).next() else {
            return BTreeSet::new();
        };
        worktree
            .read(cx)
            .snapshot()
            .files(false, 0)
            .map(|entry| entry.path.as_unix_str().to_string())
            .filter(|path| path == index_path || is_reminder_note(vault, path))
            .collect()
    })
}

async fn mtime_of(fs: &Arc<dyn Fs>, path: &Path) -> Option<(u64, u32)> {
    let metadata = fs.metadata(path).await.ok()??;
    if metadata.is_dir {
        return None;
    }
    metadata.mtime.to_seconds_and_nanos_for_persistence()
}

fn open_buffer(
    projects: &[Entity<Project>],
    abs: &Path,
    cx: &mut AsyncApp,
) -> Option<Entity<Buffer>> {
    projects.iter().find_map(|project| {
        project.read_with(cx, |project, cx| {
            project
                .project_path_for_absolute_path(abs, cx)
                .and_then(|path| project.get_open_buffer(&path, cx))
        })
    })
}

async fn read_text(
    fs: &Arc<dyn Fs>,
    projects: &[Entity<Project>],
    abs: &Path,
    cx: &mut AsyncApp,
) -> Result<Option<String>> {
    if let Some(buffer) = open_buffer(projects, abs, cx) {
        return Ok(Some(buffer.read_with(cx, |buffer, _| buffer.text())));
    }
    if !fs.is_file(abs).await {
        return Ok(None);
    }
    fs.load(abs)
        .await
        .map(Some)
        .with_context(|| format!("reading {}", abs.display()))
}

/// A linked note as `read_home` found it.
struct HomeText {
    text: String,
    /// What to keep for the next batch: the text by mtime, when it came from
    /// the disk.
    cache: Option<CachedText>,
    /// Whether the disk was read.
    #[cfg(test)]
    loaded: bool,
}

/// A linked note's text: its open buffer, the cached text while its mtime
/// is unchanged, or the disk. `None` when the note is missing.
async fn read_home(
    fs: &Arc<dyn Fs>,
    projects: &[Entity<Project>],
    abs: &Path,
    cached: Option<&CachedText>,
    cx: &mut AsyncApp,
) -> Result<Option<HomeText>> {
    if let Some(buffer) = open_buffer(projects, abs, cx) {
        return Ok(Some(HomeText {
            text: buffer.read_with(cx, |buffer, _| buffer.text()),
            cache: None,
            #[cfg(test)]
            loaded: false,
        }));
    }
    let Some(mtime) = mtime_of(fs, abs).await else {
        return Ok(read_text(fs, projects, abs, cx)
            .await?
            .map(|text| HomeText {
                text,
                cache: None,
                #[cfg(test)]
                loaded: true,
            }));
    };
    if let Some(cached) = cached.filter(|cached| cached.mtime == mtime) {
        return Ok(Some(HomeText {
            text: cached.text.clone(),
            cache: Some(cached.clone()),
            #[cfg(test)]
            loaded: false,
        }));
    }
    let text = fs
        .load(abs)
        .await
        .with_context(|| format!("reading {}", abs.display()))?;
    Ok(Some(HomeText {
        cache: Some(CachedText {
            mtime,
            text: text.clone(),
        }),
        text,
        #[cfg(test)]
        loaded: true,
    }))
}

/// Writes through the open buffer as one undoable transaction, or through
/// `Fs`. `edit` gets the current text and returns the new one, or `None` to
/// leave the note alone.
async fn write_text(
    fs: &Arc<dyn Fs>,
    projects: &[Entity<Project>],
    abs: &Path,
    edit: impl FnOnce(Option<&str>) -> Option<String>,
    cx: &mut AsyncApp,
) -> Result<bool> {
    if let Some(buffer) = open_buffer(projects, abs, cx) {
        let (text, was_dirty) =
            buffer.read_with(cx, |buffer, _| (buffer.text(), buffer.is_dirty()));
        let Some(new_text) = edit(Some(&text)) else {
            return Ok(false);
        };
        let diff = buffer
            .read_with(cx, |buffer, cx| buffer.diff(new_text, cx))
            .await;
        // `apply_diff` is its own transaction; finalizing it keeps it out of
        // the person's next undo group, so one undo takes it back.
        let applied = buffer.update(cx, |buffer, cx| {
            let applied = buffer.apply_diff(diff, cx).is_some();
            if applied {
                buffer.finalize_last_transaction();
            }
            applied
        });
        if !applied {
            return Err(anyhow!(
                "the note changed while the reminder edit was applied"
            ));
        }
        // A note with no unsaved edits of the person's is saved, so the change
        // reaches the disk and the phone even in a tab nobody looks at.
        if !was_dirty
            && let Some(project) = projects.iter().find(|project| {
                project.read_with(cx, |project, cx| {
                    project
                        .project_path_for_absolute_path(abs, cx)
                        .and_then(|path| project.get_open_buffer(&path, cx))
                        .is_some_and(|open| open == buffer)
                })
            })
        {
            project
                .update(cx, |project, cx| project.save_buffer(buffer.clone(), cx))
                .await
                .with_context(|| format!("saving {}", abs.display()))?;
        }
        return Ok(true);
    }
    let text = if fs.is_file(abs).await {
        Some(
            fs.load(abs)
                .await
                .with_context(|| format!("reading {}", abs.display()))?,
        )
    } else {
        None
    };
    let Some(new_text) = edit(text.as_deref()) else {
        return Ok(false);
    };
    if let Some(parent) = abs.parent() {
        fs.create_dir(parent).await?;
    }
    fs.atomic_write(abs.to_path_buf(), new_text)
        .await
        .with_context(|| format!("writing {}", abs.display()))?;
    Ok(true)
}

/// The rows the cursor sits on in each workspace's active editor, per open
/// note, so the id waits until the person leaves the line. A note in a
/// background tab gets its ids whatever its cursor says.
fn cursor_rows(
    workspaces: &[Entity<Workspace>],
    buffers: &HashMap<String, Entity<Buffer>>,
    cx: &mut AsyncApp,
) -> HashMap<String, HashSet<usize>> {
    let mut rows: HashMap<String, HashSet<usize>> = HashMap::new();
    if buffers.is_empty() {
        return rows;
    }
    for workspace in workspaces {
        let editors: Vec<Entity<editor::Editor>> = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<editor::Editor>())
                .into_iter()
                .collect()
        });
        for editor in editors {
            editor.update(cx, |editor, cx| {
                let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
                    return;
                };
                let Some((path, _)) = buffers.iter().find(|(_, open)| **open == buffer) else {
                    return;
                };
                let snapshot = editor.display_snapshot(cx);
                for selection in editor.selections.all::<Point>(&snapshot) {
                    rows.entry(path.clone())
                        .or_default()
                        .insert(selection.head().row as usize);
                }
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reminders::TestClock;
    use crate::vault::VaultConfig;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use settings::SettingsStore;

    const NOW: &str = "2026-10-21 14:05:00";

    fn now() -> NaiveDateTime {
        NaiveDateTime::parse_from_str(NOW, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            TestClock::install(now(), cx);
        });
    }

    fn vault() -> Vault {
        Vault {
            root: PathBuf::from("/vault"),
            config: VaultConfig::default(),
        }
    }

    struct Setup {
        fs: Arc<FakeFs>,
        project: Entity<Project>,
        service: Entity<ReminderService>,
    }

    async fn setup(cx: &mut TestAppContext, tree: serde_json::Value) -> Setup {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", tree).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = cx.update(|cx| ReminderService::attach(vault(), &project, None, cx));
        settle(cx);
        Setup {
            fs,
            project,
            service,
        }
    }

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..12 {
            cx.executor().advance_clock(BATCH_DELAY);
            cx.run_until_parked();
        }
    }

    async fn read(setup: &Setup, path: &str) -> String {
        setup
            .fs
            .load(&Path::new("/vault").join(path))
            .await
            .unwrap_or_default()
    }

    async fn write(setup: &Setup, cx: &mut TestAppContext, path: &str, text: &str) {
        setup
            .fs
            .atomic_write(Path::new("/vault").join(path), text.to_string())
            .await
            .unwrap();
        settle(cx);
    }

    fn ids_in(text: &str) -> Vec<String> {
        text.match_indices("<!--rem:")
            .map(|(at, _)| text[at + 8..at + 20].to_string())
            .collect()
    }

    #[gpui::test]
    async fn a_token_gets_an_id_and_one_link(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "projects": { "house.md": "# House\n- [ ] Call the plumber (@2026-10-21 14:30)\n" },
            }),
        )
        .await;
        let house = read(&setup, "projects/house.md").await;
        let ids = ids_in(&house);
        assert_eq!(ids.len(), 1, "{house}");
        let index = read(&setup, "reminders.md").await;
        assert_eq!(
            index
                .matches(&format!("- [[projects/house]] <!--rem:{}-->", ids[0]))
                .count(),
            1,
            "{index}"
        );
        setup.service.read_with(cx, |service, _| {
            let reminders = service.reminders();
            assert_eq!(reminders.len(), 1);
            assert_eq!(reminders[0].text.as_deref(), Some("Call the plumber"));
            assert_eq!(service.status(), &ReminderStatus::Idle);
        });
        assert!(
            setup
                .fs
                .is_file(Path::new("/vault/.thock/cache/reminders/scan.json"))
                .await
        );
    }

    #[gpui::test]
    async fn the_index_follows_moves_state_and_removals(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n- [ ] Ask (@2026-10-23) <!--rem:222222222222-->\n",
                "b.md": "# B\n",
                "reminders.md": "## Open\n- [[a]] <!--rem:111111111111-->\n- [[a]] <!--rem:222222222222-->\n\n## Done\n",
            }),
        )
        .await;

        // A cut and paste between notes retargets the link.
        write(
            &setup,
            cx,
            "a.md",
            "- [ ] Ask (@2026-10-23) <!--rem:222222222222-->\n",
        )
        .await;
        write(
            &setup,
            cx,
            "b.md",
            "# B\n- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n",
        )
        .await;
        assert_eq!(
            read(&setup, "reminders.md").await,
            "## Open\n- [[b]] <!--rem:111111111111-->\n- [[a]] <!--rem:222222222222-->\n\n## Done\n"
        );

        // A dropped token moves the entry to Done.
        write(&setup, cx, "a.md", "- [ ] Ask <!--rem:222222222222-->\n").await;
        assert!(
            read(&setup, "reminders.md")
                .await
                .contains("## Done\n- [[a]] dropped 2026-10-21 <!--rem:222222222222-->")
        );

        // A deleted line waits out the grace, then reads removed.
        write(&setup, cx, "b.md", "# B\n").await;
        assert!(
            read(&setup, "reminders.md")
                .await
                .contains("- [[b]] <!--rem:111111111111-->")
        );
        cx.executor().advance_clock(Duration::from_secs(11));
        settle(cx);
        assert!(
            read(&setup, "reminders.md")
                .await
                .contains("- [[b]] removed 2026-10-21 <!--rem:111111111111-->"),
            "{}",
            read(&setup, "reminders.md").await
        );
    }

    #[gpui::test]
    async fn copies_tick_homes_and_deleted_links_return(cx: &mut TestAppContext) {
        let mirror = "## Day planner\n\n### Reminders\n- [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->\n";
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "projects": { "house.md": "- [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->\n" },
                "daily": { "2026-10-21.md": mirror },
                "reminders.md": "## Open\n- [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\n## Done\n",
            }),
        )
        .await;
        write(
            &setup,
            cx,
            "daily/2026-10-21.md",
            &mirror.replace("- [ ] 14:30", "- [x] 14:30"),
        )
        .await;
        assert_eq!(
            read(&setup, "projects/house.md").await,
            "- [x] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->\n"
        );
        assert!(
            read(&setup, "reminders.md")
                .await
                .contains("✅ 2026-10-21 <!--rem:7d3e55a1c0f2-->")
        );

        write(
            &setup,
            cx,
            "projects/house.md",
            "- [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->\n",
        )
        .await;
        write(&setup, cx, "reminders.md", "## Open\n\n## Done\n").await;
        assert_eq!(
            read(&setup, "reminders.md").await,
            "## Open\n- [[projects/house]] <!--rem:7d3e55a1c0f2-->\n\n## Done\n"
        );
    }

    #[gpui::test]
    async fn an_open_buffer_gets_its_id_as_one_undoable_edit(cx: &mut TestAppContext) {
        let setup = setup(cx, json!({ ".thock": {}, "a.md": "# A\n" })).await;
        let buffer = setup
            .project
            .update(cx, |project, cx| {
                project.open_local_buffer("/vault/a.md", cx)
            })
            .await
            .unwrap();
        buffer.update(cx, |buffer, cx| {
            buffer.edit([(4..4, "Call (@2026-10-22)\n")], None, cx)
        });
        setup.service.update(cx, |service, cx| {
            service.dirty.insert("a.md".into());
            service.schedule(cx);
        });
        cx.executor().advance_clock(TYPING_QUIET);
        settle(cx);
        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert_eq!(ids_in(&text).len(), 1, "{text}");
        buffer.update(cx, |buffer, cx| buffer.undo(cx));
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.text()),
            "# A\nCall (@2026-10-22)\n",
            "one undo takes the id back off"
        );
        setup.service.read_with(cx, |service, _| {
            assert_eq!(
                service.status(),
                &ReminderStatus::Idle,
                "the write reported success"
            );
        });
    }

    #[gpui::test]
    async fn the_write_api_edits_home_lines(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n",
                "reminders.md": "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            }),
        )
        .await;
        let due = thock_sync_core::Due::parse("2026-10-23 09:00").unwrap();
        setup
            .service
            .update(cx, |service, cx| {
                service.edit_reminder("111111111111", HomeEdit::Rewrite(due), cx)
            })
            .await
            .unwrap();
        settle(cx);
        assert_eq!(
            read(&setup, "a.md").await,
            "- [ ] Call (@2026-10-23 09:00) <!--rem:111111111111-->\n"
        );
        setup
            .service
            .update(cx, |service, cx| {
                service.edit_reminder("111111111111", HomeEdit::Close, cx)
            })
            .await
            .unwrap();
        settle(cx);
        assert!(read(&setup, "a.md").await.starts_with("- [x] Call"));
        setup.service.read_with(cx, |service, _| {
            assert_eq!(
                service
                    .reminder("111111111111")
                    .map(|reminder| reminder.state),
                Some(crate::reminder_index::IndexedState::Closed)
            );
        });
        assert!(
            setup
                .service
                .update(cx, |service, cx| service.edit_reminder(
                    "999999999999",
                    HomeEdit::Close,
                    cx
                ))
                .await
                .is_err()
        );

        let (path, row) = setup
            .service
            .update(cx, |service, cx| service.new_loose_line(cx))
            .await
            .unwrap();
        assert_eq!(path, PathBuf::from("/vault/reminders.md"));
        let index = read(&setup, "reminders.md").await;
        assert_eq!(
            index.split('\n').nth(row as usize),
            Some("- [ ] "),
            "{index}"
        );
    }

    #[gpui::test]
    async fn startup_reads_only_changed_notes(cx: &mut TestAppContext) {
        let Setup { fs, service, .. } = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n",
                "b.md": "- [ ] Ask (@2026-10-23) <!--rem:222222222222-->\n",
                "c.md": "Plain\n",
                "reminders.md": "## Open\n- [[a]] <!--rem:111111111111-->\n- [[b]] <!--rem:222222222222-->\n\n## Done\n",
            }),
        )
        .await;
        drop(service);
        cx.update(|cx| {
            cx.default_global::<GlobalReminderServices>()
                .by_project
                .clear();
        });
        cx.run_until_parked();
        fs.atomic_write(PathBuf::from("/vault/c.md"), "Plain (@2026-10-24)\n".into())
            .await
            .unwrap();

        // A new window: the service attaches before the worktree's first
        // scan is delivered, so it sees every file reported as loaded.
        let project = Project::test(fs.clone(), [], cx).await;
        let service = cx.update(|cx| ReminderService::attach(vault(), &project, None, cx));
        project
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/vault", true, cx)
            })
            .await
            .unwrap();
        settle(cx);
        // c.md and the index, then both again after the id and the link; a
        // note the first scan reports unchanged is never read.
        let reads = service.read_with(cx, |service, _| service.reads());
        assert!(reads <= 4, "read {reads} notes");
        let index = fs.load(Path::new("/vault/reminders.md")).await.unwrap();
        assert!(index.contains("- [[c]]"), "{index}");
    }

    fn worktree_id(project: &Entity<Project>, cx: &App) -> project::WorktreeId {
        project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .unwrap()
            .read(cx)
            .id()
    }

    fn updated_entries(path: &str, change: project::PathChange) -> project::UpdatedEntriesSet {
        let path = util::rel_path::RelPath::from_unix_str(path)
            .unwrap()
            .into_arc();
        Arc::from([(path, project::ProjectEntryId::from_proto(1), change)])
    }

    #[gpui::test]
    async fn a_time_only_token_outside_a_daily_note_gets_no_id(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "projects": { "house.md": "- [ ] Call the plumber (@17:00)\n" },
                "daily": { "2026-10-21.md": "- [ ] Water the plants (@17:00)\n" },
            }),
        )
        .await;
        assert_eq!(
            read(&setup, "projects/house.md").await,
            "- [ ] Call the plumber (@17:00)\n"
        );
        assert_eq!(ids_in(&read(&setup, "daily/2026-10-21.md").await).len(), 1);
        let index = read(&setup, "reminders.md").await;
        assert!(!index.contains("projects/house"), "{index}");
        assert!(index.contains("[[daily/2026-10-21]]"), "{index}");
    }

    #[gpui::test]
    async fn a_failed_edit_stays_on_the_status_row_until_retry_applies_it(cx: &mut TestAppContext) {
        const LINE: &str = "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n";
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": LINE,
                "reminders.md": "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            }),
        )
        .await;
        setup
            .fs
            .insert_file("/vault/a.md", vec![0xff, 0xfe, 0xfd])
            .await;
        settle(cx);
        let failed = setup
            .service
            .update(cx, |service, cx| {
                service.edit_reminder("111111111111", HomeEdit::Close, cx)
            })
            .await;
        assert!(failed.is_err());

        // The note reads again; the batch that follows has no error of its
        // own, and the edit still waits.
        write(&setup, cx, "a.md", LINE).await;
        assert_eq!(read(&setup, "a.md").await, LINE);
        setup.service.read_with(cx, |service, _| {
            assert!(
                matches!(service.status(), ReminderStatus::Failing { .. }),
                "{:?}",
                service.status()
            );
        });

        setup.service.update(cx, |service, cx| service.retry(cx));
        settle(cx);
        assert!(read(&setup, "a.md").await.starts_with("- [x] Call"));
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.status(), &ReminderStatus::Idle);
        });
    }

    #[gpui::test]
    async fn an_edit_with_nothing_to_change_shows_on_the_status_row(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": "- [x] Call (@2026-10-22) <!--rem:111111111111-->\n",
                "reminders.md": "## Open\n\n## Done\n- [[a]] ✅ 2026-10-21 <!--rem:111111111111-->\n",
            }),
        )
        .await;
        let outcome = setup
            .service
            .update(cx, |service, cx| {
                service.edit_reminder("111111111111", HomeEdit::Close, cx)
            })
            .await;
        assert!(outcome.is_err());
        settle(cx);
        setup.service.read_with(cx, |service, _| {
            assert!(matches!(service.status(), ReminderStatus::Failing { .. }));
        });
        setup.service.update(cx, |service, cx| service.retry(cx));
        settle(cx);
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.status(), &ReminderStatus::Idle);
        });
    }

    #[gpui::test]
    async fn ids_read_from_an_unsaved_buffer_are_cached_without_an_mtime(cx: &mut TestAppContext) {
        let setup = setup(cx, json!({ ".thock": {}, "a.md": "# A\n" })).await;
        let buffer = setup
            .project
            .update(cx, |project, cx| {
                project.open_local_buffer("/vault/a.md", cx)
            })
            .await
            .unwrap();
        buffer.update(cx, |buffer, cx| {
            buffer.edit([(4..4, "Call (@2026-10-22)\n")], None, cx)
        });
        setup.service.update(cx, |service, cx| {
            service.dirty.insert("a.md".into());
            service.schedule(cx);
        });
        cx.executor().advance_clock(TYPING_QUIET);
        settle(cx);
        assert!(buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        setup.service.read_with(cx, |service, _| {
            let entry = service.scan.get("a.md").unwrap();
            assert!(entry.lines.iter().any(|line| line.id.is_some()));
            assert_eq!(entry.mtime, None, "read again at the next startup");
        });
    }

    #[gpui::test]
    async fn a_quiet_batch_rereads_and_rewrites_nothing(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n",
                "reminders.md": "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            }),
        )
        .await;
        let quiet_batch = |cx: &mut TestAppContext| {
            setup.service.update(cx, |service, cx| {
                service.needs_plan = true;
                service.schedule(cx);
            });
            settle(cx);
        };
        // The first quiet batch reads a.md as a linked note and keeps it.
        quiet_batch(cx);
        let cache = Path::new("/vault/.thock/cache/reminders/scan.json");
        let written = setup.fs.metadata(cache).await.unwrap().unwrap().mtime;
        let loads = setup.service.read_with(cx, |service, _| service.home_loads);
        quiet_batch(cx);
        assert_eq!(
            setup.fs.metadata(cache).await.unwrap().unwrap().mtime,
            written,
            "scan.json is written only when the scan changed"
        );
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.home_loads, loads, "a.md is unchanged");
            assert_eq!(service.reminders().len(), 1);
            assert_eq!(service.reminders()[0].text.as_deref(), Some("Call"));
        });
    }

    #[gpui::test]
    async fn the_services_own_writes_never_restart_the_settle_count(cx: &mut TestAppContext) {
        let setup = setup(cx, json!({ ".thock": {}, "a.md": "# A\n" })).await;
        setup.service.update(cx, |service, cx| {
            service.recent_writes.insert("a.md".into());
            service.settle_rounds = 3;
            service.handle_project_event(
                setup.project.clone(),
                &project::Event::WorktreeUpdatedEntries(
                    worktree_id(&setup.project, cx),
                    updated_entries("a.md", project::PathChange::Updated),
                ),
                cx,
            );
            assert_eq!(service.settle_rounds, 3);
            service.recent_writes.clear();
            service.handle_project_event(
                setup.project.clone(),
                &project::Event::WorktreeUpdatedEntries(
                    worktree_id(&setup.project, cx),
                    updated_entries("a.md", project::PathChange::Updated),
                ),
                cx,
            );
            assert_eq!(service.settle_rounds, 0);
        });
    }

    #[gpui::test]
    async fn two_projects_on_one_vault_share_a_service(cx: &mut TestAppContext) {
        let setup = setup(cx, json!({ ".thock": {}, "a.md": "# A\n" })).await;
        let second = Project::test(setup.fs.clone(), [Path::new("/vault")], cx).await;
        let service = cx.update(|cx| ReminderService::attach(vault(), &second, None, cx));
        assert_eq!(service.entity_id(), setup.service.entity_id());
        cx.update(|cx| {
            assert_eq!(
                service_for_project(&second, cx).map(|service| service.entity_id()),
                Some(setup.service.entity_id())
            );
        });
    }

    #[test]
    fn a_failed_write_shows_a_retry_row() {
        use crate::sync_status::{ActionKind, Connector, status_for_reminders};
        assert_eq!(status_for_reminders(&ReminderStatus::Idle), None);
        let row = status_for_reminders(&ReminderStatus::Failing {
            error: "a.md: disk full".into(),
        })
        .unwrap();
        assert_eq!(row.connector, Connector::Reminders);
        assert_eq!(
            row.primary_action().map(|action| action.kind),
            Some(ActionKind::RetryReminders)
        );
    }

    #[gpui::test]
    async fn the_cursor_line_gets_its_id_once_the_cursor_leaves(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", json!({ ".thock": {}, "a.md": "# A\n\n" }))
            .await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let project_path = project
            .read_with(cx, |project, cx| {
                project.find_project_path("/vault/a.md", cx)
            })
            .unwrap();
        let editor = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<editor::Editor>()
            .unwrap();
        let weak_workspace = workspace.downgrade();
        cx.update(|_, cx| ReminderService::attach(vault(), &project, Some(weak_workspace), cx));
        cx.run_until_parked();

        editor.update_in(cx, |editor, window, cx| {
            editor.move_to_end(&Default::default(), window, cx);
            editor.insert("Call the bank (@2026-10-22)", window, cx);
        });
        let buffer = editor.read_with(cx, |editor, cx| {
            editor.buffer().read(cx).as_singleton().unwrap()
        });
        project
            .update(cx, |project, cx| project.save_buffer(buffer.clone(), cx))
            .await
            .unwrap();
        for _ in 0..16 {
            cx.executor().advance_clock(BATCH_DELAY);
            cx.run_until_parked();
        }
        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert_eq!(
            text, "# A\n\nCall the bank (@2026-10-22)",
            "no id under the cursor"
        );

        editor.update_in(cx, |editor, window, cx| {
            editor.move_to_beginning(&Default::default(), window, cx);
        });
        for _ in 0..16 {
            cx.executor().advance_clock(BATCH_DELAY);
            cx.run_until_parked();
        }
        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert_eq!(
            ids_in(&text).len(),
            1,
            "the id lands once the cursor leaves: {text}"
        );
        assert!(text.starts_with("# A\n\nCall the bank (@2026-10-22) <!--rem:"));
    }

    #[gpui::test]
    async fn a_settings_change_reloads_and_tells_listeners(cx: &mut TestAppContext) {
        let setup = setup(cx, json!({ ".thock": {}, "a.md": "# A\n" })).await;
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&setup.service, move |_, event: &ReminderEvent, _| {
                events.lock().unwrap().push(event.clone());
            })
        });
        write(
            &setup,
            cx,
            ".thock/reminders.toml",
            "[headings]\nday = [\"Lembretes\"]\n",
        )
        .await;
        assert!(
            events
                .lock()
                .unwrap()
                .contains(&ReminderEvent::SettingsChanged)
        );
        setup.service.read_with(cx, |service, _| {
            assert_eq!(
                service.settings().day_headings,
                vec!["Lembretes".to_string()]
            );
        });
    }

    struct EditorWindow {
        project: Entity<Project>,
        workspace: Entity<Workspace>,
        cx: gpui::VisualTestContext,
    }

    async fn editor_window(cx: &mut TestAppContext, tree: serde_json::Value) -> EditorWindow {
        init_test(cx);
        cx.update(|cx| {
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", tree).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        let (workspace, window_cx) = new_window(&project, cx);
        EditorWindow {
            project,
            workspace,
            cx: window_cx,
        }
    }

    fn new_window(
        project: &Entity<Project>,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, gpui::VisualTestContext) {
        let (multi_workspace, window_cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let window_cx = window_cx.clone();
        let workspace = multi_workspace.read_with(&window_cx, |multi, _| multi.workspace().clone());
        let weak = workspace.downgrade();
        cx.update(|cx| ReminderService::attach(vault(), project, Some(weak), cx));
        (workspace, window_cx)
    }

    async fn open_editor(
        workspace: &Entity<Workspace>,
        project: &Entity<Project>,
        path: &str,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<editor::Editor> {
        let project_path = project
            .read_with(cx, |project, cx| project.find_project_path(path, cx))
            .unwrap();
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<editor::Editor>()
            .unwrap()
    }

    /// Types `text` at the end of the editor's note and saves it, leaving the
    /// cursor on the new line.
    async fn type_and_save(
        editor: &Entity<editor::Editor>,
        project: &Entity<Project>,
        text: &str,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<Buffer> {
        editor.update_in(cx, |editor, window, cx| {
            editor.move_to_end(&Default::default(), window, cx);
            editor.insert(text, window, cx);
        });
        let buffer = editor.read_with(cx, |editor, cx| {
            editor.buffer().read(cx).as_singleton().unwrap()
        });
        project
            .update(cx, |project, cx| project.save_buffer(buffer.clone(), cx))
            .await
            .unwrap();
        buffer
    }

    fn settle_window(cx: &mut gpui::VisualTestContext) {
        for _ in 0..16 {
            cx.executor().advance_clock(BATCH_DELAY);
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    async fn a_note_left_in_a_background_tab_gets_its_id(cx: &mut TestAppContext) {
        let EditorWindow {
            project,
            workspace,
            mut cx,
            ..
        } = editor_window(
            cx,
            json!({ ".thock": {}, "a.md": "# A\n\n", "b.md": "# B\n" }),
        )
        .await;
        let editor = open_editor(&workspace, &project, "/vault/a.md", &mut cx).await;
        let buffer = type_and_save(&editor, &project, "Call the bank (@2026-10-22)", &mut cx).await;
        settle_window(&mut cx);
        assert!(ids_in(&buffer.read_with(&cx, |buffer, _| buffer.text())).is_empty());

        // Switching tabs leaves the cursor where it was; the id lands anyway.
        open_editor(&workspace, &project, "/vault/b.md", &mut cx).await;
        settle_window(&mut cx);
        let text = buffer.read_with(&cx, |buffer, _| buffer.text());
        assert_eq!(ids_in(&text).len(), 1, "{text}");
    }

    #[gpui::test]
    async fn notes_waiting_on_cursors_are_all_retried(cx: &mut TestAppContext) {
        let EditorWindow {
            project,
            workspace,
            mut cx,
            ..
        } = editor_window(
            cx,
            json!({ ".thock": {}, "a.md": "# A\n\n", "b.md": "# B\n\n" }),
        )
        .await;
        let rounds = |cx: &mut gpui::VisualTestContext| {
            for _ in 0..3 {
                cx.executor().advance_clock(BATCH_DELAY);
                cx.run_until_parked();
            }
        };
        // a.md waits on its cursor; then b.md waits on its own before a.md's
        // retry comes round. Neither retry may cancel the other.
        let a = open_editor(&workspace, &project, "/vault/a.md", &mut cx).await;
        let a_buffer = type_and_save(&a, &project, "Call (@2026-10-22)", &mut cx).await;
        rounds(&mut cx);
        let b = open_editor(&workspace, &project, "/vault/b.md", &mut cx).await;
        let b_buffer = type_and_save(&b, &project, "Ask (@2026-10-23)", &mut cx).await;
        rounds(&mut cx);
        b.update_in(&mut cx, |editor, window, cx| {
            editor.move_to_beginning(&Default::default(), window, cx)
        });
        settle_window(&mut cx);
        for buffer in [a_buffer, b_buffer] {
            let text = buffer.read_with(&cx, |buffer, _| buffer.text());
            assert_eq!(ids_in(&text).len(), 1, "{text}");
        }
    }

    #[gpui::test]
    async fn a_service_edit_saves_a_note_with_no_unsaved_changes(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n",
                "reminders.md": "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            }),
        )
        .await;
        let buffer = setup
            .project
            .update(cx, |project, cx| {
                project.open_local_buffer("/vault/a.md", cx)
            })
            .await
            .unwrap();
        setup
            .service
            .update(cx, |service, cx| {
                service.edit_reminder("111111111111", HomeEdit::Close, cx)
            })
            .await
            .unwrap();
        settle(cx);
        assert!(!buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        assert!(read(&setup, "a.md").await.starts_with("- [x] Call"));
        assert!(read(&setup, "reminders.md").await.contains("✅ 2026-10-21"));
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.status(), &ReminderStatus::Idle);
        });
    }

    #[gpui::test]
    async fn an_unreadable_note_or_index_is_never_taken_as_empty(cx: &mut TestAppContext) {
        let setup = setup(
            cx,
            json!({
                ".thock": {},
                "a.md": "- [ ] Call (@2026-10-22) <!--rem:111111111111-->\n",
                "reminders.md": "## Open\n- [[a]] <!--rem:111111111111-->\n\n## Done\n",
            }),
        )
        .await;
        let index = read(&setup, "reminders.md").await;
        setup
            .fs
            .insert_file("/vault/a.md", vec![0xff, 0xfe, 0xfd])
            .await;
        settle(cx);
        cx.executor().advance_clock(Duration::from_secs(20));
        settle(cx);
        assert_eq!(
            read(&setup, "reminders.md").await,
            index,
            "never marked removed"
        );
        setup.service.read_with(cx, |service, _| {
            assert!(matches!(service.status(), ReminderStatus::Failing { .. }));
        });

        setup
            .fs
            .insert_file("/vault/reminders.md", vec![0xff, 0xfe, 0xfd])
            .await;
        settle(cx);
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.reminders().len(), 1, "the last reading stays");
        });
    }

    #[gpui::test]
    async fn a_broken_settings_file_keeps_the_last_good_one(cx: &mut TestAppContext) {
        let setup = setup(cx, json!({ ".thock": {}, "a.md": "# A\n" })).await;
        write(
            &setup,
            cx,
            ".thock/reminders.toml",
            "index = \"life/lembretes.md\"\n",
        )
        .await;
        write(&setup, cx, ".thock/reminders.toml", "index = [[[").await;
        write(&setup, cx, "a.md", "# A again\n").await;
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.settings().index, "life/lembretes.md");
            assert!(
                matches!(service.status(), ReminderStatus::Failing { .. }),
                "the error stays until the file parses"
            );
        });
        write(&setup, cx, ".thock/reminders.toml", "[ring]\ndesk = true\n").await;
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.status(), &ReminderStatus::Idle);
            assert_eq!(service.settings().index, "reminders.md");
        });
    }

    #[gpui::test]
    async fn retry_reaches_the_day_sync_too(cx: &mut TestAppContext) {
        let setup = setup(cx, json!({ ".thock": {}, "a.md": "# A\n" })).await;
        assert!(
            setup
                .service
                .read_with(cx, |service, _| service.day_sync().is_some())
        );
        setup.service.update(cx, |service, cx| service.retry(cx));
        settle(cx);
        setup.service.read_with(cx, |service, _| {
            assert_eq!(service.status(), &ReminderStatus::Idle);
        });
    }
}
