//! Today's note exists when the day starts (spec
//! `thock/specs/v51-today-note-on-open.md`): one service per local project
//! on a vault creates today's daily note from the template when the vault
//! opens and when the date rolls over, and removes past days whose page is
//! still only the template. Nothing happens in a vault without a daily
//! template.

use anyhow::{Context as _, Result};
use chrono::{NaiveDate, NaiveDateTime};
use db::kvp::KeyValueStore;
use fs::{Fs, RemoveOptions};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EntityId, Global, Subscription, WeakEntity,
};
use project::Project;
use std::collections::HashMap;
use std::sync::Arc;
use workspace::Workspace;

use crate::history;
use crate::notes::{self, EnsureNoteOutcome, NoteKind, cleanup_dates, is_template_only};
use crate::reminders::{DayChanged, ReminderClock, ReminderDay};
use crate::vault::{Vault, VaultStatus};
use crate::vault_sync::{self, PhoneSyncState};

/// Where the last day a note was ensured is kept, per vault root, so the
/// next launch's cleanup reaches back to it however long the app was closed.
const RECORD_KEY_PREFIX: &str = "thock-today-note:";
const RECORD_DATE_FORMAT: &str = "%Y-%m-%d";

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
            cx.default_global::<GlobalTodayNoteServices>()
                .0
                .remove(&project_id);
        })
        .detach();
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
        TodayNoteService::attach(vault, project, Some(workspace), cx);
    }
}

#[derive(Default)]
struct GlobalTodayNoteServices(HashMap<EntityId, Entity<TodayNoteService>>);

impl Global for GlobalTodayNoteServices {}

/// The today-note service of the vault `project` is on, if it is one.
pub fn service_for_project(
    project: &Entity<Project>,
    cx: &App,
) -> Option<Entity<TodayNoteService>> {
    cx.try_global::<GlobalTodayNoteServices>()?
        .0
        .get(&project.entity_id())
        .cloned()
}

pub struct TodayNoteService {
    project: Entity<Project>,
    workspace: Option<WeakEntity<Workspace>>,
    vault: Vault,
    running: bool,
    /// A trigger arrived while a round ran: run once more when it ends.
    again: bool,
    /// Rounds finished, for tests.
    #[cfg(test)]
    rounds: usize,
    _subscriptions: Vec<Subscription>,
}

impl TodayNoteService {
    /// Starts the service for `project`, runs its first round now and runs
    /// one more on every day change. `workspace` is where a failed creation
    /// is shown.
    pub fn attach(
        vault: Vault,
        project: &Entity<Project>,
        workspace: Option<WeakEntity<Workspace>>,
        cx: &mut App,
    ) -> Entity<Self> {
        let service = cx.new(|cx| Self::new(vault, project.clone(), workspace, cx));
        cx.default_global::<GlobalTodayNoteServices>()
            .0
            .insert(project.entity_id(), service.clone());
        service
    }

    fn new(
        vault: Vault,
        project: Entity<Project>,
        workspace: Option<WeakEntity<Workspace>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let day = ReminderDay::global(cx);
        let day_subscription = cx.subscribe(&day, |this, _, _: &DayChanged, cx| {
            this.run_round(cx);
        });
        let mut this = Self {
            project,
            workspace,
            vault,
            running: false,
            again: false,
            #[cfg(test)]
            rounds: 0,
            _subscriptions: vec![day_subscription],
        };
        this.run_round(cx);
        this
    }

    #[cfg(test)]
    pub fn rounds(&self) -> usize {
        self.rounds
    }

