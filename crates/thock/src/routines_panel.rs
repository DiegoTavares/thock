//! The Routines panel (V7 spec §7.4): the generic navigation panel. Each
//! enabled Routine contributes one section — quick links, skills — rendered
//! from its on-disk `routine.toml`. The panel also hosts discovery ("In this
//! vault"), activation, removal, and the New Routine entry points.
//!
//! This is the evolution of the V1 Timeline panel; its dock identity
//! (persisted keys, activation priority) is deliberately unchanged.

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use gpui::{
    Action, App, AsyncWindowContext, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, KeyContext, Pixels, PromptLevel, SharedString, Subscription, WeakEntity, Window,
    actions, px,
};
use menu::{Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::Project;
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::str::FromStr as _;
use ui::prelude::*;
use ui::{
    Button, Divider, Icon, IconButton, IconSize, KeyBinding, Label, ListItem, ListSubHeader,
    Tooltip, rems_from_px,
};
use util::ResultExt as _;
use workspace::dock::{DockPosition, Panel, PanelEvent};
use workspace::notifications::NotificationId;
use workspace::{OpenOptions, OpenVisible, Toast, Workspace};

use crate::agent_panel::RunSkill;
use crate::getting_started;
use crate::notes::{EnsureNoteOutcome, TimelineEntry, ensure_note};
use crate::routines::{
    self, DiscoveredRoutine, LinkKind, RoutineLink, RoutineLoad, RoutineManifest, RoutineSkill,
};
use crate::vault::{OnboardingState, Vault, VaultStatus, scaffold_vault};

// The pre-V7 key, kept verbatim so the dock's persisted layout survives the
// panel's rename.
const ROUTINES_PANEL_KEY: &str = "ThockTimelinePanel";

actions!(
    thock,
    [
        /// Toggles focus on the Thock routines panel.
        ToggleFocus,
        /// Opens today's daily note, creating it if needed.
        OpenToday,
        /// Opens yesterday's daily note, creating it if needed.
        OpenYesterday,
        /// Opens tomorrow's daily note, creating it if needed.
        OpenTomorrow,
        /// Opens this week's weekly note, creating it if needed.
        OpenThisWeek,
        /// Opens last week's weekly note, creating it if needed.
        OpenLastWeek,
        /// Creates a new Routine with your agent (the New Routine ritual).
        NewRoutine,
        /// Opens the selected ritual's instructions so you can read or change them.
        ViewSkill,
        /// Hides the selected group of extra notes or setup steps.
        CollapseGroup,
        /// Shows what the selected group of extra notes or setup steps holds.
        ExpandGroup,
        /// Opens the introduction to Thock in your browser.
        OpenGuide,
        /// Opens the customize guide — themes, text size, and keyboard shortcuts.
        OpenCustomize,
        /// Hides the Getting started list from the routines panel.
        HideGettingStarted
    ]
);

/// Opens a Routine quick link by the stable ids declared in `routine.toml`,
/// for user keybindings (V7 §7.5), e.g.
/// `["thock::OpenLink", { "routine": "timeline", "link": "today" }]`.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = thock)]
#[serde(deny_unknown_fields)]
pub struct OpenLink {
    pub routine: String,
    pub link: String,
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<RoutinesPanel>(window, cx);
        });
        register_open_action::<OpenToday>(workspace, TimelineEntry::Today);
        register_open_action::<OpenYesterday>(workspace, TimelineEntry::Yesterday);
        register_open_action::<OpenTomorrow>(workspace, TimelineEntry::Tomorrow);
        register_open_action::<OpenThisWeek>(workspace, TimelineEntry::ThisWeek);
        register_open_action::<OpenLastWeek>(workspace, TimelineEntry::LastWeek);
        workspace.register_action(|workspace, action: &OpenLink, window, cx| {
            if let Some(panel) = workspace.panel::<RoutinesPanel>(cx) {
                panel.update(cx, |panel, cx| {
                    panel.open_link_by_id(action.routine.clone(), action.link.clone(), window, cx);
                });
            }
        });
        workspace.register_action(|workspace, _: &NewRoutine, window, cx| {
            crate::agent_panel::AgentPanel::launch_in_workspace(
                workspace,
                new_routine_launch_request(),
                window,
                cx,
            );
        });
        workspace.register_action(|workspace, _: &OpenGuide, _window, cx| {
            if let Some(panel) = workspace.panel::<RoutinesPanel>(cx) {
                panel.update(cx, |panel, cx| panel.open_guide(cx));
            }
        });
        workspace.register_action(|workspace, _: &OpenCustomize, window, cx| {
            if let Some(panel) = workspace.panel::<RoutinesPanel>(cx) {
                panel.update(cx, |panel, cx| panel.open_customize(window, cx));
            }
        });
        workspace.register_action(|workspace, _: &HideGettingStarted, _window, cx| {
            if let Some(panel) = workspace.panel::<RoutinesPanel>(cx) {
                panel.update(cx, |panel, cx| panel.hide_getting_started(cx));
            }
        });
    })
    .detach();
}

fn register_open_action<A: Action>(workspace: &mut Workspace, entry: TimelineEntry) {
    workspace.register_action(move |workspace, _: &A, window, cx| {
        if let Some(panel) = workspace.panel::<RoutinesPanel>(cx) {
            panel.update(cx, |panel, cx| panel.open_note(entry, window, cx));
        }
    });
}

fn new_routine_launch_request() -> crate::agent_panel::LaunchRequest {
    crate::agent_panel::LaunchRequest {
        title: "New Routine".to_string(),
        kickoff: Some(crate::agent::run_skill_kickoff(
            routines::NEW_ROUTINE_SKILL_PATH,
        )),
        tier: crate::agent::ModelTier::Default,
    }
}

/// The Routine `icon` field's named subset of `IconName` (V7 decision 10).
/// Unknown names and `None` fall back to `Blocks`. `ROUTINES.md` documents
/// the list.
/// An icon a manifest can ask for. Most are `IconName` variants, but a few
/// useful glyphs (the browser link's `html`) only exist in the file-type icon
/// set, which has no enum variant — hence the asset-path arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowIcon {
    Named(IconName),
    Path(&'static str),
}

impl RowIcon {
    fn build(self) -> Icon {
        match self {
            Self::Named(name) => Icon::new(name),
            Self::Path(path) => Icon::from_path(path),
        }
    }
}

/// Resolves a manifest `icon = "…"` name. Aliases cover the names that don't
/// match an `IconName` one-for-one; everything else is looked up as a
/// snake_case `IconName`, so a Routine author can reach the whole icon set
/// without the app maintaining a whitelist. Unknown names resolve to `None`
/// so the caller can fall back to its own default.
fn manifest_icon(name: &str) -> Option<RowIcon> {
    match name {
        "todo" => Some(RowIcon::Named(IconName::ListTodo)),
        "html" => Some(RowIcon::Path("icons/file_icons/html.svg")),
        other => IconName::from_str(other).ok().map(RowIcon::Named),
    }
}

fn routine_icon(name: Option<&str>) -> RowIcon {
    name.and_then(manifest_icon)
        .unwrap_or(RowIcon::Named(IconName::Blocks))
}

/// A quick link's icon: the manifest's override, else a default that reflects
/// where the link goes — a note, some other file, or out to the browser.
fn link_icon(link: &RoutineLink) -> RowIcon {
    link.icon
        .as_deref()
        .and_then(manifest_icon)
        .unwrap_or_else(|| match link.kind {
            LinkKind::Browser => RowIcon::Path("icons/file_icons/html.svg"),
            LinkKind::Editor | LinkKind::Preview => {
                if link.open.ends_with(".md") {
                    RowIcon::Named(IconName::Notepad)
                } else {
                    RowIcon::Named(IconName::FileCode)
                }
            }
        })
}

/// A skill's icon: the manifest's override, else a glyph that says what the
/// row *does*. Rituals get the run glyph so a verb never reads like one of
/// the note rows above it; setup steps get the settings glyph.
fn skill_icon(skill: &RoutineSkill) -> RowIcon {
    skill
        .icon
        .as_deref()
        .and_then(manifest_icon)
        .unwrap_or(RowIcon::Named(if skill.kind.is_setup() {
            IconName::Settings
        } else {
            IconName::PlayOutlined
        }))
}

/// Zone captions. They separate a Routine's destinations from its verbs, and
/// only appear when a section actually holds both.
const NOTES_CAPTION: &str = "Notes";
const RITUALS_CAPTION: &str = "Rituals";
/// The implicit group every `kind = "setup"` skill lands in.
const SETUP_GROUP: &str = "Setup";

/// Groups are keyed per Routine; the separator is a character no manifest
/// label can contain.
fn group_key(routine_id: &str, label: &str) -> String {
    format!("{routine_id}\u{1}{label}")
}

/// The pseudo section id the Getting started rows use in nav keys; not a
/// Routine, so it can never collide with one (routine ids are directory
/// names, and this one contains no files).
const GETTING_STARTED_SECTION_ID: &str = "\u{1}getting-started";

/// The first-run checklist's four steps (V18 §5.4), in render order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GettingStartedStep {
    Introduction,
    Customize,
    Agent,
    Tour,
}

impl GettingStartedStep {
    const ALL: [Self; 4] = [Self::Introduction, Self::Customize, Self::Agent, Self::Tour];

    fn key(self) -> &'static str {
        match self {
            Self::Introduction => "introduction",
            Self::Customize => "customize",
            Self::Agent => "agent",
            Self::Tour => "tour",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Introduction => "Read the introduction",
            Self::Customize => "Customize",
            Self::Agent => "Connect your agent",
            Self::Tour => "Take the tour",
        }
    }

    fn done(self, steps: getting_started::Steps) -> bool {
        match self {
            Self::Introduction => steps.introduction,
            Self::Customize => steps.customize,
            Self::Agent => steps.agent,
            Self::Tour => steps.tour,
        }
    }
}

/// What a row *is*, independent of where it currently sits. Opening or
/// closing a group renumbers everything below it, so the cursor is re-found
/// by this rather than left on a stale index.
fn row_key(row: &NavRow) -> String {
    match &row.kind {
        NavRowKind::Link(link) => nav_key(&row.routine_id, "link", &link.id),
        NavRowKind::Skill(skill) => nav_key(&row.routine_id, "skill", &skill.id),
        NavRowKind::Group(label) => nav_key(&row.routine_id, "group", label),
        NavRowKind::GettingStarted(step) => nav_key(&row.routine_id, "step", step.key()),
    }
}

fn nav_key(routine_id: &str, tag: &str, id: &str) -> String {
    format!("{routine_id}\u{1}{tag}\u{1}{id}")
}

