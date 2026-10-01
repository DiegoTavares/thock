//! One sync indicator for every connector (spec `v32-sync-status-indicator.md`):
//! an icon in the status bar whose popover lists Calendar, Gmail, Inbox, and
//! Readwise with each one's action, plus the status model both that popover
//! and the panels' inline rows render from. A panel keeps a status row only
//! while the user has to act on it; the icon carries a dot for the same
//! states, so nothing broken hides behind a click.

use gpui::{
    Action, Anchor, AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, KeyContext, SharedString, Subscription, Task, Window, actions,
};
use menu::{
    Cancel, Confirm, SelectChild, SelectFirst, SelectLast, SelectNext, SelectParent, SelectPrevious,
};
use project::Project;
use std::rc::Rc;
use std::time::Duration;
use ui::prelude::*;
use ui::{
    Button, ButtonLike, CommonAnimationExt as _, Icon, IconName, IconSize, IconWithIndicator,
    Indicator, Label, LabelSize, PopoverMenu, PopoverMenuHandle, Tooltip,
};
use util::ResultExt as _;
use workspace::{HideStatusItem, ItemHandle, StatusItemView, Workspace};

use crate::calendar_service::{
    self, AddPlannerHeading, CalendarService, ChoosePlannerHeading, ConnectGoogleWorkspace,
    HoldReason, SyncCalendarNow, SyncState,
};
use crate::gmail_service::{self, GmailService, SyncGmailNow};
use crate::inbox_service::{self, InboxService, SyncInboxNow, TriageInbox};
use crate::readwise_service::{self, ConnectReadwise, ReadwiseService, SyncReadwiseNow};

/// How often an open popover re-renders so "synced 2m ago" keeps up.
const RELATIVE_TIME_REFRESH: Duration = Duration::from_secs(30);

actions!(
    thock,
    [
        /// Shows how each connected service — Calendar, Gmail, Inbox,
        /// Readwise — is doing, with a fix for anything that needs one.
        ToggleSyncStatus
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleSyncStatus, window, cx| {
            let Some(indicator) = workspace
                .status_bar()
                .read(cx)
                .item_of_type::<SyncStatusIndicator>()
            else {
                return;
            };
            // Toggling builds the popover, which reads the indicator, so the
            // handle is taken out before the indicator could be mid-update.
            let popover_handle = indicator.read(cx).popover_handle.clone();
            popover_handle.toggle(window, cx);
        });
    })
    .detach();
}

/// The connectors in the order their rows are listed, so rows never jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Connector {
    Calendar,
    Gmail,
    Inbox,
    Readwise,
}

impl Connector {
    pub fn name(self) -> &'static str {
        match self {
            Self::Calendar => "Calendar",
            Self::Gmail => "Gmail",
            Self::Inbox => "Inbox",
            Self::Readwise => "Readwise",
        }
    }
}

/// How much a connector's state needs the user, from nothing at all to a
/// sync that has stopped. Ordered so the worst one can be picked for the icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attention {
    #[default]
    None,
    /// Healthy, but something is waiting on the user (inbox items to triage).
    Waiting,
    /// The user can fix it: not connected yet, a missing heading or label.
    Warning,
    /// Sync has stopped: a failure or a lost sign-in.
    Error,
}

/// The named action a row's button runs. Every one is also in the command
/// palette, so no row is reachable only by mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    SyncCalendarNow,
    SyncGmailNow,
    SyncInboxNow,
    SyncReadwiseNow,
    ConnectGoogleWorkspace,
    ConnectReadwise,
    AddPlannerHeading,
    ChoosePlannerHeading,
    TriageInbox,
}

impl ActionKind {
    fn boxed(self) -> Box<dyn Action> {
        match self {
            Self::SyncCalendarNow => SyncCalendarNow.boxed_clone(),
            Self::SyncGmailNow => SyncGmailNow.boxed_clone(),
            Self::SyncInboxNow => SyncInboxNow.boxed_clone(),
            Self::SyncReadwiseNow => SyncReadwiseNow.boxed_clone(),
            Self::ConnectGoogleWorkspace => ConnectGoogleWorkspace.boxed_clone(),
            Self::ConnectReadwise => ConnectReadwise.boxed_clone(),
            Self::AddPlannerHeading => AddPlannerHeading.boxed_clone(),
            Self::ChoosePlannerHeading => ChoosePlannerHeading.boxed_clone(),
            Self::TriageInbox => TriageInbox.boxed_clone(),
        }
    }

    /// A sync is something to watch from the popover; everything else takes
    /// the user somewhere (a sign-in, a prompt, a ritual), so the popover
    /// gets out of the way.
    fn keeps_popover_open(self) -> bool {
        matches!(
            self,
            Self::SyncCalendarNow | Self::SyncGmailNow | Self::SyncInboxNow | Self::SyncReadwiseNow
        )
    }
}

/// A row's button: an existing action under the label this state gives it
/// ("Retry" and "Sync now" are the same action).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorAction {
    pub label: SharedString,
    pub kind: ActionKind,
}

impl ConnectorAction {
    fn new(kind: ActionKind, label: &'static str) -> Self {
        Self {
            label: label.into(),
            kind,
        }
    }

    pub fn dispatch(&self, window: &mut Window, cx: &mut App) {
        window.dispatch_action(self.kind.boxed(), cx);
    }
}

/// What one connector's row says and offers, however it is rendered.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectorStatus {
    pub connector: Connector,
    /// The sentence after the connector's name: "synced 2m ago", "3 waiting".
    pub summary: SharedString,
    /// The tooltip: the error text, or what to do about a hold.
    pub detail: Option<SharedString>,
    pub attention: Attention,
    /// Mid-flight (connecting, importing a library): the icon spins.
    pub busy: bool,
    /// The first action is the primary one, run on `enter`.
    pub actions: Vec<ConnectorAction>,
}

impl ConnectorStatus {
    fn new(connector: Connector, summary: impl Into<SharedString>) -> Self {
        Self {
            connector,
            summary: summary.into(),
            detail: None,
            attention: Attention::None,
            busy: false,
            actions: Vec::new(),
        }
    }

    fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn attention(mut self, attention: Attention) -> Self {
        self.attention = attention;
        self
    }

    fn busy(mut self) -> Self {
        self.busy = true;
        self
    }

    fn action(mut self, kind: ActionKind, label: &'static str) -> Self {
        self.actions.push(ConnectorAction::new(kind, label));
        self
    }

    /// Whether the row belongs in its panel too, not only in the popover.
    pub fn needs_user(&self) -> bool {
        self.attention != Attention::None
    }

    pub fn primary_action(&self) -> Option<&ConnectorAction> {
        self.actions.first()
    }
}

/// The Calendar row (V8 §10.3, V26 §7.3). `None` without a config.
pub fn status_for_calendar(state: &SyncState) -> Option<ConnectorStatus> {
    let connector = Connector::Calendar;
    let sync_now = ActionKind::SyncCalendarNow;
    let connect = ActionKind::ConnectGoogleWorkspace;
    Some(match state {
        SyncState::NoConfig => return None,
        SyncState::NeverConnected => ConnectorStatus::new(connector, "not connected")
            .attention(Attention::Warning)
            .action(connect, "Connect Google Workspace"),
        SyncState::Connecting => ConnectorStatus::new(connector, "connecting…").busy(),
        SyncState::Idle => {
            ConnectorStatus::new(connector, "waiting for first sync").action(sync_now, "Sync now")
        }
        SyncState::Synced { at } => {
            ConnectorStatus::new(connector, format!("synced {}", format_ago(at.elapsed())))
                .action(sync_now, "Sync now")
        }
        // A structural hold is a question with an answer, so it comes with
        // the buttons that answer it; a plain wait clears on its own.
        SyncState::Holding { reason } => {
            let status = ConnectorStatus::new(connector, reason.summary());
            let status = match reason.detail() {
                Some(detail) => status.detail(detail),
                None => status,
            };
            match reason {
                HoldReason::Waiting(_) => status.action(sync_now, "Sync now"),
                HoldReason::NoPlannerHeading { .. } => status
                    .attention(Attention::Warning)
                    .action(ActionKind::AddPlannerHeading, "Add heading")
                    .action(ActionKind::ChoosePlannerHeading, "Use another…"),
                HoldReason::PlannerHeadingTooDeep { .. } => status
                    .attention(Attention::Warning)
                    .action(sync_now, "Sync now"),
            }
        }
        SyncState::Failing { error } => ConnectorStatus::new(connector, "sync failed")
            .detail(error.clone())
            .attention(Attention::Error)
            .action(sync_now, "Retry"),
        SyncState::Disconnected => ConnectorStatus::new(connector, "sign-in expired")
            .attention(Attention::Error)
            .action(connect, "Reconnect"),
    })
}

/// The Gmail row (V9 §10.3, V15 §7.3). A Gmail hold always names a label
/// the user can create, so it is a warning rather than a wait.
pub fn status_for_gmail(state: &SyncState) -> Option<ConnectorStatus> {
    let connector = Connector::Gmail;
    let sync_now = ActionKind::SyncGmailNow;
    let connect = ActionKind::ConnectGoogleWorkspace;
    Some(match state {
        SyncState::NoConfig => return None,
        SyncState::NeverConnected => ConnectorStatus::new(connector, "not connected")
            .attention(Attention::Warning)
            .action(connect, "Connect Google Workspace"),
        SyncState::Connecting => ConnectorStatus::new(connector, "connecting…").busy(),
        SyncState::Idle => {
            ConnectorStatus::new(connector, "waiting for first check").action(sync_now, "Sync now")
        }
        SyncState::Synced { at } => {
            ConnectorStatus::new(connector, format!("checked {}", format_ago(at.elapsed())))
                .action(sync_now, "Sync now")
        }
        SyncState::Holding { reason } => ConnectorStatus::new(connector, reason.summary())
            .attention(Attention::Warning)
            .action(sync_now, "Sync now"),
        SyncState::Failing { error } => ConnectorStatus::new(connector, "sync failed")
            .detail(error.clone())
            .attention(Attention::Error)
            .action(sync_now, "Retry"),
        SyncState::Disconnected => ConnectorStatus::new(connector, "sign-in needed")
            .attention(Attention::Error)
            .action(connect, "Reconnect"),
    })
}

/// The Inbox row (V13 §10.4). Items waiting are the way into triage. When
/// Gmail already offers the Google connect button, this row only points at
/// it, so the button shows once.
pub fn status_for_inbox(
    state: &SyncState,
    queue_depth: usize,
    gmail_offers_connect: bool,
) -> Option<ConnectorStatus> {
    let connector = Connector::Inbox;
    let sync_now = ActionKind::SyncInboxNow;
    let connect = ActionKind::ConnectGoogleWorkspace;
    Some(match state {
        SyncState::NoConfig => return None,
        SyncState::NeverConnected if gmail_offers_connect => {
            ConnectorStatus::new(connector, "via Google Workspace")
        }
        SyncState::NeverConnected => ConnectorStatus::new(connector, "not connected")
            .attention(Attention::Warning)
            .action(connect, "Connect Google Workspace"),
        SyncState::Connecting => ConnectorStatus::new(connector, "connecting…").busy(),
        SyncState::Idle => {
            ConnectorStatus::new(connector, "waiting for first check").action(sync_now, "Sync now")
        }
        SyncState::Synced { .. } if queue_depth == 0 => {
            ConnectorStatus::new(connector, "empty").action(sync_now, "Sync now")
        }
        SyncState::Synced { .. } => {
            ConnectorStatus::new(connector, format!("{queue_depth} waiting"))
                .attention(Attention::Waiting)
                .action(ActionKind::TriageInbox, "Triage")
                .action(sync_now, "Sync now")
        }
        SyncState::Holding { reason } => ConnectorStatus::new(connector, reason.summary())
            .attention(Attention::Warning)
            .action(sync_now, "Sync now"),
        SyncState::Failing { error } => ConnectorStatus::new(connector, "sync failed")
            .detail(error.clone())
            .attention(Attention::Error)
            .action(sync_now, "Retry"),
        SyncState::Disconnected => ConnectorStatus::new(connector, "sign-in expired")
            .attention(Attention::Error)
            .action(connect, "Reconnect"),
    })
}

