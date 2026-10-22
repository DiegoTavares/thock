//! The desk notifier (spec `thock/specs/v45-reminders.md` §6.3): rings open
//! reminders through the system's notification center while Thock is open,
//! once per due, and turns *Done*, *In 1 hour* and *Tomorrow 09:00* into
//! home-line edits through the `ReminderService`. One per vault, so two
//! windows ring once.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use chrono::NaiveDateTime;
use gpui::{
    App, AppContext as _, Context, Entity, Global, SharedString, Subscription, SystemNotification,
    SystemNotificationAction, SystemNotificationResponse, Task, TaskExt as _, WeakEntity,
};
use release_channel::ReleaseChannel;
use thock_sync_core::{Due, cut_text, note_link};
use util::ResultExt as _;

use crate::reminder_index::{HomeEdit, IndexedReminder};
use crate::reminder_service::{ReminderEvent, ReminderService};
use crate::reminders::{ReminderClock, due_label};
use crate::vault::VAULT_MARKER_DIR;

/// The longest the notifier sleeps, so a wake from sleep or a clock change
/// rings within a minute.
const CHECK_INTERVAL: Duration = Duration::from_secs(60);
const FIRED_FILE: &str = "cache/reminders/fired.json";
const TAG_PREFIX: &str = "rem:";
const SUMMARY_TAG: &str = "rem:summary";
const ACTION_DONE: &str = "done";
const ACTION_SNOOZE: &str = "snooze";
const ACTION_TOMORROW: &str = "tomorrow";
const ACTION_OPEN: &str = "open";
const TITLE_CHARS: usize = 80;

/// Names the app to the notification center, then routes every answer to the
/// notifier whose vault holds the reminder.
pub fn init(cx: &mut App) {
    let channel = ReleaseChannel::try_global(cx).unwrap_or(ReleaseChannel::Dev);
    cx.set_app_identity(channel.app_id(), channel.display_name());
    cx.on_system_notification_response(|response, cx| {
        let notifiers: Vec<Entity<ReminderNotifier>> = cx
            .try_global::<GlobalNotifiers>()
            .map(|global| global.0.iter().filter_map(WeakEntity::upgrade).collect())
            .unwrap_or_default();
        for notifier in notifiers {
            let handled = notifier.update(cx, |notifier, cx| notifier.answer(&response, cx));
            if handled {
                break;
            }
        }
    });
}

#[derive(Default)]
struct GlobalNotifiers(Vec<WeakEntity<ReminderNotifier>>);

impl Global for GlobalNotifiers {}

pub struct ReminderNotifier {
    service: WeakEntity<ReminderService>,
    fired_path: PathBuf,
    /// `id → due` of every reminder that has rung, so a restart never rings
    /// one twice and a rescheduled one rings again.
    fired: BTreeMap<String, String>,
    fired_loaded: bool,
    /// When this notifier started: reminders due before it came due while
    /// Thock was closed.
    started_at: NaiveDateTime,
    summarized: bool,
    wake: Option<Task<()>>,
    _subscription: Subscription,
}

/// The tag a ring carries: `rem:<id>:<due>`.
pub fn notification_tag(id: &str, due: Due) -> String {
    format!("{TAG_PREFIX}{id}:{due}")
}

fn tag_id(tag: &str) -> Option<&str> {
    let rest = tag.strip_prefix(TAG_PREFIX)?;
    Some(rest.split_once(':').map_or(rest, |(id, _)| id))
}

impl ReminderNotifier {
    /// Starts the notifier for `service`'s vault.
    pub fn new(service: &Entity<ReminderService>, cx: &mut App) -> Entity<Self> {
        let fired_path = service
            .read(cx)
            .vault()
            .root
            .join(VAULT_MARKER_DIR)
            .join(FIRED_FILE);
        let notifier = cx.new(|cx| {
            let subscription = cx.subscribe(service, |this: &mut Self, _, event, cx| {
                if matches!(
                    event,
                    ReminderEvent::IndexChanged | ReminderEvent::DayChanged { .. }
                ) {
                    this.check(cx);
                }
            });
            let mut this = Self {
                service: service.downgrade(),
                fired_path,
                fired: BTreeMap::new(),
                fired_loaded: false,
                started_at: ReminderClock::global(cx).now(),
                summarized: false,
                wake: None,
                _subscription: subscription,
            };
            this.load_fired(cx);
            this
        });
        cx.default_global::<GlobalNotifiers>()
            .0
            .retain(|known| known.upgrade().is_some());
        cx.default_global::<GlobalNotifiers>()
            .0
            .push(notifier.downgrade());
        notifier
    }