    /// Ensures today's note and cleans past template-only days. A trigger
    /// during a round queues one more round rather than cutting it short.
    pub fn run_round(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.again = true;
            return;
        }
        self.running = true;
        let now = ReminderClock::global(cx).now();
        let round = Round {
            project: self.project.clone(),
            workspace: self.workspace.clone(),
            vault: self.vault.clone(),
            fs: self.project.read(cx).fs().clone(),
            now,
        };
        cx.spawn(async move |this, cx| {
            if let Err(error) = round.run(cx).await {
                log::error!("Thock: today's note round failed: {error:#}");
            }
            this.update(cx, |this, cx| {
                this.running = false;
                #[cfg(test)]
                {
                    this.rounds += 1;
                }
                if std::mem::take(&mut this.again) {
                    this.run_round(cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

/// One pass: the inputs a round needs, taken from the service when it starts.
struct Round {
    project: Entity<Project>,
    workspace: Option<WeakEntity<Workspace>>,
    vault: Vault,
    fs: Arc<dyn Fs>,
    now: NaiveDateTime,
}

impl Round {
    async fn run(self, cx: &mut AsyncApp) -> Result<()> {
        let template_path = self.vault.template_path(NoteKind::Daily);
        if !self.fs.is_file(&template_path).await {
            log::debug!(
                "Thock: no daily template at {}; today's note is left to the user",
                template_path.display()
            );
            return Ok(());
        }
        let today = self.now.date();
        let record_key = record_key(&self.vault);
        let last_ensured = {
            let store = cx.update(|cx| KeyValueStore::global(cx));
            let key = record_key.clone();
            cx.background_spawn(async move { store.read_kvp(&key) })
                .await
                .context("reading the last day a note was ensured")?
                .and_then(|value: String| {
                    NaiveDate::parse_from_str(&value, RECORD_DATE_FORMAT).ok()
                })
        };

        self.ensure_today(today, cx).await?;

        let store = cx.update(|cx| KeyValueStore::global(cx));
        let value = today.format(RECORD_DATE_FORMAT).to_string();
        cx.background_spawn(async move { store.write_kvp(record_key, value).await })
            .await
            .context("recording the day a note was ensured")?;

        wait_for_sync(&self.project, cx).await?;
        self.clean_past_days(today, last_ensured, cx).await
    }

    async fn ensure_today(&self, today: NaiveDate, cx: &mut AsyncApp) -> Result<()> {
        let ensured = notes::ensure_note(
            &self.fs,
            &self.vault,
            NoteKind::Daily,
            today,
            self.now.time(),
        )
        .await;
        let (path, outcome) = match ensured {
            Ok(ensured) => ensured,
            Err(error) => {
                if let Some(workspace) = &self.workspace {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .show_error(format!("Couldn't create today's note: {error:#}"), cx)
                        })
                        .ok();
                }
                return Err(error.context("creating today's note"));
            }
        };
        if outcome != EnsureNoteOutcome::AlreadyExisted {
            log::info!("Thock: created today's note at {}", path.display());
            cx.update(|cx| notes::announce_note(&self.project, &path, cx));
        }
        Ok(())
    }

    /// Removes the past days in the window whose page is still only the
    /// template (§4.3). A page that is open in an editor waits for the next
    /// round; a page that fails to read or remove is logged and tried again
    /// then too.
    async fn clean_past_days(
        &self,
        today: NaiveDate,
        last_ensured: Option<NaiveDate>,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let template_path = self.vault.template_path(NoteKind::Daily);
        let template = self
            .fs
            .load(&template_path)
            .await
            .with_context(|| format!("reading {}", template_path.display()))?;

        let mut candidates = Vec::new();
        for date in cleanup_dates(today, last_ensured) {
            let path = self.vault.note_path(NoteKind::Daily, date);
            if !self.fs.is_file(&path).await {
                continue;
            }
            let open = self.project.update(cx, |project, cx| {
                project
                    .project_path_for_absolute_path(&path, cx)
                    .and_then(|project_path| project.get_open_buffer(&project_path, cx))
                    .is_some()
            });
            if !open {
                candidates.push((date, path));
            }
        }
        if candidates.is_empty() {
            return Ok(());
        }

        // One restore point before the day's tidy, so what it removes is one
        // restore away even though the content is the template.
        cx.update(|cx| history::checkpoint_before_ai_write(&self.project, cx));

        for (date, path) in candidates {
            let removal = {
                let fs = self.fs.clone();
                let template = template.clone();
                let path = path.clone();
                let title = notes::note_title(&path);
                cx.background_spawn(async move {
                    let note = fs.load(&path).await?;
                    if !is_template_only(&note, &template, date, &title) {
                        return anyhow::Ok(false);
                    }
                    fs.remove_file(
                        &path,
                        RemoveOptions {
                            recursive: false,
                            ignore_if_not_exists: true,
                        },
                    )
                    .await?;
                    Ok(true)
                })
            };
            match removal.await {
                Ok(true) => log::info!("Thock: removed the untouched note {}", path.display()),
                Ok(false) => {}
                Err(error) => log::warn!("Thock: couldn't tidy {}: {error:#}", path.display()),
            }
        }
        Ok(())
    }
}

fn record_key(vault: &Vault) -> String {
    format!("{RECORD_KEY_PREFIX}{}", vault.root.display())
}

/// Waits until the vault's sync service, if there is one, has finished the
/// session's first pass (or has nothing to run), so every write the phone
/// queued while the desk was closed is on disk before a page is judged (§5).
async fn wait_for_sync(project: &Entity<Project>, cx: &mut AsyncApp) -> Result<()> {
    loop {
        let busy = cx.update(|cx| {
            vault_sync::service_for_project(project, cx).filter(|service| {
                matches!(
                    service.read(cx).status(),
                    PhoneSyncState::Starting | PhoneSyncState::Working
                )
            })
        });
        let Some(service) = busy else {
            return Ok(());
        };
        let (sender, receiver) = futures::channel::oneshot::channel::<()>();
        let mut sender = Some(sender);
        let subscription = cx.update(|cx| {
            cx.observe(&service, move |_, _| {
                if let Some(sender) = sender.take() {
                    sender.send(()).ok();
                }
            })
        });
        receiver.await.ok();
        drop(subscription);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::expand_template;
    use crate::reminders::TestClock;
    use crate::vault::{DEFAULT_DAILY_TEMPLATE, VaultConfig};
    use chrono::NaiveTime;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use settings::SettingsStore;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn init_test(cx: &mut TestAppContext, start: &str) -> Arc<TestClock> {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            TestClock::install(at(start), cx)
        })
    }

    fn vault() -> Vault {
        Vault {
            root: PathBuf::from("/vault"),
            config: VaultConfig::default(),
        }
    }

    fn note_path(date: &str) -> PathBuf {
        PathBuf::from(format!("/vault/daily/{date}.md"))
    }

    /// What `ensure_note` writes for `date` at nine.
    fn template_only(date: &str) -> String {
        let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
        expand_template(
            DEFAULT_DAILY_TEMPLATE,
            day,
            NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            date,
        )
    }

    fn vault_tree(daily: serde_json::Value) -> serde_json::Value {
        json!({
            ".thock": {},
            "templates": { "daily.md": DEFAULT_DAILY_TEMPLATE },
            "daily": daily,
        })
    }

    async fn attach(
        fs: &Arc<FakeFs>,
        cx: &mut TestAppContext,
    ) -> (Entity<Project>, Entity<TodayNoteService>) {
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = cx.update(|cx| TodayNoteService::attach(vault(), &project, None, cx));
        cx.run_until_parked();
        (project, service)
    }

    async fn exists(fs: &Arc<FakeFs>, date: &str) -> bool {
        fs.is_file(&note_path(date)).await
    }

    async fn read(fs: &Arc<FakeFs>, date: &str) -> String {
        fs.load(&note_path(date)).await.unwrap()
    }

    fn record(cx: &mut TestAppContext) -> Option<String> {
        let store = cx.update(|cx| KeyValueStore::global(cx));
        store.read_kvp(&record_key(&vault())).unwrap()
    }

    #[gpui::test]
    async fn a_vault_opens_with_todays_note(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-30 09:00:00");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", vault_tree(json!({}))).await;
        let (_project, service) = attach(&fs, cx).await;

        assert_eq!(service.read_with(cx, |service, _| service.rounds()), 1);
        assert_eq!(read(&fs, "2026-10-30").await, template_only("2026-10-30"));
        assert_eq!(record(cx).as_deref(), Some("2026-10-30"));
    }

    #[gpui::test]
    async fn an_existing_note_is_never_touched(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-30 09:00:00");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            vault_tree(json!({ "2026-10-30.md": "user edits\n" })),
        )
        .await;
        attach(&fs, cx).await;
        assert_eq!(read(&fs, "2026-10-30").await, "user edits\n");

        // The example day is prose, never the template.
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", vault_tree(json!({}))).await;
        let fs_dyn: Arc<dyn Fs> = fs.clone();
        let today = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        notes::ensure_example_day(
            &fs_dyn,
            &vault(),
            today,
            NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        )
        .await
        .unwrap();
        let example = read(&fs, "2026-10-30").await;
        attach(&fs, cx).await;
        assert_eq!(read(&fs, "2026-10-30").await, example);
    }

    #[gpui::test]
    async fn midnight_makes_the_new_day_and_tidies_the_old(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-29 23:59:30");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", vault_tree(json!({}))).await;
        let (_project, service) = attach(&fs, cx).await;
        assert!(exists(&fs, "2026-10-29").await);
        assert!(!exists(&fs, "2026-10-30").await);

        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();

        assert_eq!(service.read_with(cx, |service, _| service.rounds()), 2);
        assert_eq!(read(&fs, "2026-10-30").await, template_only("2026-10-30"));
        assert!(
            !exists(&fs, "2026-10-29").await,
            "yesterday's untouched page should be gone"
        );
        assert_eq!(record(cx).as_deref(), Some("2026-10-30"));
    }

    #[gpui::test]
    async fn one_line_of_the_users_keeps_the_note(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-30 09:00:00");
        let fs = FakeFs::new(cx.executor());
        let written = format!("{}- Called Ana\n", template_only("2026-10-29"));
        let with_calendar = format!(
            "{}\n### Calendar\n- 10:00 - 10:30 Standup\n",
            template_only("2026-10-28")
        );
        fs.insert_tree(
            "/vault",
            vault_tree(json!({
                "2026-10-29.md": written,
                "2026-10-28.md": with_calendar,
                "2026-10-27.md": template_only("2026-10-27"),
            })),
        )
        .await;
        attach(&fs, cx).await;

        assert_eq!(read(&fs, "2026-10-29").await, written);
        assert_eq!(read(&fs, "2026-10-28").await, with_calendar);
        assert!(!exists(&fs, "2026-10-27").await);
    }

    #[gpui::test]
    async fn a_time_line_does_not_count(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-30 09:00:00");
        let template = "# {{date:dddd, MMMM D, YYYY}}\n\nStarted at {{time}}\n\n## Journal\n";
        let yesterday = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let note = expand_template(
            template,
            yesterday,
            NaiveTime::from_hms_opt(17, 45, 0).unwrap(),
            "2026-10-29",
        );
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            json!({
                ".thock": {},
                "templates": { "daily.md": template },
                "daily": { "2026-10-29.md": note },
            }),
        )
        .await;
        attach(&fs, cx).await;
        assert!(!exists(&fs, "2026-10-29").await);
    }