/// What the Readwise row needs beyond the sync state.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadwiseExtras<'a> {
    pub importing_library: bool,
    pub last_landed: usize,
    pub config_error: Option<&'a SharedString>,
}

/// The Readwise row (V31 §8.5). A config problem replaces the sync state.
pub fn status_for_readwise(state: &SyncState, extras: ReadwiseExtras) -> Option<ConnectorStatus> {
    let connector = Connector::Readwise;
    let sync_now = ActionKind::SyncReadwiseNow;
    let connect = ActionKind::ConnectReadwise;
    if matches!(state, SyncState::NoConfig) {
        return None;
    }
    if let Some(error) = extras.config_error {
        return Some(
            ConnectorStatus::new(connector, error.clone())
                .detail("Fix it in .thock/readwise.toml.")
                .attention(Attention::Warning),
        );
    }
    Some(match state {
        SyncState::NoConfig => return None,
        SyncState::NeverConnected => ConnectorStatus::new(connector, "not connected")
            .attention(Attention::Warning)
            .action(connect, "Connect Readwise"),
        SyncState::Connecting => ConnectorStatus::new(connector, "connecting…").busy(),
        SyncState::Idle if extras.importing_library => {
            ConnectorStatus::new(connector, "importing your library…").busy()
        }
        SyncState::Idle => ConnectorStatus::new(connector, "checking…"),
        SyncState::Synced { at } => {
            let mut summary = format!("synced {}", format_ago(at.elapsed()));
            match extras.last_landed {
                0 => {}
                1 => summary.push_str(" · +1 highlight"),
                landed => summary.push_str(&format!(" · +{landed} highlights")),
            }
            ConnectorStatus::new(connector, summary).action(sync_now, "Sync now")
        }
        SyncState::Holding { reason } => ConnectorStatus::new(connector, reason.summary())
            .attention(Attention::Warning)
            .action(sync_now, "Sync now"),
        SyncState::Failing { error } => ConnectorStatus::new(connector, "sync failed")
            .detail(error.clone())
            .attention(Attention::Error)
            .action(sync_now, "Retry"),
        SyncState::Disconnected => ConnectorStatus::new(connector, "token rejected")
            .attention(Attention::Error)
            .action(connect, "Reconnect"),
    })
}

pub fn calendar_status(service: &CalendarService) -> Option<ConnectorStatus> {
    status_for_calendar(service.state())
}

pub fn gmail_status(service: &GmailService) -> Option<ConnectorStatus> {
    status_for_gmail(service.state())
}

pub fn inbox_status(
    service: &InboxService,
    gmail: Option<&GmailService>,
) -> Option<ConnectorStatus> {
    let gmail_offers_connect =
        gmail.is_some_and(|gmail| matches!(gmail.state(), SyncState::NeverConnected));
    status_for_inbox(service.state(), service.queue_depth(), gmail_offers_connect)
}

pub fn readwise_status(service: &ReadwiseService) -> Option<ConnectorStatus> {
    status_for_readwise(
        service.state(),
        ReadwiseExtras {
            importing_library: service.importing_library(),
            last_landed: service.last_landed(),
            config_error: service.config_error(),
        },
    )
}

/// The rows a panel keeps at its top: only the ones the user has to act on.
pub fn inline_statuses(
    statuses: impl IntoIterator<Item = Option<ConnectorStatus>>,
) -> Vec<ConnectorStatus> {
    statuses
        .into_iter()
        .flatten()
        .filter(ConnectorStatus::needs_user)
        .collect()
}

pub fn worst_attention(statuses: &[ConnectorStatus]) -> Attention {
    statuses
        .iter()
        .map(|status| status.attention)
        .max()
        .unwrap_or_default()
}

/// What the status-bar icon says without opening anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glance {
    pub attention: Attention,
    pub busy: bool,
    /// "All synced", or the first connector that needs the user.
    pub tooltip: SharedString,
}

impl Glance {
    /// `None` when no connector is configured, which hides the icon.
    pub fn from_statuses(statuses: &[ConnectorStatus]) -> Option<Self> {
        if statuses.is_empty() {
            return None;
        }
        let tooltip = statuses
            .iter()
            .find(|status| status.needs_user())
            .map(|status| format!("{} · {}", status.connector.name(), status.summary).into())
            .unwrap_or_else(|| "All synced".into());
        Some(Self {
            attention: worst_attention(statuses),
            busy: statuses.iter().any(|status| status.busy),
            tooltip,
        })
    }
}

pub(crate) fn format_ago(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0 => "just now".to_string(),
        1..=59 => format!("{minutes}m ago"),
        _ => format!("{}h ago", minutes / 60),
    }
}

/// Where a row is drawn, which decides its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPlacement {
    /// At the top of a panel: the connector's name prefixes the summary.
    Inline,
    /// In the popover: a name column, and the keyboard cursor.
    Popover {
        selected: bool,
        selected_action: usize,
    },
}

type RunAction = Rc<dyn Fn(&ConnectorAction, &mut Window, &mut App)>;

fn attention_color(attention: Attention) -> Color {
    match attention {
        Attention::None => Color::Muted,
        Attention::Waiting => Color::Accent,
        Attention::Warning => Color::Warning,
        Attention::Error => Color::Error,
    }
}

/// A panel's inline row: the status with its name, and its buttons
/// dispatching their actions directly.
pub fn render_inline_row(status: &ConnectorStatus, cx: &App) -> AnyElement {
    render_connector_row(
        status,
        RowPlacement::Inline,
        Rc::new(|action, window, cx| action.dispatch(window, cx)),
        cx,
    )
}