/// Where the cursor belongs once the row list has changed shape: back on the
/// row it was on, or — when that row went away with the group that held it —
/// on the group's own row.
fn reanchor_selection(rows: &[NavRow], anchor: &str, group_row: &str) -> Option<usize> {
    rows.iter()
        .position(|row| row_key(row) == anchor)
        .or_else(|| rows.iter().position(|row| row_key(row) == group_row))
}

/// A Routine section in display order (V11 §4): the destinations it wants in
/// reach, then the groups holding the ones it doesn't, then its rituals, then
/// the collapsed Setup row. Captions appear only when a section holds both
/// places and verbs — with nothing to separate they would be decoration.
fn section_items(
    manifest: &RoutineManifest,
    expanded_groups: &HashSet<String>,
) -> Vec<SectionItem> {
    fn item(kind: SectionItemKind) -> SectionItem {
        SectionItem { group: None, kind }
    }
    let is_expanded =
        |label: &str| expanded_groups.contains(&group_key(manifest.id.as_str(), label));

    let mut items = Vec::new();
    let (rituals, setup_skills): (Vec<&RoutineSkill>, Vec<&RoutineSkill>) = manifest
        .skills
        .iter()
        .partition(|skill| !skill.kind.is_setup());
    let captioned = !manifest.links.is_empty() && !manifest.skills.is_empty();

    if captioned {
        items.push(item(SectionItemKind::Caption(NOTES_CAPTION.into())));
    }
    for link in manifest.links.iter().filter(|link| link.group.is_none()) {
        items.push(item(SectionItemKind::Link(link.clone())));
    }
    let mut labels: Vec<&str> = Vec::new();
    for label in manifest
        .links
        .iter()
        .filter_map(|link| link.group.as_deref())
    {
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    for label in labels {
        let members = manifest
            .links
            .iter()
            .filter(|link| link.group.as_deref() == Some(label));
        let expanded = is_expanded(label);
        let label: SharedString = label.to_string().into();
        items.push(item(SectionItemKind::Group {
            label: label.clone(),
            expanded,
        }));
        if expanded {
            items.extend(members.map(|link| SectionItem {
                group: Some(label.clone()),
                kind: SectionItemKind::Link(link.clone()),
            }));
        }
    }

    if captioned && !rituals.is_empty() {
        items.push(item(SectionItemKind::Caption(RITUALS_CAPTION.into())));
    }
    for skill in rituals {
        items.push(item(SectionItemKind::Skill(skill.clone())));
    }
    if !setup_skills.is_empty() {
        let expanded = is_expanded(SETUP_GROUP);
        items.push(item(SectionItemKind::Group {
            label: SETUP_GROUP.into(),
            expanded,
        }));
        if expanded {
            items.extend(setup_skills.into_iter().map(|skill| SectionItem {
                group: Some(SETUP_GROUP.into()),
                kind: SectionItemKind::Skill(skill.clone()),
            }));
        }
    }
    items
}

/// A zone caption. Not a row: it takes no selection and can't be collapsed,
/// which is the whole reason it replaced the old "Skills" folder entry. The
/// rule running off to the right is what makes it read as a separator rather
/// than as another entry in the list.
fn render_caption(label: SharedString) -> AnyElement {
    div()
        .pl(px(12.))
        .pt_1()
        .child(
            ListSubHeader::new(label).end_slot(
                div()
                    .flex_1()
                    .child(Divider::horizontal())
                    .into_any_element(),
            ),
        )
        .into_any_element()
}

/// Shows the routines panel (opening the left dock) when the workspace is a
/// vault. Called once at startup after all panels are registered, so the
/// navigation rail — not the file tree — is what a vault opens on.
pub fn show_panel_if_vault(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let is_vault = workspace
        .visible_worktrees(cx)
        .next()
        .is_some_and(|worktree| {
            matches!(
                Vault::detect(&worktree.read(cx).abs_path()),
                VaultStatus::Valid(_)
            )
        });
    if is_vault {
        workspace.open_panel::<RoutinesPanel>(window, cx);
    }
}

/// One keyboard-navigable row of the panel: a Routine's link, skill, or the
/// disclosure row standing in for a collapsed group. The flat list
/// generalizes the old fixed Timeline cursor (V7 §7.4).
enum NavRowKind {
    Link(RoutineLink),
    Skill(RoutineSkill),
    Group(SharedString),
    GettingStarted(GettingStartedStep),
}

struct NavRow {
    routine_id: String,
    /// `Some(label)` when the row lives inside that expanded group, so
    /// `left` on a member can close the group it came from.
    group: Option<SharedString>,
    kind: NavRowKind,
}

/// One rendered entry of a Routine section, in display order. Captions are
/// not selectable, so the keyboard row list is this list minus its captions
/// — deriving both from a single walk is what keeps the rendered order and
/// the cursor indices from drifting apart.
struct SectionItem {
    /// `Some(label)` when the item sits inside that collapsible group.
    group: Option<SharedString>,
    kind: SectionItemKind,
}

enum SectionItemKind {
    Caption(SharedString),
    Link(RoutineLink),
    Skill(RoutineSkill),
    Group { label: SharedString, expanded: bool },
}

pub struct RoutinesPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    vault_status: VaultStatus,
    /// Content fingerprint of `routines/*/routine.toml` and the pending
    /// ready markers; folded into the refresh snapshot so a new or edited
    /// definition re-renders without a registry write (V7 §9 trap 2).
    fingerprint: Vec<(String, String)>,
    /// Keyboard cursor over the flat row list. `None` means the highlight
    /// follows whichever link matches the active editor item.
    selected_index: Option<usize>,
    /// Enabled Routines in registry order — live manifests or visible error
    /// entries, reloaded whenever the refresh snapshot changes.
    routines: Vec<RoutineLoad>,
    /// Valid-or-broken `routine.toml` findings in the vault that aren't
    /// registered yet — the picker's "In this vault" group.
    discovered: Vec<DiscoveredRoutine>,
    /// Catalog Routines without an enabled registry entry.
    addable_catalog: Vec<RoutineManifest>,
    /// Sections start expanded; this tracks the ones the user collapsed.
    collapsed_routines: HashSet<String>,
    /// Groups (demoted links, setup steps) start collapsed; this tracks the
    /// ones the user opened, keyed by `group_key`.
    expanded_groups: HashSet<String>,
    show_add_routines: bool,
    /// Ready markers already offered with a toast this session, so a marker
    /// awaiting activation doesn't re-toast on every refresh.
    ready_toasted: HashSet<String>,
    /// The first-run Getting started checklist (V18 §5.4), `None` while
    /// inactive. Re-derived off the UI thread whenever the refresh snapshot
    /// or the agent connection changes.
    getting_started: Option<getting_started::Steps>,
    position: DockPosition,
    /// Whether an onboarding marker/expiry check is mid-flight. Checks are
    /// serialized (never cancelled): a cancelled check could persist a state
    /// transition and die before its one-shot effect (the tour, the expiry
    /// re-prompt) fires, silently eating it. Serializing also keeps this
    /// panel's registry read-modify-writes from racing each other.
    onboarding_check_running: bool,
    /// A check was requested while one was running; run one more when it
    /// finishes so the latest filesystem state is always observed.
    onboarding_recheck: bool,
    _subscriptions: Vec<Subscription>,
}

/// A not-yet-onboarded Routine whose marker/expiry needs checking (V5 §7.4).
struct OnboardingCandidate {
    routine_id: String,
    routine_name: String,
    /// Vault-relative explainer doc — the capabilities tour.
    doc: String,
    /// Vault-relative onboarding skill file.
    onboarding_file: String,
    state: Option<OnboardingState>,
    installed_at: Option<DateTime<Utc>>,
}

