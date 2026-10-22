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
    Subscription, Task, WeakEntity, actions,
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
    projects: Vec<WeakEntity<Project>>,
    workspaces: Vec<WeakEntity<Workspace>>,
    scan: BTreeMap<String, ScanEntry>,
    /// Whether `scan` was filled from `scan.json` or a full scan yet.
    scan_loaded: bool,
    batch_state: BatchState,
    /// Notes to re-read in the next batch, vault-relative.
    dirty: BTreeSet<String>,
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
    /// Notes read by batches, for the startup test.
    #[cfg(test)]
    reads: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ReminderEvent> for ReminderService {}

/// One batch's inputs, taken from the service.
struct Job {
    fs: Arc<dyn Fs>,
    vault: Vault,
    settings: Option<ReminderSettings>,
    config_needed: bool,
    projects: Vec<Entity<Project>>,
    workspaces: Vec<Entity<Workspace>>,
    dirty: BTreeSet<String>,
    full_scan: bool,
    rebuild: bool,
    scan: BTreeMap<String, ScanEntry>,
    scan_loaded: bool,
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
    created: Vec<String>,
    errors: Vec<String>,
    recheck_at: Option<NaiveDateTime>,
    reminders: Vec<IndexedReminder>,
    #[cfg(test)]
    reads: usize,
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
            projects: Vec::new(),
            workspaces: Vec::new(),
            scan: BTreeMap::new(),
            scan_loaded: false,
            batch_state: BatchState::default(),
            dirty: BTreeSet::new(),
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
            #[cfg(test)]
            reads: 0,
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
            } else if path == self.settings.index || is_reminder_note(&self.vault, path) {
                if matches!(change, project::PathChange::Added) {
                    cx.emit(ReminderEvent::NoteCreated {
                        path: path.to_string(),
                    });
                } else if path != self.settings.index {
                    cx.emit(ReminderEvent::NoteChanged {
                        path: path.to_string(),
                    });
                }
                self.dirty.insert(path.to_string());
                touched = true;
            }
        }
        if touched {
            self.settle_rounds = 0;
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

    /// Runs the batch again after a failed write.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.full_scan = true;
        self.needs_plan = true;
        self.settle_rounds = 0;
        self.schedule(cx);
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
        let has_work = !self.dirty.is_empty() || self.full_scan || self.needs_plan;
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
            return None;
        }
        self.settle_rounds += 1;
        self.workspaces
            .retain(|workspace| workspace.upgrade().is_some());
        Some(Job {
            fs,
            vault: self.vault.clone(),
            settings: self.settings_loaded.then(|| self.settings.clone()),
            config_needed: !self.settings_loaded,
            projects,
            workspaces: self
                .workspaces
                .iter()
                .filter_map(WeakEntity::upgrade)
                .collect(),
            dirty: std::mem::take(&mut self.dirty),
            full_scan: std::mem::take(&mut self.full_scan),
            rebuild: std::mem::take(&mut self.rebuild),
            scan: self.scan.clone(),
            scan_loaded: self.scan_loaded,
            state: self.batch_state.clone(),
            now: {
                self.needs_plan = false;
                ReminderClock::global(cx).now()
            },
        })
    }

    fn finish_batch(&mut self, result: JobResult, cx: &mut Context<Self>) {
        if let Some(settings) = result.settings {
            let settings = settings.unwrap_or_else(|error| {
                log::warn!("Thock reminders: {error:#}; using the defaults");
                ReminderSettings::default()
            });
            let changed = self.settings != settings;
            self.settings = settings;
            self.settings_loaded = true;
            if changed {
                cx.emit(ReminderEvent::SettingsChanged);
            }
        }
        self.scan = result.scan;
        self.scan_loaded = true;
        self.batch_state = result.state;
        #[cfg(test)]
        {
            self.reads += result.reads;
        }
        self.dirty.extend(result.written);
        if !result.at_cursor.is_empty() {
            let paths = result.at_cursor;
            self.cursor_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TYPING_QUIET).await;
                this.update(cx, |this, cx| {
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
        for path in result.created {
            cx.emit(ReminderEvent::NoteCreated { path });
        }
        self.status = match result.errors.first() {
            Some(error) => ReminderStatus::Failing {
                error: error.clone().into(),
            },
            None => ReminderStatus::Idle,
        };
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
        let first = !self.loaded;
        self.loaded = true;
        if result.reminders != self.reminders || first {
            self.reminders = result.reminders;
            cx.emit(ReminderEvent::IndexChanged);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // The batch.

    async fn run_batch(mut job: Job, cx: &mut AsyncApp) -> JobResult {
        let mut errors = Vec::new();
        let settings_result = if job.config_needed {
            let loaded = load_reminder_settings(&job.fs, &job.vault.root).await;
            if let Err(error) = &loaded {
                errors.push(format!("{error:#}"));
            }
            Some(loaded)
        } else {
            None
        };
        let settings = match (&job.settings, &settings_result) {
            (Some(settings), _) => settings.clone(),
            (None, Some(Ok(settings))) => settings.clone(),
            _ => ReminderSettings::default(),
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
        {
            job.scan = cache.files;
        }

        let mut dirty = job.dirty.clone();
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
        for path in &dirty {
            let abs = job.vault.root.join(path);
            let mtime = mtime_of(&job.fs, &abs).await;
            mtimes.insert(path.clone(), mtime);
            let text = match buffers.get(path) {
                Some(buffer) => Some(buffer.read_with(cx, |buffer, _| buffer.text())),
                None if mtime.is_some() => job.fs.load(&abs).await.log_err(),
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

        let mut created = Vec::new();
        let previous: BTreeMap<String, Vec<ScanLine>> = dirty
            .iter()
            .filter_map(|path| Some((path.clone(), job.scan.get(path)?.lines.clone())))
            .collect();
        for path in &dirty {
            match texts.get(path) {
                Some(text) => {
                    if !job.scan.contains_key(path) && job.scan_loaded && path != &index_path {
                        created.push(path.clone());
                    }
                    job.scan.insert(
                        path.clone(),
                        ScanEntry {
                            mtime: mtimes.get(path).copied().flatten(),
                            lines: scan_note(text, path, &config),
                        },
                    );
                }
                None => {
                    job.scan.remove(path);
                }
            }
        }
        // `NoteCreated` already went out for file events; only the startup
        // scan's discoveries are new here, and those are not creations.
        created.clear();

        let skip_rows = cursor_rows(&job.workspaces, &buffers, cx);
        let at_cursor: Vec<String> = skip_rows
            .iter()
            .filter(|(path, rows)| {
                job.scan.get(*path).is_some_and(|entry| {
                    entry
                        .lines
                        .iter()
                        .any(|line| line.token && line.id.is_none() && rows.contains(&line.row))
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
        let plan = plan_batch(
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
        );

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

        // Read the index the way every desk reader sees it.
        let index_now = read_text(
            &job.fs,
            &job.projects,
            &job.vault.root.join(&index_path),
            cx,
        )
        .await
        .unwrap_or_default();
        let mut linked = HashMap::new();
        for entry in crate::reminder_index::parse_index(&index_now, &index_path, &settings, &config)
        {
            if let crate::reminder_index::EntryKind::Link { home } = entry.kind
                && !linked.contains_key(&home)
            {
                let text = match texts.get(&home) {
                    Some(text) if !written.contains(&home) => Some(text.clone()),
                    _ => read_text(&job.fs, &job.projects, &job.vault.root.join(&home), cx).await,
                };
                if let Some(text) = text {
                    linked.insert(home, text);
                }
            }
        }
        let reminders = read_index(&index_path, &index_now, &linked, &settings, &config);

        let cache = ScanCache {
            version: 1,
            files: job.scan.clone(),
        };
        if let Some(text) = serde_json::to_string(&cache).log_err() {
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
            created,
            errors,
            recheck_at: plan.recheck_at,
            reminders,
            #[cfg(test)]
            reads,
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
        cx.spawn(async move |_, _| {
            if task.await? {
                Ok(())
            } else {
                Err(anyhow!(
                    "the reminder's line in {home} could not be changed"
                ))
            }
        })
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
        cx.spawn(async move |this, cx| {
            let outcome = write_text(&fs, &projects, &abs, edit, cx).await;
            this.update(cx, |this, cx| match &outcome {
                Ok(_) => {
                    this.dirty.insert(path.clone());
                    this.settle_rounds = 0;
                    this.schedule(cx);
                }
                Err(error) => {
                    this.status = ReminderStatus::Failing {
                        error: format!("{path}: {error:#}").into(),
                    };
                    cx.notify();
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
) -> Option<String> {
    if let Some(buffer) = open_buffer(projects, abs, cx) {
        return Some(buffer.read_with(cx, |buffer, _| buffer.text()));
    }
    if !fs.is_file(abs).await {
        return None;
    }
    fs.load(abs).await.log_err()
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
        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        let Some(new_text) = edit(Some(&text)) else {
            return Ok(false);
        };
        let diff = buffer
            .read_with(cx, |buffer, cx| buffer.diff(new_text, cx))
            .await;
        let applied = buffer.update(cx, |buffer, cx| {
            buffer.start_transaction();
            let applied = buffer.apply_diff(diff, cx).is_some();
            buffer.end_transaction(cx);
            buffer.finalize_last_transaction();
            applied
        });
        return if applied {
            Ok(true)
        } else {
            Err(anyhow!(
                "the note changed while the reminder edit was applied"
            ))
        };
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

/// The rows a cursor sits on, per open note, so the id waits until the
/// cursor leaves the line.
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
            workspace.items_of_type::<editor::Editor>(cx).collect()
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
        let Setup {
            fs,
            project,
            service,
        } = setup(
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

        let service = cx.update(|cx| ReminderService::attach(vault(), &project, None, cx));
        settle(cx);
        // c.md and the index, then both again after the id and the link.
        let reads = service.read_with(cx, |service, _| service.reads());
        assert!(reads <= 5, "read {reads} notes");
        let index = fs.load(Path::new("/vault/reminders.md")).await.unwrap();
        assert!(index.contains("- [[c]]"), "{index}");
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
}