/// The one row renderer behind the popover and the panels. `run` is what a
/// button does with its action, so the popover can close itself after.
pub fn render_connector_row(
    status: &ConnectorStatus,
    placement: RowPlacement,
    run: RunAction,
    cx: &App,
) -> AnyElement {
    let name = status.connector.name();
    let color = attention_color(status.attention);
    let (summary_text, selected_action) = match placement {
        RowPlacement::Inline => (format!("{name} · {}", status.summary), None),
        RowPlacement::Popover {
            selected,
            selected_action,
        } => (
            status.summary.to_string(),
            selected.then_some(selected_action),
        ),
    };
    let summary = Label::new(summary_text).size(LabelSize::Small).color(color);
    let summary = match &status.detail {
        Some(detail) => div()
            .id(SharedString::from(format!("thock-sync-{name}-summary")))
            .child(summary)
            .tooltip(Tooltip::text(detail.clone()))
            .into_any_element(),
        None => summary.into_any_element(),
    };
    let buttons = status.actions.iter().enumerate().map(|(index, action)| {
        let run = run.clone();
        let action = action.clone();
        Button::new(
            SharedString::from(format!("thock-sync-{name}-action-{index}")),
            action.label.clone(),
        )
        .label_size(LabelSize::Small)
        .toggle_state(selected_action == Some(index))
        .on_click(move |_, window, cx| run(&action, window, cx))
    });

    match placement {
        RowPlacement::Inline => h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(summary)
            .child(h_flex().gap_1().children(buttons))
            .into_any_element(),
        RowPlacement::Popover { selected, .. } => h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .rounded_sm()
            .when(selected, |this| {
                this.bg(cx.theme().colors().element_selected)
            })
            .child(
                div()
                    .w(px(72.))
                    .flex_none()
                    .child(Label::new(name).size(LabelSize::Small)),
            )
            .child(div().flex_1().min_w_0().child(summary))
            .child(h_flex().flex_none().gap_1().children(buttons))
            .into_any_element(),
    }
}

/// The status-bar item: hidden until a connector has a config, then an icon
/// that spins while something is in flight and carries a dot for the worst
/// attention. Clicking it (or `thock::ToggleSyncStatus`) opens the popover.
pub struct SyncStatusIndicator {
    project: Entity<Project>,
    calendar: Option<Entity<CalendarService>>,
    gmail: Option<Entity<GmailService>>,
    inbox: Option<Entity<InboxService>>,
    readwise: Option<Entity<ReadwiseService>>,
    popover_handle: PopoverMenuHandle<SyncStatusPopover>,
    _subscriptions: Vec<Subscription>,
}

impl SyncStatusIndicator {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        Self::for_project(workspace.project().clone(), cx)
    }

    pub fn for_project(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let project_subscription = cx.subscribe(&project, |this, _, event, cx| {
            if matches!(
                event,
                project::Event::WorktreeAdded(_)
                    | project::Event::WorktreeRemoved(_)
                    | project::Event::WorktreeUpdatedEntries(..)
            ) {
                this.resolve_services(cx);
                cx.notify();
            }
        });
        let mut this = Self {
            project,
            calendar: None,
            gmail: None,
            inbox: None,
            readwise: None,
            popover_handle: PopoverMenuHandle::default(),
            _subscriptions: vec![project_subscription],
        };
        this.resolve_services(cx);
        this
    }

    /// The services are created by their own workspace observers, which run
    /// after the status bar is built, so missing ones are looked up again on
    /// every later trigger until all four are found.
    fn resolve_services(&mut self, cx: &mut Context<Self>) {
        let project = self.project.clone();
        Self::resolve(
            &mut self.calendar,
            calendar_service::service_for_project(&project, cx),
            &mut self._subscriptions,
            cx,
        );
        Self::resolve(
            &mut self.gmail,
            gmail_service::service_for_project(&project, cx),
            &mut self._subscriptions,
            cx,
        );
        Self::resolve(
            &mut self.inbox,
            inbox_service::service_for_project(&project, cx),
            &mut self._subscriptions,
            cx,
        );
        Self::resolve(
            &mut self.readwise,
            readwise_service::service_for_project(&project, cx),
            &mut self._subscriptions,
            cx,
        );
    }

    fn resolve<T: 'static>(
        slot: &mut Option<Entity<T>>,
        found: Option<Entity<T>>,
        subscriptions: &mut Vec<Subscription>,
        cx: &mut Context<Self>,
    ) {
        if slot.is_some() {
            return;
        }
        if let Some(service) = found {
            subscriptions.push(cx.observe(&service, |_, _, cx| cx.notify()));
            *slot = Some(service);
        }
    }

    /// Every configured connector's row, in display order.
    pub fn statuses(&self, cx: &App) -> Vec<ConnectorStatus> {
        let gmail = self.gmail.as_ref().map(|service| service.read(cx));
        let mut statuses = Vec::new();
        statuses.extend(
            self.calendar
                .as_ref()
                .and_then(|service| calendar_status(service.read(cx))),
        );
        statuses.extend(gmail.and_then(gmail_status));
        statuses.extend(
            self.inbox
                .as_ref()
                .and_then(|service| inbox_status(service.read(cx), gmail)),
        );
        statuses.extend(
            self.readwise
                .as_ref()
                .and_then(|service| readwise_status(service.read(cx))),
        );
        statuses
    }

    pub fn glance(&self, cx: &App) -> Option<Glance> {
        Glance::from_statuses(&self.statuses(cx))
    }

    fn dot(attention: Attention) -> Option<Indicator> {
        match attention {
            Attention::None => None,
            attention => Some(Indicator::dot().color(attention_color(attention))),
        }
    }
}