    fn fs(&self, cx: &App) -> Option<std::sync::Arc<dyn fs::Fs>> {
        self.service.upgrade()?.read(cx).fs(cx)
    }

    fn load_fired(&mut self, cx: &mut Context<Self>) {
        let Some(fs) = self.fs(cx) else {
            self.fired_loaded = true;
            return;
        };
        let path = self.fired_path.clone();
        cx.spawn(async move |this, cx| {
            let fired = match fs.load(&path).await {
                Ok(text) => serde_json::from_str(&text).log_err().unwrap_or_default(),
                Err(_) => BTreeMap::new(),
            };
            this.update(cx, |this, cx| {
                this.fired = fired;
                this.fired_loaded = true;
                this.check(cx);
            })
            .log_err();
        })
        .detach();
    }

    /// Rings what is due, then sleeps until the next due or a minute.
    fn check(&mut self, cx: &mut Context<Self>) {
        let Some(service) = self.service.upgrade() else {
            return;
        };
        if !service.read(cx).is_loaded() {
            return;
        }
        let clock = ReminderClock::global(cx);
        let now = clock.now();
        let today = now.date();
        let (settings, reminders) = {
            let service = service.read(cx);
            (service.settings().clone(), service.reminders().to_vec())
        };
        let open: Vec<(&IndexedReminder, Due, NaiveDateTime)> = reminders
            .iter()
            .filter(|reminder| reminder.is_open())
            .filter_map(|reminder| {
                let due = reminder.due?;
                Some((reminder, due, due.at(settings.date_only_at)))
            })
            .collect();

        let mut next: Option<NaiveDateTime> = None;
        if self.fired_loaded && settings.ring_on_desk {
            let mut missed = Vec::new();
            let mut changed = false;
            for (reminder, due, at) in &open {
                let due_key = due.to_string();
                if self.fired.get(&reminder.id) == Some(&due_key) {
                    continue;
                }
                if *at > now {
                    next = Some(next.map_or(*at, |next| next.min(*at)));
                    continue;
                }
                if *at < self.started_at && !self.summarized {
                    missed.push(reminder.id.clone());
                } else {
                    cx.show_system_notification(ring(reminder, *due, today, &settings));
                }
                self.fired.insert(reminder.id.clone(), due_key);
                changed = true;
            }
            if !missed.is_empty() {
                let count = missed.len();
                cx.show_system_notification(SystemNotification {
                    tag: SUMMARY_TAG.into(),
                    title: if count == 1 {
                        "1 reminder came due while Thock was closed".into()
                    } else {
                        format!("{count} reminders came due while Thock was closed").into()
                    },
                    body: "They wait in Reminders.".into(),
                    actions: vec![SystemNotificationAction {
                        id: ACTION_OPEN.into(),
                        label: "Open Reminders".into(),
                    }],
                });
            }
            self.summarized = true;
            let before = self.fired.len();
            self.fired
                .retain(|id, _| reminders.iter().any(|reminder| &reminder.id == id));
            if changed || before != self.fired.len() {
                self.save_fired(cx);
            }
        }

        let wait = next
            .map_or(CHECK_INTERVAL, |next| clock.duration_until(next))
            .clamp(Duration::from_millis(1), CHECK_INTERVAL);
        self.wake = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| this.check(cx)).log_err();
        }));
    }

    fn save_fired(&self, cx: &mut Context<Self>) {
        let Some(fs) = self.fs(cx) else { return };
        let Some(text) = serde_json::to_string_pretty(&self.fired).log_err() else {
            return;
        };
        let path = self.fired_path.clone();
        cx.background_spawn(async move {
            if let Some(parent) = path.parent() {
                fs.create_dir(parent).await?;
            }
            fs.atomic_write(path, text).await
        })
        .detach_and_log_err(cx);
    }

    /// Handles an answer to one of this vault's notifications. Returns
    /// whether the notification was this notifier's.
    fn answer(&mut self, response: &SystemNotificationResponse, cx: &mut Context<Self>) -> bool {
        let Some(service) = self.service.upgrade() else {
            return false;
        };
        if response.tag == SUMMARY_TAG {
            service.update(cx, |_, cx| cx.emit(ReminderEvent::ShowReminders));
            return true;
        }
        let Some(id) = tag_id(&response.tag) else {
            return false;
        };
        if service.read(cx).reminder(id).is_none() {
            return false;
        }
        let clock = ReminderClock::global(cx);
        let settings = service.read(cx).settings().clone();
        let edit = match response.action_id.as_deref() {
            Some(ACTION_DONE) => HomeEdit::Close,
            Some(ACTION_SNOOZE) => HomeEdit::Rewrite(settings.snoozed_due(clock.now())),
            Some(ACTION_TOMORROW) => HomeEdit::Rewrite(settings.tomorrow_due(clock.today())),
            _ => {
                service.update(cx, |_, cx| cx.emit(ReminderEvent::ShowReminders));
                return true;
            }
        };
        let id = id.to_string();
        let task: Task<Result<()>> =
            service.update(cx, |service, cx| service.edit_reminder(&id, edit, cx));
        task.detach_and_log_err(cx);
        true
    }
}