    #[gpui::test]
    async fn the_window_is_thirty_days_without_a_record(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-30 09:00:00");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            vault_tree(json!({
                "2026-09-30.md": template_only("2026-09-30"),
                "2026-09-29.md": template_only("2026-09-29"),
            })),
        )
        .await;
        attach(&fs, cx).await;
        assert!(
            !exists(&fs, "2026-09-30").await,
            "30 days back is in the window"
        );
        assert!(exists(&fs, "2026-09-29").await, "31 days back is not");
    }

    #[gpui::test]
    async fn a_page_this_service_made_is_tidied_after_any_gap(cx: &mut TestAppContext) {
        let clock = init_test(cx, "2026-10-30 09:00:00");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", vault_tree(json!({}))).await;
        let (project, service) = attach(&fs, cx).await;
        assert!(exists(&fs, "2026-10-30").await);

        // The app closes for 45 days.
        drop(service);
        drop(project);
        cx.update(|cx| cx.default_global::<GlobalTodayNoteServices>().0.clear());
        cx.run_until_parked();
        clock.set(at("2026-12-14 09:00:00"));

        attach(&fs, cx).await;
        assert!(
            !exists(&fs, "2026-10-30").await,
            "the page of the last launch day is outside 30 days but inside the record"
        );
        assert_eq!(read(&fs, "2026-12-14").await, template_only("2026-12-14"));
        assert_eq!(record(cx).as_deref(), Some("2026-12-14"));
    }

    #[gpui::test]
    async fn an_open_note_waits(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-30 09:00:00");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            vault_tree(json!({ "2026-10-29.md": template_only("2026-10-29") })),
        )
        .await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let _buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(note_path("2026-10-29"), cx)
            })
            .await
            .unwrap();
        cx.update(|cx| TodayNoteService::attach(vault(), &project, None, cx));
        cx.run_until_parked();

        assert!(
            exists(&fs, "2026-10-29").await,
            "an open page is left alone"
        );
        assert!(exists(&fs, "2026-10-30").await);
    }

    #[gpui::test]
    async fn no_template_means_nothing_happens(cx: &mut TestAppContext) {
        init_test(cx, "2026-10-30 09:00:00");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            json!({
                ".thock": {},
                "daily": { "2026-10-28.md": "" },
            }),
        )
        .await;
        let (_project, service) = attach(&fs, cx).await;

        assert_eq!(service.read_with(cx, |service, _| service.rounds()), 1);
        assert!(!exists(&fs, "2026-10-30").await);
        assert_eq!(read(&fs, "2026-10-28").await, "");
        assert_eq!(record(cx), None);
    }
}