impl Render for SyncStatusIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.resolve_services(cx);
        let Some(glance) = self.glance(cx) else {
            return div().hidden().into_any_element();
        };
        let icon = Icon::new(IconName::ArrowCircle)
            .size(IconSize::Small)
            .color(Color::Muted);
        let icon = if glance.busy {
            icon.with_rotate_animation(2).into_any_element()
        } else {
            IconWithIndicator::new(icon, Self::dot(glance.attention))
                .indicator_border_color(Some(cx.theme().colors().status_bar_background))
                .into_any_element()
        };
        let indicator = cx.weak_entity();
        let tooltip = glance.tooltip;
        div()
            .child(
                PopoverMenu::new("thock-sync-status")
                    .menu(move |window, cx| {
                        let indicator = indicator.upgrade()?;
                        Some(cx.new(|cx| SyncStatusPopover::new(indicator, window, cx)))
                    })
                    .anchor(Anchor::BottomRight)
                    .with_handle(self.popover_handle.clone())
                    .trigger_with_tooltip(
                        ButtonLike::new("thock-sync-status-button").child(icon),
                        move |_window, cx| {
                            Tooltip::for_action(tooltip.clone(), &ToggleSyncStatus, cx)
                        },
                    ),
            )
            .into_any_element()
    }
}

impl StatusItemView for SyncStatusIndicator {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resolve_services(cx);
    }

    /// The icon already hides itself whenever no connector is configured.
    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        None
    }
}

/// The popover under the icon: one row per configured connector, with a
/// keyboard cursor that follows a connector rather than a position, so a
/// row appearing above it doesn't move it onto a different connector.
pub struct SyncStatusPopover {
    indicator: Entity<SyncStatusIndicator>,
    focus_handle: FocusHandle,
    selected: Option<Connector>,
    selected_action: usize,
    _subscriptions: Vec<Subscription>,
    _relative_time_refresh: Task<()>,
}