/// The notification for one reminder: its text as the title, when and where
/// as the body (`14:30 · projects/house`, `today · reminders`).
fn ring(
    reminder: &IndexedReminder,
    due: Due,
    today: chrono::NaiveDate,
    settings: &crate::reminders::ReminderSettings,
) -> SystemNotification {
    let when = match due.time {
        Some(time) if due.date == today => time.format("%H:%M").to_string(),
        None if due.date == today => "today".to_string(),
        _ => due_label(due, today),
    };
    let text = reminder.text.as_deref().unwrap_or("Reminder");
    SystemNotification {
        tag: notification_tag(&reminder.id, due).into(),
        title: SharedString::from(cut_text(text, TITLE_CHARS)),
        body: format!("{when} · {}", note_link(&reminder.home)).into(),
        actions: vec![
            SystemNotificationAction {
                id: ACTION_DONE.into(),
                label: "Done".into(),
            },
            SystemNotificationAction {
                id: ACTION_SNOOZE.into(),
                label: "In 1 hour".into(),
            },
            SystemNotificationAction {
                id: ACTION_TOMORROW.into(),
                label: format!("Tomorrow {}", settings.tomorrow_at.format("%H:%M")).into(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reminder_service::clear_services_for_test;
    use crate::reminders::{TestClock, WallClock as _};
    use crate::vault::{Vault, VaultConfig};
    use fs::{FakeFs, Fs as _};
    use gpui::TestAppContext;
    use project::Project;
    use serde_json::json;
    use settings::SettingsStore;
    use std::path::Path;
    use std::sync::Arc;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()
    }

    struct Setup {
        fs: Arc<FakeFs>,
        project: Entity<Project>,
        service: Entity<ReminderService>,
        clock: Arc<TestClock>,
    }

    async fn setup(cx: &mut TestAppContext, start: &str, tree: serde_json::Value) -> Setup {
        let clock = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            init(cx);
            TestClock::install(at(start), cx)
        });
        assert!(cx.app_identity().is_some(), "identity comes first");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", tree).await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let service = attach(&project, cx);
        settle(cx);
        Setup {
            fs,
            project,
            service,
            clock,
        }
    }

    fn attach(project: &Entity<Project>, cx: &mut TestAppContext) -> Entity<ReminderService> {
        cx.update(|cx| {
            ReminderService::attach(
                Vault {
                    root: "/vault".into(),
                    config: VaultConfig::default(),
                },
                project,
                None,
                cx,
            )
        })
    }

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..12 {
            cx.executor().advance_clock(Duration::from_millis(500));
            cx.run_until_parked();
        }
    }

    fn advance(cx: &mut TestAppContext, by: Duration) {
        let mut left = by;
        while !left.is_zero() {
            let step = left.min(Duration::from_secs(30));
            cx.executor().advance_clock(step);
            cx.run_until_parked();
            left -= step;
        }
        settle(cx);
    }

    fn tags(cx: &TestAppContext) -> Vec<String> {
        cx.shown_system_notifications()
            .into_iter()
            .map(|notification| notification.tag.to_string())
            .collect()
    }

    const HOUSE: &str = "- [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->\nAsk about the boiler (@2026-10-22) <!--rem:7c11e0b4d2a9-->\n";
    const INDEX: &str = "## Open\n- [[projects/house]] <!--rem:7d3e55a1c0f2-->\n- [[projects/house]] <!--rem:7c11e0b4d2a9-->\n\n## Done\n";

    fn vault_tree() -> serde_json::Value {
        json!({
            ".thock": {},
            "projects": { "house.md": HOUSE },
            "reminders.md": INDEX,
        })
    }

    #[gpui::test]
    async fn rings_at_the_due_minute_and_date_only_at_nine(cx: &mut TestAppContext) {
        let _setup = setup(cx, "2026-10-21 14:05", vault_tree()).await;
        assert!(tags(cx).is_empty());

        advance(cx, Duration::from_secs(24 * 60));
        assert!(tags(cx).is_empty(), "14:29 is early");
        advance(cx, Duration::from_secs(60));
        let shown = cx.shown_system_notifications();
        assert_eq!(shown.len(), 1);
        let ring = &shown[0];
        assert_eq!(ring.tag.as_ref(), "rem:7d3e55a1c0f2:2026-10-21 14:30");
        assert_eq!(ring.title.as_ref(), "Call the plumber");
        assert_eq!(ring.body.as_ref(), "14:30 · projects/house");
        let labels: Vec<&str> = ring
            .actions
            .iter()
            .map(|action| action.label.as_ref())
            .collect();
        assert_eq!(labels, ["Done", "In 1 hour", "Tomorrow 09:00"]);

        // The date-only one rings at 09:00 on its day.
        advance(cx, Duration::from_secs((18 * 60 + 29) * 60));
        assert_eq!(tags(cx).len(), 1, "08:59 the next day");
        advance(cx, Duration::from_secs(60));
        assert_eq!(
            tags(cx).last().map(String::as_str),
            Some("rem:7c11e0b4d2a9:2026-10-22")
        );
        assert_eq!(
            cx.shown_system_notifications()
                .last()
                .map(|ring| ring.body.to_string()),
            Some("today · projects/house".to_string())
        );
    }

    #[gpui::test]
    async fn a_restart_never_rings_twice_and_misses_are_summed_up(cx: &mut TestAppContext) {
        let setup = setup(cx, "2026-10-22 10:00", vault_tree()).await;
        // Both came due before the desk opened: one summary, no burst.
        let shown = cx.shown_system_notifications();
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!(shown[0].tag.as_ref(), SUMMARY_TAG);
        assert_eq!(
            shown[0].title.as_ref(),
            "2 reminders came due while Thock was closed"
        );
        assert!(
            setup
                .fs
                .is_file(Path::new("/vault/.thock/cache/reminders/fired.json"))
                .await
        );

        let Setup {
            project, service, ..
        } = setup;
        drop(service);
        cx.update(clear_services_for_test);
        cx.run_until_parked();
        let _service = attach(&project, cx);
        settle(cx);
        advance(cx, Duration::from_secs(120));
        assert_eq!(
            cx.shown_system_notifications().len(),
            1,
            "nothing rings again"
        );
    }

    #[gpui::test]
    async fn each_answer_edits_the_home_line(cx: &mut TestAppContext) {
        let setup = setup(cx, "2026-10-21 14:29", vault_tree()).await;
        advance(cx, Duration::from_secs(60));
        let tag = "rem:7d3e55a1c0f2:2026-10-21 14:30";
        assert_eq!(tags(cx), [tag]);
        let house = || async {
            setup
                .fs
                .load(Path::new("/vault/projects/house.md"))
                .await
                .unwrap()
        };

        let snoozed = setup.service.read_with(cx, |service, _| {
            service.settings().snoozed_due(setup.clock.now())
        });
        cx.simulate_system_notification_response(SystemNotificationResponse {
            tag: tag.into(),
            action_id: Some(ACTION_SNOOZE.into()),
        });
        settle(cx);
        assert!(
            house().await.starts_with(&format!(
                "- [ ] Call the plumber (@{snoozed}) <!--rem:7d3e55a1c0f2-->"
            )),
            "{}",
            house().await
        );

        cx.simulate_system_notification_response(SystemNotificationResponse {
            tag: format!("rem:7d3e55a1c0f2:{snoozed}").into(),
            action_id: Some(ACTION_TOMORROW.into()),
        });
        settle(cx);
        assert!(
            house()
                .await
                .starts_with("- [ ] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->")
        );

        cx.simulate_system_notification_response(SystemNotificationResponse {
            tag: "rem:7d3e55a1c0f2:2026-10-22 09:00".into(),
            action_id: Some(ACTION_DONE.into()),
        });
        settle(cx);
        assert!(
            house()
                .await
                .starts_with("- [x] Call the plumber (@2026-10-22 09:00) <!--rem:7d3e55a1c0f2-->")
        );
    }

    #[gpui::test]
    async fn desk_false_stays_silent(cx: &mut TestAppContext) {
        let _setup = setup(
            cx,
            "2026-10-21 14:29",
            json!({
                ".thock": { "reminders.toml": "[ring]\ndesk = false\n" },
                "projects": { "house.md": HOUSE },
                "reminders.md": INDEX,
            }),
        )
        .await;
        advance(cx, Duration::from_secs(120));
        assert!(tags(cx).is_empty());
    }

    #[test]
    fn tags_carry_the_id_and_due() {
        let due = Due::parse("2026-10-21 14:30").unwrap();
        let tag = notification_tag("7d3e55a1c0f2", due);
        assert_eq!(tag, "rem:7d3e55a1c0f2:2026-10-21 14:30");
        assert_eq!(tag_id(&tag), Some("7d3e55a1c0f2"));
        assert_eq!(tag_id("gcal:x"), None);
    }
}