impl RoutinesPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            RoutinesPanel::new(workspace, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = workspace.weak_handle();
        let workspace_entity = cx.entity();
        cx.new(|cx| {
            let project_subscription =
                cx.subscribe_in(&project, window, |this: &mut Self, _, event, window, cx| {
                    if matches!(
                        event,
                        project::Event::WorktreeAdded(_)
                            | project::Event::WorktreeRemoved(_)
                            | project::Event::WorktreeUpdatedEntries(..)
                    ) {
                        this.refresh_vault_status(cx);
                        // The marker dir lives inside the worktree, so entry
                        // updates double as the onboarding file-watch.
                        this.schedule_onboarding_check(window, cx);
                    }
                });
            // On active-item changes, drop the keyboard cursor so the
            // highlight goes back to following the open note.
            let workspace_subscription = cx.subscribe(
                &workspace_entity,
                |this: &mut Self, _, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::ActiveItemChanged) {
                        this.selected_index = None;
                        cx.notify();
                    }
                },
            );
            // The connect flow saving a global default writes outside the
            // vault, so no worktree event fires — the epoch global is how
            // the checklist's "Connect your agent" row learns about it.
            let connection_subscription =
                cx.observe_global::<crate::agent_panel::ConnectionEpoch>(|this: &mut Self, cx| {
                    this.refresh_getting_started(cx);
                });
            let mut this = Self {
                workspace: weak_workspace,
                project,
                focus_handle: cx.focus_handle(),
                vault_status: VaultStatus::NotAVault,
                fingerprint: Vec::new(),
                selected_index: None,
                routines: Vec::new(),
                discovered: Vec::new(),
                addable_catalog: Vec::new(),
                collapsed_routines: HashSet::new(),
                expanded_groups: HashSet::new(),
                show_add_routines: false,
                ready_toasted: HashSet::new(),
                getting_started: None,
                position: DockPosition::Left,
                onboarding_check_running: false,
                onboarding_recheck: false,
                _subscriptions: vec![
                    project_subscription,
                    workspace_subscription,
                    connection_subscription,
                ],
            };
            this.refresh_vault_status(cx);
            // Startup check: a marker written while the app was closed still
            // completes onboarding "on next focus" (V5 §7.4).
            this.schedule_onboarding_check(window, cx);
            this
        })
    }

    fn workspace_root(&self, cx: &App) -> Option<PathBuf> {
        let worktree = self.project.read(cx).visible_worktrees(cx).next()?;
        Some(worktree.read(cx).abs_path().to_path_buf())
    }

    fn refresh_vault_status(&mut self, cx: &mut Context<Self>) {
        let status = match self.workspace_root(cx) {
            Some(root) => Vault::detect(&root),
            None => VaultStatus::NotAVault,
        };
        let fingerprint = match &status {
            VaultStatus::Valid(vault) => routines::refresh_fingerprint(&vault.root),
            _ => Vec::new(),
        };
        let status_changed = status != self.vault_status;
        if status_changed || fingerprint != self.fingerprint {
            self.vault_status = status;
            self.fingerprint = fingerprint;
            self.refresh_routines();
            if status_changed {
                self.reconcile_routines(cx);
            }
            self.offer_ready_routines(cx);
            self.refresh_getting_started(cx);
            cx.notify();
        }
    }

    /// Re-derives the Getting started checklist off the UI thread: the step
    /// markers are cheap, but resolving the agent command reads the global
    /// settings file. Once every step is done the checklist dismisses itself
    /// and the section disappears for good (V18 §5.4).
    fn refresh_getting_started(&mut self, cx: &mut Context<Self>) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            if self.getting_started.take().is_some() {
                cx.notify();
            }
            return;
        };
        let vault = vault.clone();
        let derive = cx.background_spawn(async move {
            let connected = crate::agent::resolved_command(Some(&vault)).is_some();
            match getting_started::state(&vault.root, connected) {
                Some(steps) if steps.all_done() => {
                    getting_started::dismiss(&vault.root);
                    None
                }
                state => state,
            }
        });
        cx.spawn(async move |this, cx| {
            let state = derive.await;
            this.update(cx, |this, cx| {
                if this.getting_started != state {
                    this.getting_started = state;
                    cx.notify();
                }
            })
        })
        .detach_and_log_err(cx);
    }

    /// `thock::OpenGuide` and the checklist's first row: the introduction is
    /// a vault-local HTML page, opened like any browser link. Opening it is
    /// what completes the step — evidence, not a click on a checkbox.
    fn open_guide(&mut self, cx: &mut Context<Self>) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        let root = vault.root.clone();
        cx.open_with_system(&root.join(getting_started::GUIDE_PATH));
        let mark = cx
            .background_spawn(async move { crate::getting_started::mark_introduction_read(&root) });
        cx.spawn(async move |this, cx| {
            mark.await?;
            this.update(cx, |this, cx| this.refresh_getting_started(cx))
        })
        .detach_and_log_err(cx);
    }

    /// `thock::OpenCustomize` and the checklist's second row: the customize
    /// page (themes, text size, shortcuts) opened as a rendered preview,
    /// with the live theme selector popped on top — so "dark isn't the only
    /// look" is *shown*, not described. Opening it is what completes the
    /// step.
    fn open_customize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        let root = vault.root.clone();
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            crate::open_abs_path_as_preview(
                workspace,
                root.join(getting_started::CUSTOMIZE_PATH),
                cx,
            )
            .await?;
            // The page's first section narrates the picker now hovering over
            // it; arrowing through themes live is the whole lesson.
            cx.update(|window, cx| {
                window.dispatch_action(
                    zed_actions::theme_selector::Toggle::default().boxed_clone(),
                    cx,
                );
            })?;
            let mark =
                cx.background_spawn(async move { getting_started::mark_customize_read(&root) });
            mark.await?;
            this.update(cx, |this, cx| this.refresh_getting_started(cx))
        })
        .detach_and_log_err(cx);
    }

    /// `thock::HideGettingStarted`: puts the checklist away early. The step
    /// markers stay, so nothing is forgotten — only the section goes.
    fn hide_getting_started(&mut self, cx: &mut Context<Self>) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        let root = vault.root.clone();
        let dismiss = cx.background_spawn(async move {
            getting_started::dismiss(&root);
        });
        cx.spawn(async move |this, cx| {
            dismiss.await;
            this.update(cx, |this, cx| this.refresh_getting_started(cx))
        })
        .detach_and_log_err(cx);
    }

    fn run_getting_started_step(
        &mut self,
        step: GettingStartedStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match step {
            GettingStartedStep::Introduction => self.open_guide(cx),
            GettingStartedStep::Customize => self.open_customize(window, cx),
            GettingStartedStep::Agent => {
                window.dispatch_action(crate::agent_panel::ConnectAgent.boxed_clone(), cx);
            }
            GettingStartedStep::Tour => self.run_skill(
                "Welcome Tour".to_string(),
                getting_started::WELCOME_TOUR_SKILL_PATH.to_string(),
                crate::agent::ModelTier::Default,
                window,
                cx,
            ),
        }
    }

    /// Runs the reconcile pass (pre-V7 migration, core files, catalog
    /// re-materialization) in the background whenever a vault is
    /// (re)detected, so a vault opened after an app update self-heals any
    /// newly shipped files that a plain open would otherwise never create.
    /// Idempotent and never clobbers user edits.
    fn reconcile_routines(&mut self, cx: &mut Context<Self>) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        let vault = vault.clone();
        let reconcile = cx.background_spawn(async move { routines::reconcile_vault(&vault) });
        cx.spawn(async move |this, cx| {
            reconcile.await?;
            this.update(cx, |this, cx| {
                this.refresh_vault_status(cx);
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
    }

    /// Reloads the panel's Routine state: enabled sections from the
    /// registry, discovered-but-unregistered definitions, and the still-
    /// addable catalog entries.
    fn refresh_routines(&mut self) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            self.routines = Vec::new();
            self.discovered = Vec::new();
            self.addable_catalog = Vec::new();
            return;
        };
        self.routines = routines::enabled_routines(vault);
        let registered: HashSet<&str> = vault
            .config
            .routines
            .installed
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        self.discovered = routines::discover_routines(&vault.root)
            .into_iter()
            .filter(|discovered| !registered.contains(discovered.id.as_str()))
            .collect();
        self.addable_catalog = match routines::catalog() {
            Ok(catalog) => catalog
                .into_iter()
                .map(|routine| routine.manifest)
                .filter(|manifest| {
                    !vault
                        .config
                        .routines
                        .installed
                        .iter()
                        .any(|entry| entry.enabled && entry.id == manifest.id)
                })
                .collect(),
            Err(error) => {
                log::error!("Thock: couldn't load the Routines catalog: {error:?}");
                Vec::new()
            }
        };
    }

    /// Toasts that go out from synchronous call paths. Deferred because some
    /// callers (workspace action handlers) hold the workspace lease —
    /// updating the workspace inline would double-lease and panic.
    fn show_error_deferred(&self, message: String, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.show_error(message, cx))
                .log_err();
        });
    }

    fn show_toast_deferred(&self, id: NotificationId, message: String, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_toast(Toast::new(id, message).autohide(), cx);
                })
                .log_err();
        });
    }

    fn open_note(&mut self, entry: TimelineEntry, window: &mut Window, cx: &mut Context<Self>) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            // Only reachable via the `thock:` commands; the panel itself
            // renders no entries outside a valid vault.
            struct NotAVaultToast;
            self.show_toast_deferred(
                NotificationId::unique::<NotAVaultToast>(),
                "This workspace isn't a Thock vault.".to_string(),
                cx,
            );
            return;
        };
        let vault = vault.clone();
        let now = Local::now();
        let Some((kind, date)) = entry.resolve(now.date_naive()) else {
            return;
        };
        let time = now.time();
        let workspace = self.workspace.clone();

        let ensure_note = cx.background_spawn(async move { ensure_note(&vault, kind, date, time) });
        cx.spawn_in(window, async move |_, cx| match ensure_note.await {
            Ok((path, outcome)) => {
                if outcome == EnsureNoteOutcome::CreatedWithoutTemplate {
                    workspace
                        .update(cx, |workspace, cx| {
                            struct TemplateMissingToast;
                            workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<TemplateMissingToast>(),
                                    "The note template is missing, so an empty note was created.",
                                )
                                .autohide(),
                                cx,
                            );
                        })
                        .log_err();
                }
                workspace
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
                Ok(())
            }
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_error(format!("Couldn't open the note: {error}"), cx);
                    })
                    .log_err();
                Err(error)
            }
        })
        .detach_and_log_err(cx);
    }

    fn create_vault_here(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.workspace_root(cx) else {
            return;
        };
        let workspace = self.workspace.clone();
        let scaffold = cx.background_spawn(async move { scaffold_vault(&root) });
        cx.spawn(async move |this, cx| match scaffold.await {
            Ok(()) => this.update(cx, |this, cx| this.refresh_vault_status(cx)),
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_error(format!("Couldn't create the vault: {error}"), cx);
                    })
                    .log_err();
                Err(error)
            }
        })
        .detach_and_log_err(cx);
    }

    fn vault_root(&self) -> Option<PathBuf> {
        match &self.vault_status {
            VaultStatus::Valid(vault) => Some(vault.root.clone()),
            _ => None,
        }
    }

    /// Opens a Routine-shipped markdown file (explainer doc or skill) in
    /// viewing mode. A missing file gets a toast offering to re-materialize
    /// the Routine's files.
    fn open_routine_file(
        &mut self,
        relative_path: String,
        routine_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.vault_root() else {
            return;
        };
        let path = match routines::vault_file_path(&root, &relative_path) {
            Ok(path) => path,
            Err(error) => {
                self.show_error_deferred(format!("Couldn't open the file: {error}"), cx);
                return;
            }
        };
        if !path.is_file() {
            self.show_missing_file_toast(relative_path, routine_id, cx);
            return;
        }
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let open_result = crate::open_abs_path_as_preview(workspace.clone(), path, cx).await;
            if let Err(error) = &open_result {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_error(format!("Couldn't open the file: {error}"), cx);
                    })
                    .log_err();
            }
            open_result
        })
        .detach_and_log_err(cx);
    }

    /// Opens a quick link: resolve its date templates, create the target if
    /// the link says so, then dispatch on its declared kind (V7 §7.4).
    fn open_link(
        &mut self,
        routine_id: String,
        link: RoutineLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        let vault = vault.clone();
        let now = Local::now();
        let resolved = match routines::resolve_link(&vault, &link.open, now.date_naive()) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.show_error_deferred(format!("Couldn't open {}: {error}", link.name), cx);
                return;
            }
        };
        let time = now.time();
        let workspace = self.workspace.clone();
        let ensure = cx.background_spawn({
            let resolved = resolved.clone();
            let create = link.create;
            async move { routines::ensure_link_target(&vault, create, &resolved, time) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match ensure.await {
                Ok(path) => path,
                Err(error) => {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_error(format!("Couldn't open the link: {error}"), cx);
                        })
                        .log_err();
                    return Err(error);
                }
            };
            if !path.is_file() {
                this.update(cx, |this, cx| {
                    this.show_missing_file_toast(resolved.relative_path, routine_id, cx);
                })?;
                return Ok(());
            }
            match link.kind {
                LinkKind::Editor => {
                    workspace
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
                }
                LinkKind::Preview => {
                    crate::open_abs_path_as_preview(workspace.clone(), path, cx).await?;
                }
                LinkKind::Browser => {
                    // Zed has no web view, so browser links open with the
                    // system handler.
                    cx.update(|_, cx| cx.open_with_system(&path))?;
                }
            }
            Ok(())
        })
        .detach_and_log_err(cx);
    }

    /// `thock::OpenLink { routine, link }`: dispatch by the stable ids
    /// in `routine.toml`. Unknown or disabled ids get a non-blocking toast,
    /// never a panic (V7 §7.5).
    fn open_link_by_id(
        &mut self,
        routine_id: String,
        link_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = self.routines.iter().find_map(|load| match load {
            RoutineLoad::Loaded(manifest) if manifest.id == routine_id => manifest
                .links
                .iter()
                .find(|link| link.id == link_id)
                .cloned(),
            _ => None,
        });
        match link {
            Some(link) => self.open_link(routine_id, link, window, cx),
            None => self.show_error_deferred(
                format!("No enabled Routine {routine_id:?} with a link {link_id:?} in this vault."),
                cx,
            ),
        }
    }

    /// Launches a skill in the Agent panel (V5 §6.4): the kickoff points the
    /// user's agent at the live skill file. Connection checks, the
    /// pre-session checkpoint, and terminal lifecycle all live in the panel's
    /// launch path.
    fn run_skill(
        &mut self,
        title: String,
        file: String,
        tier: crate::agent::ModelTier,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace
            .update(cx, |workspace, cx| {
                crate::agent_panel::AgentPanel::launch_in_workspace(
                    workspace,
                    crate::agent_panel::LaunchRequest {
                        title,
                        kickoff: Some(crate::agent::run_skill_kickoff(&file)),
                        tier,
                    },
                    window,
                    cx,
                );
            })
            .log_err();
    }

    fn show_missing_file_toast(
        &mut self,
        relative_path: String,
        routine_id: String,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    struct RoutineFileMissingToast;
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<RoutineFileMissingToast>(),
                            format!("{relative_path} is missing from the vault."),
                        )
                        .on_click(
                            "Reinstall the Routine's files",
                            move |window, cx| {
                                panel
                                    .update(cx, |panel, cx| {
                                        panel.reinstall_routine(routine_id.clone(), window, cx)
                                    })
                                    .log_err();
                            },
                        ),
                        cx,
                    );
                })
                .log_err();
        });
    }

    /// Re-materializes a Routine's missing files: catalog Routines from
    /// their package, vault-authored ones from their definition (bridges and
    /// scaffold dirs — there is no package to restore file contents from).
    fn reinstall_routine(
        &mut self,
        routine_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.vault_root() else {
            return;
        };
        let workspace = self.workspace.clone();
        let reinstall = cx.background_spawn(async move {
            match routines::catalog_routine(&routine_id)? {
                Some(_) => routines::install_routine(&root, &routine_id),
                None => routines::activate_routine(&root, &routine_id).map(|_| ()),
            }
        });
        cx.spawn_in(window, async move |this, cx| match reinstall.await {
            Ok(()) => this.update(cx, |this, cx| this.refresh_vault_status(cx)),
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_error(
                            format!("Couldn't reinstall the Routine's files: {error}"),
                            cx,
                        );
                    })
                    .log_err();
                Err(error)
            }
        })
        .detach_and_log_err(cx);
    }

    /// Materializes a catalog Routine into the vault (or re-enables a
    /// disabled one) and registers it; the panel refreshes without a restart.
    /// For Routines that ship onboarding, a first install continues into the
    /// trigger flow (V5 §7.3): launch the setup session when an agent is
    /// connected, otherwise offer the skippable connect-first interstitial.
    fn install_routine(&mut self, routine_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.vault_root() else {
            return;
        };
        let workspace = self.workspace.clone();
        let install = cx.background_spawn({
            async move {
                routines::install_routine(&root, &routine_id)?;
                let entry_state = match Vault::detect(&root) {
                    VaultStatus::Valid(vault) => vault
                        .config
                        .routines
                        .installed
                        .iter()
                        .find(|entry| entry.id == routine_id)
                        .and_then(|entry| entry.onboarding_state),
                    _ => None,
                };
                // Only a pending install (first time, or reinstall after full
                // removal) triggers the onboarding flow; re-enabling a
                // Routine that was set up before doesn't relaunch it. The
                // onboarding pointer comes from the just-installed live
                // definition, not the compiled catalog.
                let onboarding = if entry_state == Some(OnboardingState::Pending) {
                    routines::load_vault_manifest(&root, &routine_id)?.and_then(|manifest| {
                        manifest
                            .onboarding
                            .as_ref()
                            .map(|onboarding| (manifest.name.clone(), onboarding.skill.clone()))
                    })
                } else {
                    None
                };
                let vault = match Vault::detect(&root) {
                    VaultStatus::Valid(vault) => Some(vault),
                    _ => None,
                };
                let connected = crate::agent::resolved_command(vault.as_ref()).is_some();
                anyhow::Ok((onboarding, connected))
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            match install.await {
                Ok((onboarding, connected)) => {
                    this.update(cx, |this, cx| {
                        this.show_add_routines = false;
                        this.refresh_vault_status(cx);
                    })?;
                    let Some((routine_name, onboarding_file)) = onboarding else {
                        return Ok(());
                    };
                    let title = format!("{routine_name} setup");
                    if connected {
                        this.update_in(cx, |this, window, cx| {
                            this.run_skill(
                                title,
                                onboarding_file,
                                crate::agent::ModelTier::Default,
                                window,
                                cx,
                            );
                        })?;
                    } else {
                        let answer = cx.update(|window, cx| {
                            window.prompt(
                                PromptLevel::Info,
                                &format!("{routine_name} is installed."),
                                Some(
                                    "Connect your agent to finish setup — it can migrate \
                                     your existing notes into this vault.",
                                ),
                                &["Connect Agent", "Skip"],
                                cx,
                            )
                        })?;
                        if answer.await.log_err() == Some(0) {
                            // The launch path itself routes through the
                            // connect flow first, then continues into this
                            // session.
                            this.update_in(cx, |this, window, cx| {
                                this.run_skill(
                                    title,
                                    onboarding_file,
                                    crate::agent::ModelTier::Default,
                                    window,
                                    cx,
                                );
                            })?;
                        }
                    }
                    Ok(())
                }
                Err(error) => {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_error(format!("Couldn't add the Routine: {error}"), cx);
                        })
                        .log_err();
                    Err(error)
                }
            }
        })
        .detach_and_log_err(cx);
    }

    /// Activates a discovered (vault-authored) Routine: the explicit commit
    /// that validates, records the hash lockfile, generates bridges, and
    /// registers it enabled (V7 §5.2).
    fn activate_routine(
        &mut self,
        routine_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.vault_root() else {
            return;
        };
        let workspace = self.workspace.clone();
        let activate =
            cx.background_spawn(async move { routines::activate_routine(&root, &routine_id) });
        cx.spawn_in(window, async move |this, cx| match activate.await {
            Ok(manifest) => {
                this.update(cx, |this, cx| {
                    this.show_add_routines = false;
                    this.refresh_vault_status(cx);
                })?;
                workspace
                    .update(cx, |workspace, cx| {
                        struct RoutineActivatedToast;
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<RoutineActivatedToast>(),
                                format!("{} is now an active Routine.", manifest.name),
                            )
                            .autohide(),
                            cx,
                        );
                    })
                    .log_err();
                Ok(())
            }
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_error(format!("Couldn't activate the Routine: {error}"), cx);
                    })
                    .log_err();
                Err(error)
            }
        })
        .detach_and_log_err(cx);
    }

    /// The New Routine ritual's completion channel (V7 §7.2): a ready marker
    /// plus a valid, unregistered definition earns one activation toast.
    fn offer_ready_routines(&mut self, cx: &mut Context<Self>) {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        for routine_id in routines::pending_ready_markers(&vault.root) {
            if self.ready_toasted.contains(&routine_id) {
                continue;
            }
            // A marker without a valid, unregistered definition yet (the
            // ritual may still be writing) isn't consumed — the next refresh
            // retries.
            let Some(name) = self.discovered.iter().find_map(|discovered| {
                match (&discovered.manifest, discovered.id == routine_id) {
                    (Ok(manifest), true) => Some(manifest.name.clone()),
                    _ => None,
                }
            }) else {
                continue;
            };
            self.ready_toasted.insert(routine_id.clone());
            let panel = cx.entity().downgrade();
            let workspace = self.workspace.clone();
            let toast_id = routine_id.clone();
            cx.defer(move |cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        struct RoutineReadyToast;
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::composite::<RoutineReadyToast>(SharedString::from(
                                    toast_id.clone(),
                                )),
                                format!("{name} is ready."),
                            )
                            .on_click(
                                "Activate Routine",
                                move |window, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            panel.activate_routine(toast_id.clone(), window, cx);
                                        })
                                        .log_err();
                                },
                            ),
                            cx,
                        );
                    })
                    .log_err();
            });
        }
    }

    /// Kicks off a background pass over every not-yet-onboarded Routine with
    /// an onboarding ritual: did the done marker appear, or did the 24 h
    /// window lapse? Transitions are persisted before their one-shot effects
    /// (tour, re-prompt) fire, so those effects can't repeat.
    fn schedule_onboarding_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.onboarding_check_running {
            self.onboarding_recheck = true;
            return;
        }
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return;
        };
        let mut candidates = Vec::new();
        for load in &self.routines {
            let RoutineLoad::Loaded(manifest) = load else {
                continue;
            };
            let Some(onboarding) = &manifest.onboarding else {
                continue;
            };
            let Some(entry) = vault
                .config
                .routines
                .installed
                .iter()
                .find(|entry| entry.id == manifest.id)
            else {
                continue;
            };
            if entry.onboarding_state == Some(OnboardingState::Onboarded) {
                continue;
            }
            candidates.push(OnboardingCandidate {
                routine_id: manifest.id.clone(),
                routine_name: manifest.name.clone(),
                doc: manifest.doc.clone(),
                onboarding_file: onboarding.skill.clone(),
                state: entry.onboarding_state,
                installed_at: entry.onboarding_installed_at,
            });
        }
        if candidates.is_empty() {
            return;
        }
        let root = vault.root.clone();
        let decide = cx.background_spawn({
            let root = root.clone();
            async move {
                let now = Utc::now();
                candidates
                    .into_iter()
                    .filter_map(|candidate| {
                        let marker = routines::onboarding_marker_path(&root, &candidate.routine_id)
                            .is_file();
                        match routines::check_onboarding(
                            candidate.state,
                            candidate.installed_at,
                            marker,
                            now,
                        ) {
                            routines::OnboardingCheck::Nothing => None,
                            check => Some((candidate, check)),
                        }
                    })
                    .collect::<Vec<_>>()
            }
        });
        self.onboarding_check_running = true;
        // Detached rather than stored: dropping the task mid-effect (e.g. on
        // a burst of filesystem events) could persist a transition and then
        // never fire its effect.
        cx.spawn_in(window, async move |this, cx| {
            for (candidate, check) in decide.await {
                let outcome = match check {
                    routines::OnboardingCheck::MarkOnboarded => {
                        Self::finish_onboarding(&this, &root, candidate, cx).await
                    }
                    routines::OnboardingCheck::PromptExpiry => {
                        Self::expire_onboarding(&this, &root, candidate, cx).await
                    }
                    routines::OnboardingCheck::Nothing => Ok(()),
                };
                outcome.log_err();
            }
            this.update_in(cx, |this, window, cx| {
                this.onboarding_check_running = false;
                if std::mem::take(&mut this.onboarding_recheck) {
                    this.schedule_onboarding_check(window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The done marker appeared: persist `onboarded`, clear the badge, and
    /// open the Routine's explainer doc as the capabilities tour (V5 §7.6).
    async fn finish_onboarding(
        this: &WeakEntity<Self>,
        root: &PathBuf,
        candidate: OnboardingCandidate,
        cx: &mut AsyncWindowContext,
    ) -> Result<()> {
        let transitioned = cx
            .background_spawn({
                let root = root.clone();
                let routine_id = candidate.routine_id.clone();
                async move {
                    routines::set_onboarding_state(&root, &routine_id, OnboardingState::Onboarded)
                }
            })
            .await?;
        if !transitioned {
            return Ok(());
        }
        let workspace = this.update(cx, |this, cx| {
            this.refresh_vault_status(cx);
            this.workspace.clone()
        })?;
        let doc_path = routines::vault_file_path(root, &candidate.doc)?;
        if doc_path.is_file() {
            crate::open_abs_path_as_preview(workspace.clone(), doc_path, cx).await?;
        }
        workspace.update(cx, |workspace, cx| {
            struct OnboardedToast;
            workspace.show_toast(
                Toast::new(
                    NotificationId::unique::<OnboardedToast>(),
                    format!("{} is set up.", candidate.routine_name),
                )
                .autohide(),
                cx,
            );
        })?;
        Ok(())
    }

    /// 24 h passed without a marker: persist `expired` first (so the prompt
    /// can never fire twice), then re-prompt once via a toast. After this,
    /// only the quiet Set-up-with-AI run action remains.
    async fn expire_onboarding(
        this: &WeakEntity<Self>,
        root: &PathBuf,
        candidate: OnboardingCandidate,
        cx: &mut AsyncWindowContext,
    ) -> Result<()> {
        let transitioned = cx
            .background_spawn({
                let root = root.clone();
                let routine_id = candidate.routine_id.clone();
                async move {
                    routines::set_onboarding_state(&root, &routine_id, OnboardingState::Expired)
                }
            })
            .await?;
        if !transitioned {
            return Ok(());
        }
        this.update(cx, |this, cx| {
            this.refresh_vault_status(cx);
            let panel = cx.entity().downgrade();
            let routine_name = candidate.routine_name.clone();
            let title = format!("{routine_name} setup");
            let onboarding_file = candidate.onboarding_file.clone();
            this.workspace
                .update(cx, |workspace, cx| {
                    struct OnboardingExpiryToast;
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<OnboardingExpiryToast>(),
                            format!("Still want help setting up {routine_name}?"),
                        )
                        .on_click("Set up with AI", move |window, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    panel.run_skill(
                                        title.clone(),
                                        onboarding_file.clone(),
                                        crate::agent::ModelTier::Default,
                                        window,
                                        cx,
                                    );
                                })
                                .log_err();
                        }),
                        cx,
                    );
                })
                .log_err();
        })?;
        Ok(())
    }

    /// Removing always asks: deactivate (keep all files) or deactivate and
    /// delete the Routine's declared files. The prompt lists exactly what
    /// would be deleted; user notes and modified-since-install files are
    /// never deleted (lockfile provenance, V7 §5.3).
    fn remove_routine(&mut self, routine_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.vault_root() else {
            return;
        };
        let workspace = self.workspace.clone();
        let plan = cx.background_spawn({
            let root = root.clone();
            let routine_id = routine_id.clone();
            async move { routines::plan_removal(&root, &routine_id) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let plan = match plan.await {
                Ok(plan) => plan,
                Err(error) => {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_error(
                                format!("Couldn't prepare the Routine removal: {error}"),
                                cx,
                            );
                        })
                        .log_err();
                    return Err(error);
                }
            };

            let mut detail = String::new();
            if plan.vault_authored {
                detail.push_str(
                    "Created in this vault — its files were authored by you or your agent.\n\n",
                );
            }
            if plan.delete.is_empty() {
                detail.push_str("No Routine files would be deleted.\n");
            } else {
                detail.push_str("Deleting the Routine's files removes:\n");
                for file in &plan.delete {
                    detail.push_str("  - ");
                    detail.push_str(file);
                    detail.push('\n');
                }
            }
            if !plan.keep_modified.is_empty() {
                detail.push_str("\nModified since install, always kept:\n");
                for file in &plan.keep_modified {
                    detail.push_str("  - ");
                    detail.push_str(file);
                    detail.push('\n');
                }
            }
            detail.push_str("\nYour notes are never deleted.");

            let answer = cx.update(|window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    &format!("Remove the {} Routine?", plan.routine_name),
                    Some(&detail),
                    &[
                        "Deactivate, Keep All Files",
                        "Deactivate and Delete Routine Files",
                        "Cancel",
                    ],
                    cx,
                )
            })?;
            let Some(answer) = answer.await.log_err() else {
                return Ok(());
            };

            let operation =
                match answer {
                    0 => {
                        cx.background_spawn({
                            let root = root.clone();
                            let routine_id = routine_id.clone();
                            async move {
                                routines::deactivate_routine(&root, &routine_id).map(|()| None)
                            }
                        })
                        .await
                    }
                    1 => {
                        cx.background_spawn({
                            let root = root.clone();
                            let routine_id = routine_id.clone();
                            async move { routines::delete_routine(&root, &routine_id).map(Some) }
                        })
                        .await
                    }
                    _ => return Ok(()),
                };

            match operation {
                Ok(outcome) => {
                    let message = match outcome {
                        None => format!(
                            "Deactivated the {} Routine. All of its files were kept.",
                            plan.routine_name
                        ),
                        Some(outcome) => {
                            let mut message = format!(
                                "Removed the {} Routine and deleted {} of its files.",
                                plan.routine_name,
                                outcome.deleted.len()
                            );
                            if !outcome.kept_modified.is_empty() {
                                message.push_str(&format!(
                                    " Kept {} modified: {}.",
                                    outcome.kept_modified.len(),
                                    outcome.kept_modified.join(", ")
                                ));
                            }
                            message
                        }
                    };
                    this.update(cx, |this, cx| this.refresh_vault_status(cx))?;
                    workspace
                        .update(cx, |workspace, cx| {
                            struct RoutineRemovedToast;
                            workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<RoutineRemovedToast>(),
                                    message,
                                )
                                .autohide(),
                                cx,
                            );
                        })
                        .log_err();
                    Ok(())
                }
                Err(error) => {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .show_error(format!("Couldn't remove the Routine: {error}"), cx);
                        })
                        .log_err();
                    Err(error)
                }
            }
        })
        .detach_and_log_err(cx);
    }

    fn section_items(&self, manifest: &RoutineManifest) -> Vec<SectionItem> {
        section_items(manifest, &self.expanded_groups)
    }

    /// The flat, visible row list keyboard selection walks: the Getting
    /// started steps when the checklist is active, then every expanded,
    /// cleanly loaded Routine's selectable section items, in section order.
    fn nav_rows(&self) -> Vec<NavRow> {
        let mut rows = Vec::new();
        if self.getting_started.is_some() {
            for step in GettingStartedStep::ALL {
                rows.push(NavRow {
                    routine_id: GETTING_STARTED_SECTION_ID.to_string(),
                    group: None,
                    kind: NavRowKind::GettingStarted(step),
                });
            }
        }
        for load in &self.routines {
            let RoutineLoad::Loaded(manifest) = load else {
                continue;
            };
            if self.collapsed_routines.contains(&manifest.id) {
                continue;
            }
            for item in self.section_items(manifest) {
                let kind = match item.kind {
                    SectionItemKind::Caption(_) => continue,
                    SectionItemKind::Link(link) => NavRowKind::Link(link),
                    SectionItemKind::Skill(skill) => NavRowKind::Skill(skill),
                    SectionItemKind::Group { label, .. } => NavRowKind::Group(label),
                };
                rows.push(NavRow {
                    routine_id: manifest.id.clone(),
                    group: item.group,
                    kind,
                });
            }
        }
        rows
    }

    fn manifest(&self, routine_id: &str) -> Option<&RoutineManifest> {
        self.routines.iter().find_map(|load| match load {
            RoutineLoad::Loaded(manifest) if manifest.id == routine_id => Some(manifest),
            _ => None,
        })
    }

    /// Onboarding runs are titled after the Routine rather than the skill, so
    /// the agent thread reads as "<Routine> setup".
    fn skill_run_title(&self, routine_id: &str, skill: &RoutineSkill) -> String {
        self.manifest(routine_id)
            .filter(|manifest| {
                manifest
                    .onboarding
                    .as_ref()
                    .is_some_and(|onboarding| onboarding.skill == skill.file)
            })
            .map_or_else(
                || skill.name.clone(),
                |manifest| format!("{} setup", manifest.name),
            )
    }

    fn toggle_group(&mut self, routine_id: &str, label: &str, cx: &mut Context<Self>) {
        let expanded = !self.expanded_groups.contains(&group_key(routine_id, label));
        self.set_group_expanded(routine_id, label, expanded, cx);
    }

    /// Opens or closes a group, keeping the keyboard cursor on the row it was
    /// on. A cursor inside a group that just closed lands on that group's own
    /// row — never on whatever index happens to survive the list shrinking.
    fn set_group_expanded(
        &mut self,
        routine_id: &str,
        label: &str,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        let key = group_key(routine_id, label);
        if self.expanded_groups.contains(&key) == expanded {
            return;
        }
        // Only a real keyboard cursor is worth preserving; a `None` selection
        // is following the active editor item and should keep doing so.
        let anchor = self
            .selected_index
            .is_some()
            .then(|| self.selected_row(cx).map(|row| row_key(&row)))
            .flatten();
        if expanded {
            self.expanded_groups.insert(key);
        } else {
            self.expanded_groups.remove(&key);
        }
        if let Some(anchor) = anchor {
            let group_row = nav_key(routine_id, "group", label);
            self.selected_index = reanchor_selection(&self.nav_rows(), &anchor, &group_row);
        }
        cx.notify();
    }

    /// The absolute path of the note open in the active editor, if any.
    fn active_item_path(&self, cx: &App) -> Option<PathBuf> {
        let workspace = self.workspace.upgrade()?;
        let item = workspace.read(cx).active_item(cx)?;
        let project_path = item.project_path(cx)?;
        self.project.read(cx).absolute_path(&project_path, cx)
    }

    /// The row whose resolved link path matches the active editor item, if
    /// any — the generalization of the old fixed-entry highlight.
    fn active_row_index(&self, rows: &[NavRow], cx: &App) -> Option<usize> {
        let VaultStatus::Valid(vault) = &self.vault_status else {
            return None;
        };
        let active_path = self.active_item_path(cx)?;
        let today = Local::now().date_naive();
        rows.iter().position(|row| match &row.kind {
            NavRowKind::Link(link) => routines::resolve_link(vault, &link.open, today)
                .ok()
                .and_then(|resolved| {
                    routines::vault_file_path(&vault.root, &resolved.relative_path).ok()
                })
                .is_some_and(|path| path == active_path),
            NavRowKind::Skill(_) | NavRowKind::Group(_) | NavRowKind::GettingStarted(_) => false,
        })
    }

    /// The highlighted row: the keyboard cursor if one is set, otherwise the
    /// row matching the active editor item.
    fn effective_selected_index(&self, rows: &[NavRow], cx: &App) -> Option<usize> {
        self.selected_index
            .filter(|index| *index < rows.len())
            .or_else(|| self.active_row_index(rows, cx))
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.nav_rows();
        if rows.is_empty() {
            return;
        }
        self.selected_index = Some(match self.effective_selected_index(&rows, cx) {
            Some(index) => (index + 1).min(rows.len() - 1),
            None => 0,
        });
        cx.notify();
    }

    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.nav_rows();
        if rows.is_empty() {
            return;
        }
        self.selected_index = Some(match self.effective_selected_index(&rows, cx) {
            Some(index) => index.saturating_sub(1),
            None => rows.len() - 1,
        });
        cx.notify();
    }

    fn select_first(&mut self, _: &SelectFirst, _window: &mut Window, cx: &mut Context<Self>) {
        if self.nav_rows().is_empty() {
            return;
        }
        self.selected_index = Some(0);
        cx.notify();
    }

    fn select_last(&mut self, _: &SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.nav_rows();
        if rows.is_empty() {
            return;
        }
        self.selected_index = Some(rows.len() - 1);
        cx.notify();
    }

    fn selected_row(&self, cx: &App) -> Option<NavRow> {
        let rows = self.nav_rows();
        self.effective_selected_index(&rows, cx)
            .and_then(|index| rows.into_iter().nth(index))
    }

    /// A row does the obvious thing: a link opens, a ritual runs, a group
    /// opens or closes. Reading a ritual's instructions is `ViewSkill`.
    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        match row.kind {
            NavRowKind::Link(link) => self.open_link(row.routine_id, link, window, cx),
            NavRowKind::Skill(skill) => {
                let title = self.skill_run_title(&row.routine_id, &skill);
                self.run_skill(title, skill.file, skill.model, window, cx);
            }
            NavRowKind::Group(label) => self.toggle_group(&row.routine_id, &label, cx),
            NavRowKind::GettingStarted(step) => self.run_getting_started_step(step, window, cx),
        }
    }

    fn view_skill(&mut self, _: &ViewSkill, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        match row.kind {
            NavRowKind::Skill(skill) => {
                self.open_routine_file(skill.file, row.routine_id, window, cx);
            }
            // The tour runs on enter like a ritual, so reading its
            // instructions gets the same affordance ritual rows have.
            NavRowKind::GettingStarted(GettingStartedStep::Tour) => {
                self.open_routine_file(
                    getting_started::WELCOME_TOUR_SKILL_PATH.to_string(),
                    row.routine_id,
                    window,
                    cx,
                );
            }
            _ => {}
        }
    }

    fn expand_group(&mut self, _: &ExpandGroup, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        let NavRowKind::Group(label) = &row.kind else {
            return;
        };
        self.set_group_expanded(&row.routine_id, label, true, cx);
    }

    /// Closes the selected group, or the group the selected row lives in —
    /// then parks the cursor on the group row, so collapsing never drops the
    /// selection somewhere the user wasn't looking.
    fn collapse_group(&mut self, _: &CollapseGroup, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        let label = match (&row.kind, &row.group) {
            (NavRowKind::Group(label), _) | (_, Some(label)) => label.clone(),
            _ => return,
        };
        self.set_group_expanded(&row.routine_id, &label, false, cx);
    }

    fn render_routine_section(
        &self,
        manifest: &RoutineManifest,
        row_index: &mut usize,
        selected_index: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let routine_id = manifest.id.clone();
        let expanded = !self.collapsed_routines.contains(&manifest.id);
        // "Finish setup" chip: onboarding is pending (V5 §7.3). Cleared by
        // the done marker or the 24 h expiry, both persisted transitions.
        let finishing_setup = manifest.onboarding.is_some()
            && matches!(
                &self.vault_status,
                VaultStatus::Valid(vault) if vault.config.routines.installed.iter().any(|entry| {
                    entry.id == manifest.id
                        && entry.onboarding_state == Some(OnboardingState::Pending)
                })
            );
        let mut section = v_flex().child(
            ListItem::new(ElementId::Name(SharedString::from(format!(
                "thock-routine-{}",
                manifest.id
            ))))
            .toggle(expanded)
            .always_show_disclosure_icon(true)
            .on_toggle(cx.listener({
                let routine_id = routine_id.clone();
                move |this, _, _window, cx| {
                    if !this.collapsed_routines.remove(&routine_id) {
                        this.collapsed_routines.insert(routine_id.clone());
                    }
                    cx.notify();
                }
            }))
            .start_slot(
                routine_icon(manifest.icon.as_deref())
                    .build()
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(Label::new(manifest.name.clone()))
            .when(finishing_setup, |item| {
                item.end_slot(
                    h_flex()
                        .px_1()
                        .rounded_sm()
                        .bg(cx.theme().colors().element_background)
                        .child(
                            Label::new("Finish setup")
                                .size(LabelSize::XSmall)
                                .color(Color::Accent),
                        ),
                )
            })
            // Removal is destructive and rare, so it stays off the header
            // until the pointer is actually on the row.
            .end_slot_on_hover(
                IconButton::new(
                    ElementId::Name(SharedString::from(format!(
                        "thock-remove-routine-{}",
                        manifest.id
                    ))),
                    IconName::Trash,
                )
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text("Remove Routine…"))
                .on_click(cx.listener({
                    let routine_id = routine_id.clone();
                    move |this, _, window, cx| {
                        this.remove_routine(routine_id.clone(), window, cx);
                    }
                })),
            )
            .on_click(cx.listener({
                let routine_id = routine_id.clone();
                let doc = manifest.doc.clone();
                move |this, _, window, cx| {
                    this.open_routine_file(doc.clone(), routine_id.clone(), window, cx);
                }
            })),
        );
        if !expanded {
            return section.into_any_element();
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        // Captions take no index, which is exactly what makes them unreachable
        // by the cursor; everything else consumes one, in rendered order.
        let mut next_index = || {
            let index = *row_index;
            *row_index += 1;
            index
        };
        for item in self.section_items(manifest) {
            let nested = item.group.is_some();
            rows.push(match item.kind {
                SectionItemKind::Caption(label) => render_caption(label),
                SectionItemKind::Link(link) => {
                    let index = next_index();
                    self.render_link_row(&routine_id, &link, nested, index, selected_index, cx)
                }
                SectionItemKind::Skill(skill) => {
                    let index = next_index();
                    self.render_skill_row(manifest, &skill, nested, index, selected_index, cx)
                }
                SectionItemKind::Group {
                    label,
                    expanded: group_expanded,
                } => {
                    let index = next_index();
                    self.render_group_row(
                        &routine_id,
                        label,
                        group_expanded,
                        index,
                        selected_index,
                        cx,
                    )
                }
            });
        }
        section = section.children(rows);
        section.into_any_element()
    }

    fn render_link_row(
        &self,
        routine_id: &str,
        link: &RoutineLink,
        nested: bool,
        index: usize,
        selected_index: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        // Every row carries an icon so labels stay aligned.
        let icon = link_icon(link);
        let open = OpenLink {
            routine: routine_id.to_string(),
            link: link.id.clone(),
        };
        ListItem::new(ElementId::Name(SharedString::from(format!(
            "thock-link-{routine_id}-{}",
            link.id
        ))))
        .indent_level(if nested { 2 } else { 1 })
        .indent_step_size(px(12.))
        .toggle_state(selected_index == Some(index))
        .start_slot(icon.build().size(IconSize::XSmall).color(Color::Muted))
        .child(Label::new(link.name.clone()).size(LabelSize::Small))
        // An unbound action renders as nothing, so a chord appears the moment
        // the user binds one — which is where binding gets discovered.
        .end_slot(KeyBinding::for_action_in(&open, &self.focus_handle, cx).size(rems_from_px(10.)))
        .on_click(cx.listener({
            let routine_id = routine_id.to_string();
            let link = link.clone();
            move |this, _, window, cx| {
                this.open_link(routine_id.clone(), link.clone(), window, cx);
            }
        }))
        .into_any_element()
    }

    fn render_skill_row(
        &self,
        manifest: &RoutineManifest,
        skill: &RoutineSkill,
        nested: bool,
        index: usize,
        selected_index: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let run = RunSkill {
            skill: Some(skill.id.clone()),
        };
        let title = self.skill_run_title(&manifest.id, skill);
        ListItem::new(ElementId::Name(SharedString::from(format!(
            "thock-skill-{}-{}",
            manifest.id, skill.id
        ))))
        .indent_level(if nested { 2 } else { 1 })
        .indent_step_size(px(12.))
        .toggle_state(selected_index == Some(index))
        .start_slot(skill_icon(skill).build().size(IconSize::XSmall).color(
            if skill.kind.is_setup() {
                Color::Muted
            } else {
                Color::Accent
            },
        ))
        .child(Label::new(skill.name.clone()).size(LabelSize::Small))
        .when(!skill.summary.is_empty(), |item| {
            item.tooltip(Tooltip::text(skill.summary.clone()))
        })
        .end_slot(KeyBinding::for_action_in(&run, &self.focus_handle, cx).size(rems_from_px(10.)))
        // The row itself runs the ritual, so reading what it will do needs
        // its own affordance.
        .end_slot_on_hover(
            IconButton::new(
                ElementId::Name(SharedString::from(format!(
                    "thock-view-skill-{}-{}",
                    manifest.id, skill.id
                ))),
                IconName::FileDoc,
            )
            .icon_size(IconSize::XSmall)
            .icon_color(Color::Muted)
            .tooltip(Tooltip::text("Open these instructions"))
            .on_click(cx.listener({
                let routine_id = manifest.id.clone();
                let file = skill.file.clone();
                move |this, _, window, cx| {
                    this.open_routine_file(file.clone(), routine_id.clone(), window, cx);
                }
            })),
        )
        .on_click(cx.listener({
            let file = skill.file.clone();
            let tier = skill.model;
            move |this, _, window, cx| {
                this.run_skill(title.clone(), file.clone(), tier, window, cx);
            }
        }))
        .into_any_element()
    }

    fn render_group_row(
        &self,
        routine_id: &str,
        label: SharedString,
        expanded: bool,
        index: usize,
        selected_index: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        ListItem::new(ElementId::Name(SharedString::from(format!(
            "thock-group-{routine_id}-{label}"
        ))))
        .indent_level(1)
        .indent_step_size(px(12.))
        .toggle_state(selected_index == Some(index))
        // The disclosure sits in the start slot, not in `ListItem::toggle`'s
        // own column, so the chevron lands exactly where its siblings' icons
        // do and the group reads as one of them.
        .start_slot(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(IconSize::XSmall)
            .color(Color::Muted),
        )
        .child(
            Label::new(label.clone())
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .on_click(cx.listener({
            let routine_id = routine_id.to_string();
            move |this, _, _window, cx| this.toggle_group(&routine_id, &label, cx)
        }))
        .into_any_element()
    }

    /// An enabled Routine whose definition is missing or invalid collapses
    /// to a visible error row — never a silent skip or a broken panel (V7
    /// §3 criterion 3). Clicking opens the definition to fix it.
    fn render_invalid_routine(
        &self,
        routine_id: &str,
        error: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let manifest_rel = format!(
            "{}/{routine_id}/{}",
            routines::ROUTINES_DIR,
            routines::ROUTINE_MANIFEST_FILE
        );
        ListItem::new(ElementId::Name(SharedString::from(format!(
            "thock-invalid-routine-{routine_id}"
        ))))
        .start_slot(
            Icon::new(IconName::Warning)
                .size(IconSize::Small)
                .color(Color::Error),
        )
        .child(
            Label::new(format!("{routine_id} — invalid routine.toml"))
                .size(LabelSize::Small)
                .color(Color::Error),
        )
        .tooltip(Tooltip::text(error.to_string()))
        .on_click(cx.listener({
            let routine_id = routine_id.to_string();
            move |this, _, window, cx| {
                this.open_definition_in_editor(
                    manifest_rel.clone(),
                    routine_id.clone(),
                    window,
                    cx,
                );
            }
        }))
        .into_any_element()
    }

    /// Opens a `routine.toml` as a plain editor buffer (fix-it path for
    /// error rows).
    fn open_definition_in_editor(
        &mut self,
        relative_path: String,
        _routine_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.vault_root() else {
            return;
        };
        let Ok(path) = routines::vault_file_path(&root, &relative_path) else {
            return;
        };
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            workspace
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
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn render_add_routine(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut list = v_flex().child(
            ListItem::new("thock-add-routine")
                .toggle_state(self.show_add_routines)
                .start_slot(
                    Icon::new(IconName::Plus)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .child(Label::new("Add Routine").color(Color::Muted))
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.show_add_routines = !this.show_add_routines;
                    cx.notify();
                })),
        );
        if !self.show_add_routines {
            return list;
        }

        if !self.discovered.is_empty() {
            list = list.child(
                div().px_2().pt_1().child(
                    Label::new("In this vault")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
            );
            for discovered in &self.discovered {
                list = list.child(match &discovered.manifest {
                    Ok(manifest) => {
                        let routine_id = manifest.id.clone();
                        let tooltip = if manifest.summary.is_empty() {
                            "Created in this vault.".to_string()
                        } else {
                            format!("{} — created in this vault.", manifest.summary)
                        };
                        ListItem::new(ElementId::Name(SharedString::from(format!(
                            "thock-activate-routine-{}",
                            manifest.id
                        ))))
                        .indent_level(1)
                        .indent_step_size(px(12.))
                        .start_slot(
                            routine_icon(manifest.icon.as_deref())
                                .build()
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(Label::new(manifest.name.clone()).size(LabelSize::Small))
                        .tooltip(Tooltip::text(tooltip))
                        .end_slot(
                            Label::new("Activate")
                                .size(LabelSize::XSmall)
                                .color(Color::Accent),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_routine(routine_id.clone(), window, cx);
                        }))
                        .into_any_element()
                    }
                    Err(error) => {
                        let routine_id = discovered.id.clone();
                        let manifest_rel = format!(
                            "{}/{routine_id}/{}",
                            routines::ROUTINES_DIR,
                            routines::ROUTINE_MANIFEST_FILE
                        );
                        ListItem::new(ElementId::Name(SharedString::from(format!(
                            "thock-discovered-invalid-{routine_id}"
                        ))))
                        .indent_level(1)
                        .indent_step_size(px(12.))
                        .start_slot(
                            Icon::new(IconName::Warning)
                                .size(IconSize::XSmall)
                                .color(Color::Error),
                        )
                        .child(
                            Label::new(format!("{routine_id} — invalid routine.toml"))
                                .size(LabelSize::Small)
                                .color(Color::Error),
                        )
                        .tooltip(Tooltip::text(error.clone()))
                        .on_click(cx.listener({
                            move |this, _, window, cx| {
                                this.open_definition_in_editor(
                                    manifest_rel.clone(),
                                    routine_id.clone(),
                                    window,
                                    cx,
                                );
                            }
                        }))
                        .into_any_element()
                    }
                });
            }
        }

        for manifest in &self.addable_catalog {
            let routine_id = manifest.id.clone();
            list = list.child(
                ListItem::new(ElementId::Name(SharedString::from(format!(
                    "thock-install-routine-{}",
                    manifest.id
                ))))
                .indent_level(1)
                .indent_step_size(px(12.))
                .start_slot(
                    routine_icon(manifest.icon.as_deref())
                        .build()
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(Label::new(manifest.name.clone()).size(LabelSize::Small))
                .when(!manifest.summary.is_empty(), |item| {
                    item.tooltip(Tooltip::text(manifest.summary.clone()))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.install_routine(routine_id.clone(), window, cx);
                })),
            );
        }

        list = list.child(
            ListItem::new("thock-new-routine")
                .indent_level(1)
                .indent_step_size(px(12.))
                .start_slot(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(Label::new("New Routine with AI").size(LabelSize::Small))
                .tooltip(Tooltip::text(
                    "Your agent interviews you and writes the Routine's definition, \
                     docs, and skills into this vault.",
                ))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.new_routine(window, cx);
                })),
        );
        list
    }

    fn new_routine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                crate::agent_panel::AgentPanel::launch_in_workspace(
                    workspace,
                    new_routine_launch_request(),
                    window,
                    cx,
                );
            })
            .log_err();
    }

    fn render_non_vault(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .p_2()
            .child(Label::new("This folder isn't a Thock vault.").color(Color::Muted))
            .child(
                Button::new("thock-create-vault", "Create vault here").on_click(cx.listener(
                    |this, _, _window, cx| {
                        this.create_vault_here(cx);
                    },
                )),
            )
    }

    fn render_invalid(&self, error: &str) -> impl IntoElement {
        v_flex()
            .gap_2()
            .p_2()
            .child(Label::new("This vault's config couldn't be loaded.").color(Color::Muted))
            .child(
                Label::new(error.to_string())
                    .size(LabelSize::Small)
                    .color(Color::Error),
            )
            .child(
                Label::new("Fix .thock/config.toml and the panel will recover.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }

    /// The Getting started section (V18 §5.4): a caption plus three checkable
    /// step rows, pinned above the Routine sections while the checklist is
    /// active. Completed steps stay visible (checked, muted) until all three
    /// are done, at which point the whole section retires itself.
    fn render_getting_started(
        &self,
        steps: getting_started::Steps,
        row_index: &mut usize,
        selected_index: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut section = v_flex().child(render_caption("Getting started".into()));
        for step in GettingStartedStep::ALL {
            let index = *row_index;
            *row_index += 1;
            let done = step.done(steps);
            section = section.child(
                ListItem::new(ElementId::Name(SharedString::from(format!(
                    "thock-getting-started-{}",
                    step.key()
                ))))
                .indent_level(1)
                .indent_step_size(px(12.))
                .toggle_state(selected_index == Some(index))
                .start_slot(
                    Icon::new(if done {
                        IconName::TodoComplete
                    } else {
                        IconName::TodoPending
                    })
                    .size(IconSize::XSmall)
                    .color(if done { Color::Success } else { Color::Accent }),
                )
                .child(
                    Label::new(step.label())
                        .size(LabelSize::Small)
                        .color(if done { Color::Muted } else { Color::Default }),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_getting_started_step(step, window, cx);
                })),
            );
        }
        section.into_any_element()
    }

    fn render_routines(&self, cx: &Context<Self>) -> AnyElement {
        let rows = self.nav_rows();
        let selected_index = self.effective_selected_index(&rows, cx);
        let mut row_index = 0usize;
        let mut sections = Vec::new();
        if let Some(steps) = self.getting_started {
            sections.push(self.render_getting_started(steps, &mut row_index, selected_index, cx));
        }
        for load in &self.routines {
            sections.push(match load {
                RoutineLoad::Loaded(manifest) => {
                    self.render_routine_section(manifest, &mut row_index, selected_index, cx)
                }
                RoutineLoad::Invalid { id, error } => self.render_invalid_routine(id, error, cx),
            });
        }
        v_flex()
            .gap_1()
            .when(self.routines.is_empty(), |this| {
                this.child(
                    div().px_2().py_1().child(
                        Label::new("No Routines are enabled in this vault.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .children(sections)
            // Add Routine is vault chrome, not part of the last section —
            // separate it clearly.
            .child(div().mt_3().mb_1().px_1().child(Divider::horizontal()))
            .child(self.render_add_routine(cx))
            .into_any_element()
    }
}

impl Render for RoutinesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.vault_status {
            VaultStatus::Valid(_) => self.render_routines(cx),
            VaultStatus::NotAVault => self.render_non_vault(cx).into_any_element(),
            VaultStatus::Invalid { error } => self.render_invalid(error).into_any_element(),
        };
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("ThockRoutinesPanel");
        key_context.add("menu");
        v_flex()
            .id("thock-routines-panel")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::view_skill))
            .on_action(cx.listener(Self::expand_group))
            .on_action(cx.listener(Self::collapse_group))
            .size_full()
            .overflow_y_scroll()
            .pt_2()
            .pb_2()
            // `ListItem` hangs its disclosure a full rem outside the row's
            // content box, so the left inset has to cover that overhang or the
            // Routine header's chevron sits on the panel border.
            .pl_4()
            .pr_1()
            .child(content)
    }
}

impl EventEmitter<PanelEvent> for RoutinesPanel {}

impl Focusable for RoutinesPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for RoutinesPanel {
    fn persistent_name() -> &'static str {
        // The pre-V7 name, kept so existing dock layout state carries over.
        "Thock Timeline Panel"
    }

    fn panel_key() -> &'static str {
        ROUTINES_PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(240.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Notepad)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Routines")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        ToggleFocus.boxed_clone()
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        true
    }

    fn activation_priority(&self) -> u32 {
        // Must be unique across all panels; 0-3 and 5-7 are taken upstream.
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::routines::SkillKind;

    fn link(open: &str, kind: LinkKind, icon: Option<&str>) -> RoutineLink {
        RoutineLink {
            id: "row".into(),
            name: "Row".into(),
            open: open.into(),
            kind,
            icon: icon.map(str::to_string),
            group: None,
            create: false,
        }
    }

    fn skill(icon: Option<&str>) -> RoutineSkill {
        skill_of_kind(SkillKind::Ritual, icon)
    }

    fn skill_of_kind(kind: SkillKind, icon: Option<&str>) -> RoutineSkill {
        RoutineSkill {
            id: "row".into(),
            name: "Row".into(),
            file: "routines/x/skills/row.md".into(),
            kind,
            model: crate::agent::ModelTier::Default,
            summary: String::new(),
            icon: icon.map(str::to_string),
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    #[test]
    fn row_icons_default_by_kind() {
        assert_eq!(
            link_icon(&link("daily/2026-08-17.md", LinkKind::Editor, None)),
            RowIcon::Named(IconName::Notepad)
        );
        assert_eq!(
            link_icon(&link("daily/2026-08-17.md", LinkKind::Preview, None)),
            RowIcon::Named(IconName::Notepad)
        );
        assert_eq!(
            link_icon(&link("finance/ledger.csv", LinkKind::Editor, None)),
            RowIcon::Named(IconName::FileCode)
        );
        assert_eq!(
            link_icon(&link("weekly/site/index.html", LinkKind::Browser, None)),
            RowIcon::Path("icons/file_icons/html.svg")
        );
        // A verb reads as a verb: rituals default to the run glyph, setup
        // steps to the settings one.
        assert_eq!(
            skill_icon(&skill(None)),
            RowIcon::Named(IconName::PlayOutlined)
        );
        assert_eq!(
            skill_icon(&skill_of_kind(SkillKind::Setup, None)),
            RowIcon::Named(IconName::Settings)
        );
    }

    fn named_link(id: &str, group: Option<&str>) -> RoutineLink {
        RoutineLink {
            id: id.into(),
            name: id.into(),
            open: format!("daily/{id}.md"),
            kind: LinkKind::Editor,
            icon: None,
            group: group.map(str::to_string),
            create: false,
        }
    }

    fn named_skill(id: &str, kind: SkillKind) -> RoutineSkill {
        RoutineSkill {
            id: id.into(),
            name: id.into(),
            file: format!("routines/x/skills/{id}.md"),
            kind,
            model: crate::agent::ModelTier::Default,
            summary: String::new(),
            icon: None,
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    fn manifest(links: Vec<RoutineLink>, skills: Vec<RoutineSkill>) -> RoutineManifest {
        RoutineManifest {
            schema: 2,
            id: "x".into(),
            name: "X".into(),
            version: 1,
            summary: String::new(),
            icon: None,
            doc: "routines/x/X.md".into(),
            agent_doc: None,
            links,
            scaffold: Vec::new(),
            skills,
            onboarding: None,
            warnings: Vec::new(),
        }
    }

    /// A compact transcript of a section, so the ordering rules read as one
    /// list rather than a pile of index assertions.
    fn transcript(items: &[SectionItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| {
                let indent = if item.group.is_some() { "  " } else { "" };
                match &item.kind {
                    SectionItemKind::Caption(label) => format!("# {label}"),
                    SectionItemKind::Link(link) => format!("{indent}link {}", link.id),
                    SectionItemKind::Skill(skill) => format!("{indent}skill {}", skill.id),
                    SectionItemKind::Group { label, .. } => format!("> {label}"),
                }
            })
            .collect()
    }

    #[test]
    fn section_demotes_grouped_links_and_setup_steps() {
        let manifest = manifest(
            vec![
                named_link("today", None),
                named_link("yesterday", Some("Older notes")),
                named_link("this-week", None),
                named_link("last-week", Some("Older notes")),
            ],
            vec![
                named_skill("wrap-today", SkillKind::Ritual),
                named_skill("connect-google", SkillKind::Setup),
                named_skill("week-review", SkillKind::Ritual),
                named_skill("onboarding", SkillKind::Setup),
            ],
        );
        // Groups start closed: the two demoted links and both setup steps
        // cost one row each, not four.
        assert_eq!(
            transcript(&section_items(&manifest, &HashSet::new())),
            vec![
                "# Notes",
                "link today",
                "link this-week",
                "> Older notes",
                "# Rituals",
                "skill wrap-today",
                "skill week-review",
                "> Setup",
            ]
        );
    }

    #[test]
    fn expanded_groups_nest_their_members_in_place() {
        let manifest = manifest(
            vec![
                named_link("today", None),
                named_link("yesterday", Some("Older notes")),
            ],
            vec![named_skill("onboarding", SkillKind::Setup)],
        );
        let expanded = HashSet::from([group_key("x", "Older notes")]);
        assert_eq!(
            transcript(&section_items(&manifest, &expanded)),
            vec![
                "# Notes",
                "link today",
                "> Older notes",
                "  link yesterday",
                "> Setup",
            ]
        );
    }

    #[test]
    fn a_section_with_nothing_to_separate_gets_no_captions() {
        let links_only = manifest(vec![named_link("today", None)], Vec::new());
        assert_eq!(
            transcript(&section_items(&links_only, &HashSet::new())),
            vec!["link today"]
        );
        let skills_only = manifest(
            Vec::new(),
            vec![named_skill("wrap-today", SkillKind::Ritual)],
        );
        assert_eq!(
            transcript(&section_items(&skills_only, &HashSet::new())),
            vec!["skill wrap-today"]
        );
    }

    /// Captions take no keyboard stop, so every row the cursor can land on
    /// must line up with a rendered, actionable item.
    #[test]
    fn every_selectable_row_is_an_action() {
        let manifest = manifest(
            vec![named_link("today", None), named_link("old", Some("Older"))],
            vec![
                named_skill("wrap", SkillKind::Ritual),
                named_skill("setup", SkillKind::Setup),
            ],
        );
        let items = section_items(&manifest, &HashSet::new());
        let selectable = items
            .iter()
            .filter(|item| !matches!(item.kind, SectionItemKind::Caption(_)))
            .count();
        assert_eq!(items.len(), selectable + 2, "expected two captions");
        assert_eq!(selectable, 4, "today, Older, wrap, Setup");
    }

    fn nav_rows_of(manifest: &RoutineManifest, expanded: &HashSet<String>) -> Vec<NavRow> {
        section_items(manifest, expanded)
            .into_iter()
            .filter_map(|item| {
                let kind = match item.kind {
                    SectionItemKind::Caption(_) => return None,
                    SectionItemKind::Link(link) => NavRowKind::Link(link),
                    SectionItemKind::Skill(skill) => NavRowKind::Skill(skill),
                    SectionItemKind::Group { label, .. } => NavRowKind::Group(label),
                };
                Some(NavRow {
                    routine_id: manifest.id.clone(),
                    group: item.group,
                    kind,
                })
            })
            .collect()
    }

    #[test]
    fn toggling_a_group_keeps_the_cursor_on_its_row() {
        let manifest = manifest(
            vec![
                named_link("today", None),
                named_link("yesterday", Some("Older notes")),
            ],
            vec![named_skill("wrap-today", SkillKind::Ritual)],
        );
        let group_row = nav_key("x", "group", "Older notes");
        let wrap = nav_key("x", "skill", "wrap-today");
        let yesterday = nav_key("x", "link", "yesterday");

        // Opening the group inserts a row above the ritual: the cursor moves
        // with the row, not with the index.
        let closed = nav_rows_of(&manifest, &HashSet::new());
        assert_eq!(reanchor_selection(&closed, &wrap, &group_row), Some(2));
        let expanded = nav_rows_of(&manifest, &HashSet::from([group_key("x", "Older notes")]));
        assert_eq!(reanchor_selection(&expanded, &wrap, &group_row), Some(3));

        // Closing it takes the cursor's row with it, so the cursor lands on
        // the group row rather than on whatever index survives.
        assert_eq!(
            reanchor_selection(&expanded, &yesterday, &group_row),
            Some(2)
        );
        assert_eq!(reanchor_selection(&closed, &yesterday, &group_row), Some(1));
    }

    #[test]
    fn manifest_icons_override_defaults_and_bad_names_fall_back() {
        assert_eq!(
            link_icon(&link("finance/plan.md", LinkKind::Editor, Some("hash"))),
            RowIcon::Named(IconName::Hash)
        );
        // Any snake_case IconName works, not just a curated list.
        assert_eq!(
            skill_icon(&skill(Some("sparkle"))),
            RowIcon::Named(IconName::Sparkle)
        );
        // Aliases for the names that have no one-for-one IconName.
        assert_eq!(
            manifest_icon("todo"),
            Some(RowIcon::Named(IconName::ListTodo))
        );
        assert_eq!(
            manifest_icon("html"),
            Some(RowIcon::Path("icons/file_icons/html.svg"))
        );
        // A typo must not blank the row out.
        assert_eq!(
            link_icon(&link("finance/plan.md", LinkKind::Editor, Some("hashh"))),
            RowIcon::Named(IconName::Notepad)
        );
        assert_eq!(
            skill_icon(&skill(Some("nope"))),
            RowIcon::Named(IconName::PlayOutlined)
        );
        assert_eq!(routine_icon(Some("nope")), RowIcon::Named(IconName::Blocks));
        assert_eq!(routine_icon(None), RowIcon::Named(IconName::Blocks));
    }
}