impl SyncStatusPopover {
    pub fn new(
        indicator: Entity<SyncStatusIndicator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.observe(&indicator, |_, _, cx| cx.notify()),
            cx.on_focus_out(&focus_handle, window, |_, _, _, cx| {
                cx.emit(DismissEvent);
            }),
        ];
        let relative_time_refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(RELATIVE_TIME_REFRESH).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            indicator,
            focus_handle,
            selected: None,
            selected_action: 0,
            _subscriptions: subscriptions,
            _relative_time_refresh: relative_time_refresh,
        }
    }

    fn statuses(&self, cx: &App) -> Vec<ConnectorStatus> {
        self.indicator.read(cx).statuses(cx)
    }

    /// The cursor's row: the selected connector if it is still listed, else
    /// the first row.
    fn selected_index(&self, statuses: &[ConnectorStatus]) -> Option<usize> {
        if statuses.is_empty() {
            return None;
        }
        Some(
            self.selected
                .and_then(|connector| {
                    statuses
                        .iter()
                        .position(|status| status.connector == connector)
                })
                .unwrap_or(0),
        )
    }

    pub fn selected_connector(&self, cx: &App) -> Option<Connector> {
        let statuses = self.statuses(cx);
        self.selected_index(&statuses)
            .and_then(|index| statuses.get(index))
            .map(|status| status.connector)
    }

    fn select(&mut self, statuses: &[ConnectorStatus], index: usize, cx: &mut Context<Self>) {
        let Some(status) = statuses.get(index) else {
            return;
        };
        if self.selected != Some(status.connector) {
            self.selected = Some(status.connector);
            self.selected_action = 0;
        }
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        let statuses = self.statuses(cx);
        if let Some(index) = self.selected_index(&statuses) {
            self.select(&statuses, (index + 1).min(statuses.len() - 1), cx);
        }
    }

    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let statuses = self.statuses(cx);
        if let Some(index) = self.selected_index(&statuses) {
            self.select(&statuses, index.saturating_sub(1), cx);
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _window: &mut Window, cx: &mut Context<Self>) {
        let statuses = self.statuses(cx);
        self.select(&statuses, 0, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        let statuses = self.statuses(cx);
        self.select(&statuses, statuses.len().saturating_sub(1), cx);
    }

    fn selected_action_count(&self, cx: &App) -> usize {
        let statuses = self.statuses(cx);
        self.selected_index(&statuses)
            .map(|index| statuses[index].actions.len())
            .unwrap_or(0)
    }

    fn select_next_action(
        &mut self,
        _: &SelectChild,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.selected_action_count(cx);
        if count > 0 {
            self.selected_action = (self.selected_action + 1).min(count - 1);
            cx.notify();
        }
    }

    fn select_previous_action(
        &mut self,
        _: &SelectParent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_action = self.selected_action.saturating_sub(1);
        cx.notify();
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let statuses = self.statuses(cx);
        let Some(index) = self.selected_index(&statuses) else {
            return;
        };
        let actions = &statuses[index].actions;
        let Some(action) = actions
            .get(self.selected_action)
            .or_else(|| actions.first())
        else {
            return;
        };
        self.run(action, window, cx);
    }

    fn run(&mut self, action: &ConnectorAction, window: &mut Window, cx: &mut Context<Self>) {
        action.dispatch(window, cx);
        if !action.kind.keeps_popover_open() {
            cx.emit(DismissEvent);
        }
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for SyncStatusPopover {}

impl Focusable for SyncStatusPopover {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SyncStatusPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let statuses = self.statuses(cx);
        let selected_index = self.selected_index(&statuses);
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("ThockSyncStatus");
        key_context.add("menu");
        let popover = cx.weak_entity();
        let run: RunAction = Rc::new(move |action, window, cx| {
            popover
                .update(cx, |this, cx| this.run(action, window, cx))
                .log_err();
        });
        v_flex()
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .occlude()
            .elevation_2(cx)
            .min_w(px(340.))
            .p_1()
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::select_next_action))
            .on_action(cx.listener(Self::select_previous_action))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .when(statuses.is_empty(), |this| {
                this.child(
                    div().px_2().py_1().child(
                        Label::new("Nothing is connected yet.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .children(statuses.iter().enumerate().map(|(index, status)| {
                let selected = selected_index == Some(index);
                let selected_action = self
                    .selected_action
                    .min(status.actions.len().saturating_sub(1));
                render_connector_row(
                    status,
                    RowPlacement::Popover {
                        selected,
                        selected_action,
                    },
                    run.clone(),
                    cx,
                )
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::{FakeFs, Fs as _};
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use settings::SettingsStore;
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::time::Instant;

    fn synced() -> SyncState {
        SyncState::Synced { at: Instant::now() }
    }

    fn failing(error: &str) -> SyncState {
        SyncState::Failing {
            error: error.into(),
        }
    }

    fn labels(status: &ConnectorStatus) -> Vec<&str> {
        status
            .actions
            .iter()
            .map(|action| action.label.as_ref())
            .collect()
    }

    #[test]
    fn no_config_hides_every_connector() {
        assert_eq!(status_for_calendar(&SyncState::NoConfig), None);
        assert_eq!(status_for_gmail(&SyncState::NoConfig), None);
        assert_eq!(status_for_inbox(&SyncState::NoConfig, 3, false), None);
        assert_eq!(
            status_for_readwise(&SyncState::NoConfig, ReadwiseExtras::default()),
            None
        );
    }

    #[test]
    fn healthy_states_need_nothing_and_offer_a_sync() {
        let calendar = status_for_calendar(&synced()).unwrap();
        assert_eq!(calendar.summary.as_ref(), "synced just now");
        assert_eq!(calendar.attention, Attention::None);
        assert!(!calendar.busy);
        assert_eq!(
            calendar.primary_action().unwrap().kind,
            ActionKind::SyncCalendarNow
        );
        assert_eq!(labels(&calendar), ["Sync now"]);

        let gmail = status_for_gmail(&synced()).unwrap();
        assert_eq!(gmail.summary.as_ref(), "checked just now");
        assert_eq!(gmail.attention, Attention::None);

        let inbox = status_for_inbox(&synced(), 0, false).unwrap();
        assert_eq!(inbox.summary.as_ref(), "empty");
        assert_eq!(inbox.attention, Attention::None);

        let readwise = status_for_readwise(
            &synced(),
            ReadwiseExtras {
                last_landed: 2,
                ..ReadwiseExtras::default()
            },
        )
        .unwrap();
        assert_eq!(readwise.summary.as_ref(), "synced just now · +2 highlights");
        assert_eq!(readwise.attention, Attention::None);
        assert_eq!(
            status_for_readwise(
                &synced(),
                ReadwiseExtras {
                    last_landed: 1,
                    ..ReadwiseExtras::default()
                },
            )
            .unwrap()
            .summary
            .as_ref(),
            "synced just now · +1 highlight"
        );
    }

    #[test]
    fn in_flight_states_spin_instead_of_dotting() {
        assert!(status_for_calendar(&SyncState::Connecting).unwrap().busy);
        assert!(status_for_gmail(&SyncState::Connecting).unwrap().busy);
        let importing = status_for_readwise(
            &SyncState::Idle,
            ReadwiseExtras {
                importing_library: true,
                ..ReadwiseExtras::default()
            },
        )
        .unwrap();
        assert_eq!(importing.summary.as_ref(), "importing your library…");
        assert!(importing.busy);
        assert_eq!(importing.attention, Attention::None);
        assert!(
            !status_for_readwise(&SyncState::Idle, ReadwiseExtras::default())
                .unwrap()
                .busy
        );
    }

    #[test]
    fn failures_and_lost_sign_ins_are_errors_with_a_way_back() {
        let calendar = status_for_calendar(&failing("quota exceeded")).unwrap();
        assert_eq!(calendar.summary.as_ref(), "sync failed");
        assert_eq!(calendar.detail.as_deref(), Some("quota exceeded"));
        assert_eq!(calendar.attention, Attention::Error);
        assert_eq!(labels(&calendar), ["Retry"]);
        assert_eq!(
            calendar.primary_action().unwrap().kind,
            ActionKind::SyncCalendarNow
        );

        let gmail = status_for_gmail(&SyncState::Disconnected).unwrap();
        assert_eq!(gmail.summary.as_ref(), "sign-in needed");
        assert_eq!(gmail.attention, Attention::Error);
        assert_eq!(
            gmail.primary_action().unwrap().kind,
            ActionKind::ConnectGoogleWorkspace
        );
        assert_eq!(labels(&gmail), ["Reconnect"]);

        let readwise =
            status_for_readwise(&SyncState::Disconnected, ReadwiseExtras::default()).unwrap();
        assert_eq!(readwise.summary.as_ref(), "token rejected");
        assert_eq!(
            readwise.primary_action().unwrap().kind,
            ActionKind::ConnectReadwise
        );
    }

    #[test]
    fn never_connected_is_a_warning_with_a_connect_button() {
        let calendar = status_for_calendar(&SyncState::NeverConnected).unwrap();
        assert_eq!(calendar.attention, Attention::Warning);
        assert_eq!(labels(&calendar), ["Connect Google Workspace"]);
        let readwise =
            status_for_readwise(&SyncState::NeverConnected, ReadwiseExtras::default()).unwrap();
        assert_eq!(readwise.attention, Attention::Warning);
        assert_eq!(labels(&readwise), ["Connect Readwise"]);
    }

    #[test]
    fn the_google_connect_button_shows_once() {
        let inbox = status_for_inbox(&SyncState::NeverConnected, 0, true).unwrap();
        assert_eq!(inbox.summary.as_ref(), "via Google Workspace");
        assert_eq!(inbox.attention, Attention::None);
        assert!(inbox.actions.is_empty());

        let alone = status_for_inbox(&SyncState::NeverConnected, 0, false).unwrap();
        assert_eq!(alone.attention, Attention::Warning);
        assert_eq!(labels(&alone), ["Connect Google Workspace"]);
    }

    #[test]
    fn inbox_items_waiting_lead_to_triage() {
        let inbox = status_for_inbox(&synced(), 3, false).unwrap();
        assert_eq!(inbox.summary.as_ref(), "3 waiting");
        assert_eq!(inbox.attention, Attention::Waiting);
        assert_eq!(
            inbox.primary_action().unwrap().kind,
            ActionKind::TriageInbox
        );
        assert_eq!(labels(&inbox), ["Triage", "Sync now"]);
    }

    #[test]
    fn calendar_holds_split_into_fixable_and_waiting() {
        let waiting = status_for_calendar(&SyncState::Holding {
            reason: HoldReason::Waiting("no note for today yet".into()),
        })
        .unwrap();
        assert_eq!(waiting.summary.as_ref(), "no note for today yet");
        assert_eq!(waiting.attention, Attention::None);
        assert_eq!(waiting.detail, None);

        let no_heading = status_for_calendar(&SyncState::Holding {
            reason: HoldReason::NoPlannerHeading {
                heading: "Day planner".into(),
            },
        })
        .unwrap();
        assert_eq!(no_heading.attention, Attention::Warning);
        assert!(no_heading.detail.is_some());
        assert_eq!(labels(&no_heading), ["Add heading", "Use another…"]);
        assert_eq!(
            no_heading.primary_action().unwrap().kind,
            ActionKind::AddPlannerHeading
        );

        let too_deep = status_for_calendar(&SyncState::Holding {
            reason: HoldReason::PlannerHeadingTooDeep {
                heading: "Day planner".into(),
            },
        })
        .unwrap();
        assert_eq!(too_deep.attention, Attention::Warning);
        assert!(too_deep.detail.is_some());
    }

    #[test]
    fn gmail_and_inbox_holds_are_always_fixable() {
        let holding = SyncState::Holding {
            reason: HoldReason::Waiting("label \"thock/backlog\" not found in Gmail".into()),
        };
        assert_eq!(
            status_for_gmail(&holding).unwrap().attention,
            Attention::Warning
        );
        assert_eq!(
            status_for_inbox(&holding, 0, false).unwrap().attention,
            Attention::Warning
        );
    }

    #[test]
    fn a_readwise_config_problem_replaces_the_sync_state() {
        let error: SharedString = "unknown category \"podcasts\"".into();
        let readwise = status_for_readwise(
            &synced(),
            ReadwiseExtras {
                config_error: Some(&error),
                ..ReadwiseExtras::default()
            },
        )
        .unwrap();
        assert_eq!(readwise.summary, error);
        assert_eq!(readwise.attention, Attention::Warning);
        assert!(readwise.detail.is_some());
        assert!(readwise.actions.is_empty());
    }

    #[test]
    fn only_rows_needing_the_user_go_inline() {
        let inline = inline_statuses([
            status_for_gmail(&synced()),
            None,
            status_for_inbox(&synced(), 2, false),
            status_for_readwise(&failing("boom"), ReadwiseExtras::default()),
        ]);
        let connectors: Vec<_> = inline.iter().map(|status| status.connector).collect();
        assert_eq!(connectors, [Connector::Inbox, Connector::Readwise]);
        assert!(
            inline_statuses([status_for_gmail(&synced()), status_for_calendar(&synced())])
                .is_empty()
        );
    }

    #[test]
    fn the_glance_folds_to_the_worst_attention() {
        assert_eq!(Glance::from_statuses(&[]), None);

        let healthy = [
            status_for_calendar(&synced()).unwrap(),
            status_for_gmail(&synced()).unwrap(),
        ];
        assert_eq!(
            Glance::from_statuses(&healthy),
            Some(Glance {
                attention: Attention::None,
                busy: false,
                tooltip: "All synced".into(),
            })
        );

        let mixed = [
            status_for_calendar(&SyncState::Connecting).unwrap(),
            status_for_inbox(&synced(), 1, false).unwrap(),
            status_for_readwise(&failing("boom"), ReadwiseExtras::default()).unwrap(),
        ];
        assert_eq!(worst_attention(&mixed), Attention::Error);
        let glance = Glance::from_statuses(&mixed).unwrap();
        assert_eq!(glance.attention, Attention::Error);
        assert!(glance.busy);
        // The tooltip names the first row that needs the user, in row order.
        assert_eq!(glance.tooltip.as_ref(), "Inbox · 1 waiting");
    }

    #[test]
    fn relative_times_read_like_a_person_would_say_them() {
        assert_eq!(format_ago(Duration::from_secs(30)), "just now");
        assert_eq!(format_ago(Duration::from_secs(60 * 5)), "5m ago");
        assert_eq!(format_ago(Duration::from_secs(60 * 60 * 3)), "3h ago");
    }

    struct Services {
        calendar: Entity<CalendarService>,
        gmail: Entity<GmailService>,
        inbox: Entity<InboxService>,
        readwise: Entity<ReadwiseService>,
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            cx.bind_keys([
                KeyBinding::new("down", SelectNext, Some("ThockSyncStatus")),
                KeyBinding::new("up", SelectPrevious, Some("ThockSyncStatus")),
                KeyBinding::new("right", SelectChild, Some("ThockSyncStatus")),
                KeyBinding::new("left", SelectParent, Some("ThockSyncStatus")),
                KeyBinding::new("enter", Confirm, Some("ThockSyncStatus")),
                KeyBinding::new("escape", Cancel, Some("ThockSyncStatus")),
            ]);
        });
    }

    /// A project with every Google service and Readwise registered but
    /// unconfigured, the way a vault with no connector looks.
    async fn project_with_services(cx: &mut TestAppContext) -> (Entity<Project>, Services) {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs, [Path::new("/vault")], cx).await;
        cx.run_until_parked();
        let services = cx.update(|cx| {
            let calendar = calendar_service::new_for_test(&project, cx);
            let gmail = gmail_service::new_for_test(&project, cx);
            let inbox = inbox_service::new_for_test(&project, cx);
            let readwise = readwise_service::new_for_test(&project, cx);
            Services {
                calendar,
                gmail,
                inbox,
                readwise,
            }
        });
        cx.run_until_parked();
        (project, services)
    }

    #[gpui::test]
    async fn the_icon_hides_without_configs_and_dots_on_failure(cx: &mut TestAppContext) {
        init_test(cx);
        let (project, services) = project_with_services(cx).await;
        let indicator = cx.new(|cx| SyncStatusIndicator::for_project(project, cx));

        assert_eq!(
            indicator.read_with(cx, |indicator, cx| indicator.glance(cx)),
            None
        );

        services
            .gmail
            .update(cx, |gmail, cx| gmail.set_state_for_test(synced(), cx));
        services
            .inbox
            .update(cx, |inbox, cx| inbox.set_state_for_test(synced(), cx));
        let glance = indicator
            .read_with(cx, |indicator, cx| indicator.glance(cx))
            .unwrap();
        assert_eq!(glance.attention, Attention::None);
        assert_eq!(glance.tooltip.as_ref(), "All synced");
        assert_eq!(
            indicator.read_with(cx, |indicator, cx| indicator.statuses(cx).len()),
            2
        );

        services.gmail.update(cx, |gmail, cx| {
            gmail.set_state_for_test(failing("boom"), cx)
        });
        let glance = indicator
            .read_with(cx, |indicator, cx| indicator.glance(cx))
            .unwrap();
        assert_eq!(glance.attention, Attention::Error);
        assert_eq!(glance.tooltip.as_ref(), "Gmail · sync failed");

        services
            .gmail
            .update(cx, |gmail, cx| gmail.set_state_for_test(synced(), cx));
        let glance = indicator
            .read_with(cx, |indicator, cx| indicator.glance(cx))
            .unwrap();
        assert_eq!(glance.attention, Attention::None);
    }

    /// Stands in for the status bar: hosts the popover and catches what it
    /// dispatches, the way the workspace's registered actions would.
    struct Harness {
        popover: Entity<SyncStatusPopover>,
        ran: Rc<RefCell<Vec<&'static str>>>,
        dismissed: Rc<Cell<bool>>,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let ran = self.ran.clone();
            let ran_again = self.ran.clone();
            let ran_a_third_time = self.ran.clone();
            div()
                .size_full()
                .on_action(move |_: &SyncGmailNow, _, _| ran.borrow_mut().push("sync gmail"))
                .on_action(move |_: &ConnectReadwise, _, _| {
                    ran_again.borrow_mut().push("connect readwise")
                })
                .on_action(move |_: &ChoosePlannerHeading, _, _| {
                    ran_a_third_time.borrow_mut().push("choose heading")
                })
                .child(self.popover.clone())
        }
    }

    async fn open_popover(
        cx: &mut TestAppContext,
    ) -> (
        Services,
        Entity<SyncStatusPopover>,
        Entity<Harness>,
        &mut VisualTestContext,
    ) {
        init_test(cx);
        let (project, services) = project_with_services(cx).await;
        services
            .gmail
            .update(cx, |gmail, cx| gmail.set_state_for_test(synced(), cx));
        services.readwise.update(cx, |readwise, cx| {
            readwise.set_state_for_test(SyncState::NeverConnected, cx)
        });
        let indicator = cx.new(|cx| SyncStatusIndicator::for_project(project, cx));
        let ran = Rc::new(RefCell::new(Vec::new()));
        let dismissed = Rc::new(Cell::new(false));
        let (harness, cx) = cx.add_window_view(|window, cx| {
            let popover = cx.new(|cx| SyncStatusPopover::new(indicator.clone(), window, cx));
            cx.subscribe(&popover, {
                let dismissed = dismissed.clone();
                move |_, _, _: &DismissEvent, _| dismissed.set(true)
            })
            .detach();
            Harness {
                popover,
                ran,
                dismissed,
            }
        });
        let popover = harness.read_with(cx, |harness, _| harness.popover.clone());
        cx.update(|window, cx| {
            let focus_handle = popover.read(cx).focus_handle.clone();
            window.focus(&focus_handle, cx);
        });
        cx.run_until_parked();
        (services, popover, harness, cx)
    }

    #[gpui::test]
    async fn enter_runs_the_selected_row_s_primary_action(cx: &mut TestAppContext) {
        let (_services, popover, harness, cx) = open_popover(cx).await;
        assert_eq!(
            popover.read_with(cx, |popover, cx| popover.selected_connector(cx)),
            Some(Connector::Gmail)
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        harness.read_with(cx, |harness, _| {
            assert_eq!(*harness.ran.borrow(), ["sync gmail"]);
            assert!(!harness.dismissed.get(), "a sync keeps the popover open");
        });

        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        harness.read_with(cx, |harness, _| {
            assert_eq!(*harness.ran.borrow(), ["sync gmail", "connect readwise"]);
            assert!(harness.dismissed.get(), "a connect flow closes the popover");
        });
    }

    #[gpui::test]
    async fn left_and_right_pick_between_a_row_s_actions(cx: &mut TestAppContext) {
        let (services, popover, harness, cx) = open_popover(cx).await;
        services.calendar.update(cx, |calendar, cx| {
            calendar.set_state_for_test(
                SyncState::Holding {
                    reason: HoldReason::NoPlannerHeading {
                        heading: "Day planner".into(),
                    },
                },
                cx,
            )
        });
        cx.simulate_keystrokes("up up right enter");
        cx.run_until_parked();
        assert_eq!(
            popover.read_with(cx, |popover, cx| popover.selected_connector(cx)),
            Some(Connector::Calendar)
        );
        harness.read_with(cx, |harness, _| {
            assert_eq!(*harness.ran.borrow(), ["choose heading"]);
        });
    }

    #[gpui::test]
    async fn the_cursor_follows_its_connector_when_a_row_appears_above(cx: &mut TestAppContext) {
        let (services, popover, _harness, cx) = open_popover(cx).await;
        cx.simulate_keystrokes("down");
        assert_eq!(
            popover.read_with(cx, |popover, cx| popover.selected_connector(cx)),
            Some(Connector::Readwise)
        );

        services
            .calendar
            .update(cx, |calendar, cx| calendar.set_state_for_test(synced(), cx));
        cx.run_until_parked();
        assert_eq!(
            popover.read_with(cx, |popover, cx| popover.selected_connector(cx)),
            Some(Connector::Readwise)
        );
        cx.simulate_keystrokes("up up");
        assert_eq!(
            popover.read_with(cx, |popover, cx| popover.selected_connector(cx)),
            Some(Connector::Calendar)
        );
    }

    #[gpui::test]
    async fn escape_dismisses_the_popover(cx: &mut TestAppContext) {
        let (_services, _popover, harness, cx) = open_popover(cx).await;
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        harness.read_with(cx, |harness, _| assert!(harness.dismissed.get()));
    }
}
