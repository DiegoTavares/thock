//! The Thock Agent chat panel (V25 §3 items 3 to 7, refined by V26): a
//! right-dock panel that hosts the hosted agent (Pi over ACP) in a chat that
//! reads as a conversation, not a build log. Everything the agent did
//! between two messages collapses into one quiet activity line that expands
//! in place; paths are vault-relative; per-call failures never surface as
//! errors (the agent's prose carries them); and the allowance balance lives
//! in the footer. There are no per-change approval prompts (V25 decision
//! 16): every permission the harness asks for is granted, and safety is the
//! vault-scoped process plus the checkpoint taken before each session.

use acp_thread::{
    AcpThread, AcpThreadEvent, AgentConnection, AgentThreadEntry, AssistantMessageChunk,
    ElicitationEntryId, ElicitationStatus, SelectedPermissionOutcome, ThreadStatus, ToolCall,
    ToolCallStatus,
};
use agent_client_protocol::schema::v1 as acp;
use anyhow::Result;
use collections::{HashMap, HashSet};
use editor::{Editor, EditorMode, MinimapVisibility, SizingBehavior};
use gpui::{
    Action, AnyWindowHandle, App, AsyncWindowContext, Context, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, KeyContext, Pixels, ScrollHandle, SharedString, Subscription,
    WeakEntity, Window, actions, div, px, relative,
};
use language::language_settings::SoftWrap;
use markdown::{MarkdownElement, MarkdownFont, MarkdownStyle};
use project::Project;
use project::project_settings::DiagnosticSeverity;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use ui::prelude::*;
use ui::{
    Button, ButtonStyle, Divider, Icon, IconButton, Label, ProgressBar, SpinnerLabel, Tooltip,
};
use util::ResultExt as _;
use workspace::dock::{DockPosition, Panel, PanelEvent};
use workspace::{OpenOptions, OpenVisible, Workspace};

use crate::agent::{self, ConnectionMode, ModelTier};
use crate::agent_panel::{AgentPanel, ConnectionEpoch, LaunchRequest};
use crate::hosted_agent;
use crate::plus::{self, Entitlement, PlusError};
use crate::vault::{Vault, VaultStatus};

const CHAT_PANEL_KEY: &str = "ThockChatPanel";

actions!(
    thock,
    [
        /// Toggles focus on the Thock Agent chat panel.
        ToggleChatFocus,
        /// Starts a fresh chat with the Thock Agent.
        NewChat,
        /// Sends the message you typed to the Thock Agent.
        SendChatMessage,
        /// Stops what the Thock Agent is doing right now.
        StopChatTurn,
        /// Moves the keyboard focus into the chat message box.
        FocusChatInput,
        /// Opens the note the selected chat step touched.
        OpenChatEntry,
        /// Connects the Thock Agent with a Thock Plus invite code.
        ConnectThockAgent,
        /// Disconnects the Thock Agent and forgets its Thock Plus credential.
        DisconnectThockAgent,
        /// Runs skills with the Thock Agent instead of your own CLI agent.
        UseThockAgent,
        /// Runs skills with your own CLI agent in the terminal panel instead
        /// of the Thock Agent.
        UseOwnAgent,
        /// Shows the details of what the Thock Agent did in the selected
        /// step.
        ExpandChatActivity,
        /// Hides the details of the selected Thock Agent step.
        CollapseChatActivity,
        /// Sends your last message to the Thock Agent again after a turn
        /// that couldn't finish.
        RetryChatTurn,
        /// Shows or hides how much of the Thock Agent's allowance this
        /// cycle has used.
        ToggleChatUsage,
        /// Puts away the "Reflect now?" suggestion until the agent has noted
        /// more about you.
        DismissMemoryNudge,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleChatFocus, window, cx| {
            workspace.toggle_panel_focus::<ChatPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &NewChat, window, cx| {
            ChatPanel::launch_in_workspace(workspace, LaunchRequest::conversation(), window, cx);
        });
        workspace.register_action(|workspace, _: &ConnectThockAgent, window, cx| {
            if let Some(panel) = workspace.focus_panel::<ChatPanel>(window, cx) {
                panel.update(cx, |panel, cx| panel.open_connect(window, cx));
            }
        });
        workspace.register_action(|workspace, _: &DisconnectThockAgent, _window, cx| {
            if let Some(panel) = workspace.panel::<ChatPanel>(cx) {
                panel.update(cx, |panel, cx| panel.disconnect(cx));
            }
        });
        workspace.register_action(|workspace, _: &UseThockAgent, window, cx| {
            if let Some(panel) = workspace.focus_panel::<ChatPanel>(window, cx) {
                panel.update(cx, |panel, cx| panel.set_mode(ConnectionMode::Hosted, cx));
            }
        });
        workspace.register_action(|workspace, _: &UseOwnAgent, _window, cx| {
            if let Some(panel) = workspace.panel::<ChatPanel>(cx) {
                panel.update(cx, |panel, cx| panel.set_mode(ConnectionMode::Byo, cx));
            }
        });
    })
    .detach();
}

/// True when Run actions should go to the hosted chat instead of the
/// terminal panel: an explicit `[agent] mode`, or, when nothing was chosen,
/// whether a Thock Plus connection exists on this machine.
pub fn hosted_mode_active(workspace: &Workspace, cx: &App) -> bool {
    workspace
        .panel::<ChatPanel>(cx)
        .map(|panel| panel.read(cx).effective_mode() == ConnectionMode::Hosted)
        .unwrap_or(false)
}

/// Where the Thock Plus connection stands, as the panel last learned it.
enum PlusConnection {
    /// The keychain is still being read.
    Loading,
    Disconnected,
    Connected {
        credential: String,
        entitlement: Entitlement,
        /// The "running low" toast fires once per crossing, not per poll.
        warned_low: bool,
    },
    /// The backend turned this credential off; the message is its sentence.
    Revoked(SharedString),
}

struct ChatSession {
    title: SharedString,
    thread: Entity<AcpThread>,
    /// Kept alive for the session: dropping the last handle ends the agent
    /// process.
    _connection: Rc<dyn AgentConnection>,
    turns: u32,
    max_turns: u32,
    /// V26 §5.5: per-call failures are invisible, but a turn that could not
    /// finish gets one plain-language line, with a retry when we still hold
    /// the message that started it.
    turn_failure: Option<TurnFailure>,
    /// The last message sent, kept for the retry affordance.
    last_sent: Option<String>,
    _subscriptions: Vec<Subscription>,
}

struct TurnFailure {
    message: SharedString,
    retryable: bool,
}

/// One row of the transcript after V26 grouping (§5.1): entries render
/// one-to-one except consecutive tool calls, which collapse into a single
/// activity line. The grouping is derived on every read — the thread stays
/// the source of truth — and an activity is keyed by its first tool call id
/// so expansion and selection survive re-grouping while a turn streams.
enum ChatItem {
    Entry(usize),
    Activity {
        key: acp::ToolCallId,
        calls: Vec<(usize, acp::ToolCallId)>,
    },
}

impl ChatItem {
    fn key(&self) -> ItemKey {
        match self {
            ChatItem::Entry(index) => ItemKey::Entry(*index),
            ChatItem::Activity { key, .. } => ItemKey::Activity(key.clone()),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ItemKey {
    Entry(usize),
    Activity(acp::ToolCallId),
}

/// How one thread entry participates in the grouping.
enum EntryShape {
    Tool(acp::ToolCallId),
    /// Renders nothing (a thought-only assistant message, per decision 7);
    /// it neither takes a row nor splits a run of tool calls.
    Hidden,
    Visible,
}

fn entry_shape(entry: &AgentThreadEntry) -> EntryShape {
    match entry {
        AgentThreadEntry::ToolCall(tool_call) => EntryShape::Tool(tool_call.id.clone()),
        AgentThreadEntry::AssistantMessage(message)
            if message
                .chunks
                .iter()
                .all(|chunk| matches!(chunk, AssistantMessageChunk::Thought { .. })) =>
        {
            EntryShape::Hidden
        }
        _ => EntryShape::Visible,
    }
}

fn build_items(shapes: impl IntoIterator<Item = EntryShape>) -> Vec<ChatItem> {
    let mut items = Vec::new();
    let mut run: Vec<(usize, acp::ToolCallId)> = Vec::new();
    let flush = |items: &mut Vec<ChatItem>, run: &mut Vec<(usize, acp::ToolCallId)>| {
        if let Some((_, first_id)) = run.first() {
            items.push(ChatItem::Activity {
                key: first_id.clone(),
                calls: std::mem::take(run),
            });
        }
    };
    for (index, shape) in shapes.into_iter().enumerate() {
        match shape {
            EntryShape::Tool(id) => run.push((index, id)),
            EntryShape::Hidden => {}
            EntryShape::Visible => {
                flush(&mut items, &mut run);
                items.push(ChatItem::Entry(index));
            }
        }
    }
    flush(&mut items, &mut run);
    items
}

/// What the summary line needs from one tool call: its kind and the
/// vault-relative note it touched, if any.
struct SummaryCall {
    kind: acp::ToolKind,
    note: Option<String>,
}

/// Composes the activity line (V26 §5.2): deterministic, from the run's
/// tool kinds, with a small closed vocabulary written for a note-taker.
/// Writes win over looks, looks over everything else; `live` switches the
/// line to present tense while the run is still going.
fn summarize_activity(calls: &[SummaryCall], live: bool) -> String {
    let is_write = |kind: &acp::ToolKind| {
        matches!(
            kind,
            acp::ToolKind::Edit | acp::ToolKind::Delete | acp::ToolKind::Move
        )
    };
    let writes: Vec<&SummaryCall> = calls.iter().filter(|call| is_write(&call.kind)).collect();
    if !writes.is_empty() {
        let mut notes: Vec<&str> = writes
            .iter()
            .filter_map(|call| call.note.as_deref())
            .collect();
        notes.sort_unstable();
        notes.dedup();
        let all_deletes = writes
            .iter()
            .all(|call| matches!(call.kind, acp::ToolKind::Delete));
        return match notes.as_slice() {
            [note] if all_deletes => {
                if live {
                    format!("Removing {note}…")
                } else {
                    format!("Removed {note}")
                }
            }
            [note] => {
                if live {
                    format!("Updating {note}…")
                } else {
                    format!("Updated {note}")
                }
            }
            [] => {
                if live {
                    "Updating your notes…".to_string()
                } else {
                    "Updated your notes".to_string()
                }
            }
            notes => {
                if live {
                    "Updating your notes…".to_string()
                } else {
                    format!("Updated {} notes", notes.len())
                }
            }
        };
    }
    let looks: Vec<&SummaryCall> = calls
        .iter()
        .filter(|call| {
            matches!(
                call.kind,
                acp::ToolKind::Read | acp::ToolKind::Search | acp::ToolKind::Fetch
            )
        })
        .collect();
    if !looks.is_empty() {
        let mut notes: Vec<&str> = looks
            .iter()
            .filter_map(|call| call.note.as_deref())
            .collect();
        notes.sort_unstable();
        notes.dedup();
        let only_reads = looks
            .iter()
            .all(|call| matches!(call.kind, acp::ToolKind::Read));
        if let ([note], true) = (notes.as_slice(), only_reads) {
            return if live {
                format!("Looking at {note}…")
            } else {
                format!("Looked at {note}")
            };
        }
        return if live {
            "Looking through your notes…".to_string()
        } else {
            "Looked through your notes".to_string()
        };
    }
    if live {
        "Working behind the scenes…".to_string()
    } else {
        "Worked behind the scenes".to_string()
    }
}

/// V26 §5.3: every path shown is vault-relative. Outside-vault paths (which
/// the sandbox should prevent) fall back to the file name alone so an
/// absolute path never reaches the transcript.
fn vault_relative_label(path: &Path, vault_root: Option<&Path>) -> String {
    if let Some(relative) = vault_root.and_then(|root| path.strip_prefix(root).ok()) {
        return relative.display().to_string();
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "a file".to_string())
}

fn call_verb_and_icon(kind: &acp::ToolKind) -> (&'static str, IconName) {
    match kind {
        acp::ToolKind::Read => ("Read", IconName::ToolSearch),
        acp::ToolKind::Edit => ("Edited", IconName::ToolPencil),
        acp::ToolKind::Delete => ("Removed", IconName::ToolDeleteFile),
        acp::ToolKind::Move => ("Moved", IconName::ArrowRightLeft),
        acp::ToolKind::Search => ("Searched", IconName::ToolSearch),
        acp::ToolKind::Execute => ("Ran", IconName::ToolTerminal),
        acp::ToolKind::Think => ("Thought", IconName::ToolThink),
        acp::ToolKind::Fetch => ("Looked up", IconName::ToolWeb),
        acp::ToolKind::SwitchMode => ("Switched", IconName::ArrowRightLeft),
        _ => ("Worked", IconName::ToolHammer),
    }
}

/// The shapes of elicitation this panel can answer inline (V26 §5.7). Pi
/// asks single questions, so a one-property form covers the real traffic;
/// anything richer degrades to a plain accept/decline.
enum ElicitationShape {
    Choice {
        property: String,
        options: Vec<(String, String)>,
    },
    Text {
        property: String,
    },
    Link {
        url: String,
    },
    Confirm,
}

fn elicitation_shape(request: &acp::CreateElicitationRequest) -> ElicitationShape {
    match &request.mode {
        acp::ElicitationMode::Url(url_mode) => ElicitationShape::Link {
            url: url_mode.url.clone(),
        },
        acp::ElicitationMode::Form(form) => {
            let schema = &form.requested_schema;
            let mut properties = schema.properties.iter();
            match (properties.next(), properties.next()) {
                (Some((name, acp::ElicitationPropertySchema::String(string_schema))), None) => {
                    if let Some(one_of) = &string_schema.one_of {
                        ElicitationShape::Choice {
                            property: name.clone(),
                            options: one_of
                                .iter()
                                .map(|option| (option.value.clone(), option.title.clone()))
                                .collect(),
                        }
                    } else if let Some(values) = &string_schema.enum_values {
                        ElicitationShape::Choice {
                            property: name.clone(),
                            options: values
                                .iter()
                                .map(|value| (value.clone(), value.clone()))
                                .collect(),
                        }
                    } else {
                        ElicitationShape::Text {
                            property: name.clone(),
                        }
                    }
                }
                _ => ElicitationShape::Confirm,
            }
        }
        _ => ElicitationShape::Confirm,
    }
}

struct ConnectFlow {
    code_editor: Entity<Editor>,
    busy: bool,
    error: Option<SharedString>,
}

enum PanelView {
    Chat,
    Connect(ConnectFlow),
}

/// What `launch` decided from the connection state, resolved before any
/// mutation so the borrow of the state ends first.
enum LaunchPlan {
    Wait,
    Connect,
    Refuse(String),
    FallBack(String),
    Go { api_key: String, model: String },
}

pub struct ChatPanel {
    workspace: WeakEntity<Workspace>,
    /// The window the panel lives in, for launches that resume from an async
    /// completion (the keychain read, the connect flow) without a caller's
    /// window in hand.
    window_handle: AnyWindowHandle,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    position: DockPosition,
    vault_status: VaultStatus,
    connection: PlusConnection,
    /// The explicit `[agent] mode` choice, if any.
    mode: Option<ConnectionMode>,
    view: PanelView,
    session: Option<ChatSession>,
    /// What the launch is doing while there is no session to show yet.
    starting: Option<SharedString>,
    message_editor: Entity<Editor>,
    scroll_handle: ScrollHandle,
    /// The selected transcript row, keyed so it survives streaming
    /// re-grouping (G4): activities by tool call id, everything else by its
    /// entry index (entries only append while a turn streams).
    selected: Option<ItemKey>,
    /// The activity runs whose expansion is open, by run key.
    expanded_activities: HashSet<acp::ToolCallId>,
    /// The highlighted option per pending choice elicitation.
    elicitation_choices: HashMap<ElicitationEntryId, usize>,
    /// Single-line editors for pending free-text elicitations.
    elicitation_editors: HashMap<ElicitationEntryId, Entity<Editor>>,
    /// Read-only editors over the diffs tool calls produced, keyed by the
    /// diff entity so a re-render never rebuilds them.
    diff_editors: HashMap<EntityId, Entity<Editor>>,
    /// Whether the allowance bar is showing; off by default so the balance
    /// is a glance away, not a permanent fixture.
    show_usage: bool,
    /// A launch that is waiting on the connect flow or the keychain read.
    pending_launch: Option<LaunchRequest>,
    /// Whether enough sessions have started with unfiled notes in
    /// `memory/inbox.md` that the panel suggests running Reflect (V28
    /// decision 5). Recomputed off the UI thread at every launch.
    memory_nudge_due: bool,
    _subscriptions: Vec<Subscription>,
}

impl ChatPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            ChatPanel::new(workspace, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = workspace.weak_handle();
        cx.new(|cx| {
            let project_subscription = cx.subscribe(&project, |this: &mut Self, _, event, cx| {
                if matches!(
                    event,
                    project::Event::WorktreeAdded(_)
                        | project::Event::WorktreeRemoved(_)
                        | project::Event::WorktreeUpdatedEntries(..)
                ) {
                    this.refresh_vault_status(cx);
                }
            });
            let message_editor = cx.new(|cx| {
                let mut editor = Editor::auto_height(1, 8, window, cx);
                editor.set_placeholder_text("Ask the Thock Agent…", window, cx);
                editor.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
                editor
            });
            let mut this = Self {
                workspace: weak_workspace,
                window_handle: window.window_handle(),
                project,
                focus_handle: cx.focus_handle(),
                position: DockPosition::Right,
                vault_status: VaultStatus::NotAVault,
                connection: PlusConnection::Loading,
                mode: None,
                view: PanelView::Chat,
                session: None,
                starting: None,
                message_editor,
                scroll_handle: ScrollHandle::new(),
                selected: None,
                expanded_activities: HashSet::default(),
                elicitation_choices: HashMap::default(),
                elicitation_editors: HashMap::default(),
                diff_editors: HashMap::default(),
                show_usage: false,
                pending_launch: None,
                memory_nudge_due: false,
                _subscriptions: vec![project_subscription],
            };
            this.refresh_vault_status(cx);
            this.load_connection(cx);
            this
        })
    }

    /// Opens the panel and launches `request` with the hosted agent, routing
    /// through the connect flow first when Thock Plus isn't connected.
    pub fn launch_in_workspace(
        workspace: &mut Workspace,
        request: LaunchRequest,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let Some(panel) = workspace.panel::<ChatPanel>(cx) else {
            log::warn!("Thock: the chat panel isn't registered yet; launch dropped");
            return;
        };
        workspace.open_panel::<ChatPanel>(window, cx);
        panel.update(cx, |panel, cx| panel.launch(request, window, cx));
    }

    pub fn effective_mode(&self) -> ConnectionMode {
        match self.mode {
            Some(mode) => mode,
            None => match self.connection {
                PlusConnection::Connected { .. } => ConnectionMode::Hosted,
                _ => ConnectionMode::Byo,
            },
        }
    }

    fn vault(&self) -> Option<&Vault> {
        match &self.vault_status {
            VaultStatus::Valid(vault) => Some(vault),
            _ => None,
        }
    }

    fn entitlement(&self) -> Option<&Entitlement> {
        match &self.connection {
            PlusConnection::Connected { entitlement, .. } => Some(entitlement),
            _ => None,
        }
    }

    fn is_generating(&self, cx: &App) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.thread.read(cx).status() == ThreadStatus::Generating)
    }

    fn refresh_vault_status(&mut self, cx: &mut Context<Self>) {
        let root = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
        let status = match root {
            Some(root) => Vault::detect(&root),
            None => VaultStatus::NotAVault,
        };
        if status != self.vault_status {
            self.vault_status = status;
            cx.notify();
        }
    }

    fn show_error(&self, message: String, cx: &mut Context<Self>) {
        // Deferred: `launch` runs while the workspace is already being
        // updated, and a nested update would panic.
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.show_error(message, cx))
                .log_err();
        });
    }

    /// Reads the keychain and the mode setting, then the entitlement when a
    /// credential exists. Runs at construction and after every change.
    fn load_connection(&mut self, cx: &mut Context<Self>) {
        let mode = cx.background_spawn(async move { agent::load_global_connection_mode() });
        let http = cx.http_client();
        cx.spawn(async move |this, cx| {
            let mode = mode.await;
            this.update(cx, |this, _| this.mode = mode)?;
            let credential = match plus::read_credential(cx).await {
                Ok(credential) => credential,
                Err(error) => {
                    log::error!("Thock: couldn't read the Thock Plus credential: {error:#}");
                    None
                }
            };
            let Some(credential) = credential else {
                return this.update(cx, |this, cx| {
                    this.connection = PlusConnection::Disconnected;
                    this.continue_pending_launch(cx);
                    cx.notify();
                });
            };
            let base_url = cx
                .background_spawn(async move { plus::backend_url() })
                .await;
            let fetched = plus::fetch_entitlement(&http, &base_url, &credential).await;
            this.update(cx, |this, cx| {
                this.apply_entitlement_result(credential, fetched, cx);
                this.continue_pending_launch(cx);
            })
        })
        .detach_and_log_err(cx);
    }

    /// Folds a `GET /v1/entitlement` result into the connection state: a
    /// balance, a revocation (credential dropped, so the app falls back to
    /// the free path), or a transient failure that keeps what we had.
    fn apply_entitlement_result(
        &mut self,
        credential: String,
        fetched: Result<Entitlement>,
        cx: &mut Context<Self>,
    ) {
        match fetched {
            Ok(entitlement) => {
                let warned_low = match &self.connection {
                    PlusConnection::Connected { warned_low, .. } => *warned_low,
                    _ => false,
                };
                let crossed_low = entitlement.is_running_low() && !warned_low;
                if crossed_low {
                    self.show_error(
                        format!(
                            "The Thock Agent has used {}% of this cycle's allowance.",
                            entitlement.used_percent()
                        ),
                        cx,
                    );
                }
                self.connection = PlusConnection::Connected {
                    credential,
                    entitlement,
                    warned_low: warned_low || crossed_low,
                };
            }
            Err(error) => match error.downcast::<PlusError>() {
                Ok(error) if error.invalidates_credential() => {
                    self.connection = PlusConnection::Revoked(error.message().to_string().into());
                    cx.spawn(async move |_, cx| plus::delete_credential(cx).await)
                        .detach_and_log_err(cx);
                }
                Ok(error) => {
                    log::warn!("Thock Plus refused the entitlement read: {error}");
                    if !matches!(self.connection, PlusConnection::Connected { .. }) {
                        self.connection = PlusConnection::Disconnected;
                    }
                }
                Err(error) => {
                    log::warn!("Thock Plus couldn't be reached: {error:#}");
                    if !matches!(self.connection, PlusConnection::Connected { .. }) {
                        // Offline with a stored credential: keep the
                        // credential, show the empty state, retry on use.
                        self.connection = PlusConnection::Disconnected;
                    }
                }
            },
        }
        cx.notify();
    }

    /// Re-reads the balance; called after every turn so the footer keeps up
    /// with the backend's ledger.
    fn refresh_entitlement(&mut self, cx: &mut Context<Self>) {
        let PlusConnection::Connected { credential, .. } = &self.connection else {
            return;
        };
        let credential = credential.clone();
        let http = cx.http_client();
        cx.spawn(async move |this, cx| {
            let base_url = cx
                .background_spawn(async move { plus::backend_url() })
                .await;
            let fetched = plus::fetch_entitlement(&http, &base_url, &credential).await;
            this.update(cx, |this, cx| {
                this.apply_entitlement_result(credential, fetched, cx)
            })
        })
        .detach_and_log_err(cx);
    }

    fn continue_pending_launch(&mut self, cx: &mut Context<Self>) {
        if matches!(self.connection, PlusConnection::Loading) {
            return;
        }
        let Some(request) = self.pending_launch.take() else {
            return;
        };
        let this = cx.weak_entity();
        let window_handle = self.window_handle;
        cx.defer(move |cx| {
            window_handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.launch(request, window, cx))
                        .log_err();
                })
                .log_err();
        });
    }

    pub fn set_mode(&mut self, mode: ConnectionMode, cx: &mut Context<Self>) {
        self.mode = Some(mode);
        let save = cx.background_spawn(async move { agent::save_global_connection_mode(mode) });
        cx.spawn(async move |this, cx| {
            if let Err(error) = save.await {
                this.update(cx, |this, cx| {
                    this.show_error(format!("Couldn't save the agent choice: {error}"), cx)
                })?;
            }
            this.update(cx, |_, cx| {
                cx.update_global::<ConnectionEpoch, ()>(|epoch, _| epoch.0 += 1);
            })
        })
        .detach_and_log_err(cx);
        cx.notify();
    }

    pub fn launch(&mut self, request: LaunchRequest, window: &mut Window, cx: &mut Context<Self>) {
        if self.vault().is_none() {
            self.show_error(
                "Open a Thock vault to use the Thock Agent; it works inside your notes folder."
                    .to_string(),
                cx,
            );
            return;
        }
        let plan = match &self.connection {
            PlusConnection::Loading => LaunchPlan::Wait,
            PlusConnection::Disconnected => LaunchPlan::Connect,
            PlusConnection::Revoked(message) => LaunchPlan::FallBack(message.to_string()),
            PlusConnection::Connected { entitlement, .. } => {
                if entitlement.is_exhausted() {
                    LaunchPlan::Refuse(
                        "The Thock Agent has used all of this cycle's allowance. It comes back \
                         with the next cycle, or sooner with a top-up."
                            .to_string(),
                    )
                } else if let Some(gateway) = &entitlement.gateway {
                    LaunchPlan::Go {
                        api_key: gateway.api_key.clone(),
                        model: gateway.models.for_tier(request.tier).to_string(),
                    }
                } else {
                    LaunchPlan::Refuse(
                        "Thock Plus didn't hand out an agent key. Try reconnecting.".to_string(),
                    )
                }
            }
        };
        match plan {
            LaunchPlan::Wait => self.pending_launch = Some(request),
            LaunchPlan::Connect => {
                self.pending_launch = Some(request);
                self.open_connect(window, cx);
            }
            LaunchPlan::Refuse(message) => self.show_error(message, cx),
            LaunchPlan::FallBack(message) => {
                // The free path is always there (item 8: fall back cleanly).
                self.show_error(
                    format!("{message} Running this with your own agent instead."),
                    cx,
                );
                self.mode = Some(ConnectionMode::Byo);
                let workspace = self.workspace.clone();
                window.defer(cx, move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            AgentPanel::launch_in_workspace(workspace, request, window, cx)
                        })
                        .log_err();
                });
            }
            LaunchPlan::Go { api_key, model } => {
                // Pre-session checkpoint (V5 §6.5, V25 item 5): the undo point
                // every gate-free session relies on. Soft dependency; never
                // waits.
                crate::history::checkpoint_before_ai_write(&self.project, cx);
                self.note_session_for_memory_nudge(cx);
                self.start_session(request, api_key, model, cx);
            }
        }
    }

    /// Counts this launch toward the "Reflect now?" suggestion and refreshes
    /// whether it is due. The count lives in the vault's `.thock/state/` so
    /// it survives restarts; the read is blocking, so it runs off the UI
    /// thread.
    fn note_session_for_memory_nudge(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault() else {
            return;
        };
        let root = vault.root.clone();
        let nudge_after = vault.config.memory.nudge_after_sessions;
        cx.spawn(async move |this, cx| {
            let due = cx
                .background_spawn(
                    async move { crate::memory::note_session_started(&root, nudge_after) },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.memory_nudge_due != due {
                    this.memory_nudge_due = due;
                    cx.notify();
                }
            })
        })
        .detach_and_log_err(cx);
    }

    fn dismiss_memory_nudge(
        &mut self,
        _: &DismissMemoryNudge,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.memory_nudge_due = false;
        cx.notify();
        let Some(vault) = self.vault() else {
            return;
        };
        let root = vault.root.clone();
        cx.background_spawn(async move { crate::memory::dismiss_nudge(&root) })
            .detach_and_log_err(cx);
    }

    fn reflect_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.memory_nudge_due = false;
        self.launch(
            LaunchRequest::run_skill(
                "Reflect",
                crate::memory::REFLECT_SKILL_PATH,
                ModelTier::Fast,
            ),
            window,
            cx,
        );
    }

    /// One quiet line above the composer: the agent has noted things it
    /// hasn't filed, and Reflect is one click (or `thock::Reflect`) away.
    fn render_memory_nudge(&self, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .justify_between()
            .child(
                Label::new("Thock has a few things to file about you.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("thock-chat-reflect-now", "Reflect now")
                            .style(ButtonStyle::Filled)
                            .label_size(LabelSize::Small)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reflect_now(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("thock-chat-nudge-dismiss", "Not now")
                            .style(ButtonStyle::Subtle)
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_memory_nudge(&DismissMemoryNudge, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn start_session(
        &mut self,
        request: LaunchRequest,
        api_key: String,
        model: String,
        cx: &mut Context<Self>,
    ) {
        // Fresh process per action (V5 decision 3): the previous session's
        // handles drop here and its process ends with them.
        self.session = None;
        self.reset_transcript_state();
        self.starting = Some("Setting up the Thock Agent…".into());
        self.view = PanelView::Chat;
        cx.notify();

        let project = self.project.clone();
        let vault = self.vault().cloned();
        let window_handle = self.window_handle;
        let max_turns = self
            .entitlement()
            .map(|entitlement| entitlement.limits.max_turns_per_session)
            .unwrap_or(0);
        let tier = request.tier;
        cx.spawn(async move |this, cx| {
            let started = async {
                let command =
                    hosted_agent::prepare_launch(&project, vault, tier, &model, &api_key, cx)
                        .await?;
                this.update(cx, |this, cx| {
                    this.starting = Some("Starting the Thock Agent…".into());
                    cx.notify();
                })?;
                let connection = hosted_agent::connect(project.clone(), command, cx).await?;
                let work_dirs = project.read_with(cx, |project, cx| project.default_path_list(cx));
                let thread = cx
                    .update(|cx| {
                        connection
                            .clone()
                            .new_session(project.clone(), work_dirs, cx)
                    })
                    .await?;
                anyhow::Ok((connection, thread))
            }
            .await;
            window_handle.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.starting = None;
                    match started {
                        Ok((connection, thread)) => {
                            this.attach_session(request, connection, thread, max_turns, window, cx);
                        }
                        Err(error) => {
                            let message = match error.downcast_ref::<acp_thread::AuthRequired>() {
                                Some(_) => "The Thock Agent couldn't sign in to its model \
                                            gateway. Reconnect Thock Plus and try again."
                                    .to_string(),
                                None => format!("Couldn't start the Thock Agent: {error:#}"),
                            };
                            this.show_error(message, cx);
                            cx.notify();
                        }
                    }
                })
            })?
        })
        .detach_and_log_err(cx);
    }

    fn attach_session(
        &mut self,
        request: LaunchRequest,
        connection: Rc<dyn AgentConnection>,
        thread: Entity<AcpThread>,
        max_turns: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(&thread, window, Self::handle_thread_event);
        self.session = Some(ChatSession {
            title: request.title.clone().into(),
            thread,
            _connection: connection,
            turns: 0,
            max_turns,
            turn_failure: None,
            last_sent: None,
            _subscriptions: vec![subscription],
        });
        if let Some(kickoff) = request.kickoff {
            self.send_text(kickoff, cx);
        }
        window.focus(&self.message_editor.focus_handle(cx), cx);
        cx.notify();
    }

    /// Drops everything keyed to a session's transcript when it goes away.
    fn reset_transcript_state(&mut self) {
        self.diff_editors.clear();
        self.selected = None;
        self.expanded_activities.clear();
        self.elicitation_choices.clear();
        self.elicitation_editors.clear();
    }

    fn handle_thread_event(
        &mut self,
        thread: &Entity<AcpThread>,
        event: &AcpThreadEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AcpThreadEvent::ToolAuthorizationRequested(id) => {
                self.authorize_tool_call(thread, id, cx);
            }
            AcpThreadEvent::NewEntry | AcpThreadEvent::EntryUpdated(_) => {
                self.sync_diff_editors(thread, window, cx);
                if thread.read(cx).status() == ThreadStatus::Generating {
                    self.scroll_handle.scroll_to_bottom();
                }
            }
            AcpThreadEvent::ElicitationRequested(id) => {
                self.prepare_elicitation(thread, id, window, cx);
            }
            AcpThreadEvent::Stopped(_) => {
                self.refresh_entitlement(cx);
            }
            // Decision 8 has a floor (§5.5): failures never get error
            // styling or raw detail, but a turn that could not finish says
            // so in plain language. The mechanics go to the log.
            AcpThreadEvent::Error => {
                if let Some(session) = &mut self.session {
                    session.turn_failure = Some(TurnFailure {
                        message: "The Thock Agent couldn't finish that.".into(),
                        retryable: true,
                    });
                }
            }
            AcpThreadEvent::LoadError(error) => {
                log::error!("Thock: the agent session failed to load: {error}");
                if let Some(session) = &mut self.session {
                    session.turn_failure = Some(TurnFailure {
                        message: "The Thock Agent couldn't finish that.".into(),
                        retryable: true,
                    });
                }
            }
            AcpThreadEvent::Refusal => {
                if let Some(session) = &mut self.session {
                    session.turn_failure = Some(TurnFailure {
                        message: "The Thock Agent decided not to do that.".into(),
                        retryable: false,
                    });
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// A question from the agent blocks the turn, so it arrives selected and
    /// ready to answer from the keyboard (V26 §5.7): free-text questions get
    /// a focused editor, everything else focuses the list so `left`/`right`
    /// move the highlighted option and `enter` answers.
    fn prepare_elicitation(
        &mut self,
        thread: &Entity<AcpThread>,
        id: &ElicitationEntryId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((entry_index, elicitation)) = thread.read(cx).elicitation(id) else {
            return;
        };
        let shape = elicitation_shape(&elicitation.request);
        self.selected = Some(ItemKey::Entry(entry_index));
        match shape {
            ElicitationShape::Choice { .. } => {
                self.elicitation_choices.insert(id.clone(), 0);
                window.focus(&self.focus_handle, cx);
            }
            ElicitationShape::Text { .. } => {
                let editor = cx.new(|cx| {
                    let mut editor = Editor::single_line(window, cx);
                    editor.set_placeholder_text("Type your answer…", window, cx);
                    editor
                });
                window.focus(&editor.focus_handle(cx), cx);
                self.elicitation_editors.insert(id.clone(), editor);
            }
            ElicitationShape::Link { .. } | ElicitationShape::Confirm => {
                window.focus(&self.focus_handle, cx);
            }
        }
        self.scroll_handle.scroll_to_bottom();
    }

    /// Decision 16: no approval prompts. Whatever the harness asks, the
    /// broadest "allow" it offers is the answer.
    fn authorize_tool_call(
        &mut self,
        thread: &Entity<AcpThread>,
        id: &acp::ToolCallId,
        cx: &mut Context<Self>,
    ) {
        let outcome = thread.read(cx).tool_call(id).and_then(|(_, tool_call)| {
            let ToolCallStatus::WaitingForConfirmation { options, .. } = &tool_call.status else {
                return None;
            };
            options
                .first_option_of_kind(acp::PermissionOptionKind::AllowAlways)
                .or_else(|| options.first_option_of_kind(acp::PermissionOptionKind::AllowOnce))
                .map(|option| SelectedPermissionOutcome::new(option.option_id.clone(), option.kind))
        });
        match outcome {
            Some(outcome) => thread.update(cx, |thread, cx| {
                thread.authorize_tool_call(id.clone(), outcome, cx);
            }),
            None => log::warn!("Thock: a tool call asked for permission without an allow option"),
        }
    }

    fn sync_diff_editors(
        &mut self,
        thread: &Entity<AcpThread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let diffs = thread
            .read(cx)
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                AgentThreadEntry::ToolCall(tool_call) => Some(tool_call),
                _ => None,
            })
            .flat_map(|tool_call| tool_call.diffs().cloned())
            .filter(|diff| !self.diff_editors.contains_key(&diff.entity_id()))
            .collect::<Vec<_>>();
        for diff in diffs {
            let editor = cx.new(|cx| {
                let mut editor = Editor::new(
                    EditorMode::Full {
                        scale_ui_elements_with_buffer_font_size: false,
                        show_active_line_background: false,
                        sizing_behavior: SizingBehavior::SizeByContent,
                    },
                    diff.read(cx).multibuffer().clone(),
                    None,
                    window,
                    cx,
                );
                editor.set_show_gutter(false, cx);
                editor.disable_diagnostics(cx);
                editor.set_max_diagnostics_severity(DiagnosticSeverity::Off, cx);
                editor.disable_expand_excerpt_buttons(cx);
                editor.set_show_vertical_scrollbar(false, cx);
                editor.set_minimap_visibility(MinimapVisibility::Disabled, window, cx);
                editor.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
                editor.set_forbid_vertical_scroll(true);
                editor.set_show_indent_guides(false, cx);
                editor.set_read_only(true);
                editor.set_show_breakpoints(false, cx);
                editor.set_show_code_actions(false, cx);
                editor.set_show_git_diff_gutter(false, cx);
                editor.set_expand_all_diff_hunks(cx);
                editor
            });
            self.diff_editors.insert(diff.entity_id(), editor);
        }
    }

    fn send_text(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self
            .entitlement()
            .is_some_and(|entitlement| entitlement.is_exhausted())
        {
            self.show_error(
                "The Thock Agent has used all of this cycle's allowance.".to_string(),
                cx,
            );
            return;
        }
        let (over_limit, generating) = match &self.session {
            Some(session) => (
                session.max_turns > 0 && session.turns >= session.max_turns,
                session.thread.read(cx).status() == ThreadStatus::Generating,
            ),
            None => return,
        };
        if over_limit {
            self.show_error(
                "This chat has reached its length limit. Start a new chat to keep going."
                    .to_string(),
                cx,
            );
            return;
        }
        if generating {
            self.show_error(
                "The Thock Agent is still working. Wait for it, or stop it first.".to_string(),
                cx,
            );
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        session.turns += 1;
        session.turn_failure = None;
        session.last_sent = Some(text.clone());
        let turn = session
            .thread
            .update(cx, |thread, cx| thread.send(vec![text.as_str().into()], cx));
        self.scroll_handle.scroll_to_bottom();
        cx.spawn(async move |this, cx| {
            let result = turn.await;
            this.update(cx, |this, cx| {
                if let Err(error) = result
                    && let Some(session) = &mut this.session
                {
                    log::error!("Thock: the agent turn failed: {error:#}");
                    session.turn_failure = Some(TurnFailure {
                        message: "The Thock Agent couldn't finish that.".into(),
                        retryable: true,
                    });
                }
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
        cx.notify();
    }

    fn toggle_usage(&mut self, _: &ToggleChatUsage, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_usage = !self.show_usage;
        cx.notify();
    }

    fn retry_turn(&mut self, _: &RetryChatTurn, _window: &mut Window, cx: &mut Context<Self>) {
        let last_sent = self
            .session
            .as_ref()
            .filter(|session| session.turn_failure.as_ref().is_some_and(|f| f.retryable))
            .and_then(|session| session.last_sent.clone());
        if let Some(text) = last_sent {
            self.send_text(text, cx);
        }
    }

    fn send_message(&mut self, _: &SendChatMessage, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.message_editor.read(cx).text(cx);
        if text.trim().is_empty() {
            return;
        }
        self.message_editor
            .update(cx, |editor, cx| editor.clear(window, cx));
        if self.session.is_some() {
            self.send_text(text, cx);
        } else {
            // The first message of an ad-hoc chat is its kickoff.
            self.launch(
                LaunchRequest {
                    title: "Chat".to_string(),
                    kickoff: Some(text),
                    tier: ModelTier::Default,
                },
                window,
                cx,
            );
        }
    }

    fn stop_turn(&mut self, _: &StopChatTurn, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            session
                .thread
                .update(cx, |thread, cx| thread.cancel(cx))
                .detach();
        }
    }

    fn new_chat(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        self.launch(LaunchRequest::conversation(), window, cx);
    }

    fn focus_input(&mut self, _: &FocusChatInput, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.message_editor.focus_handle(cx), cx);
    }

    /// The transcript as the user sees it (V26 §5.1), derived fresh from
    /// the thread on every use so it can never desynchronize.
    fn items(&self, cx: &App) -> Vec<ChatItem> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        build_items(session.thread.read(cx).entries().iter().map(entry_shape))
    }

    /// Where the selection sits in `items`. An activity matches when it
    /// *contains* the selected call, not only when it starts with it, so a
    /// run that merges or splits mid-turn keeps the selection (R4).
    fn selected_position(&self, items: &[ChatItem]) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        items.iter().position(|item| match (item, selected) {
            (ChatItem::Entry(index), ItemKey::Entry(selected_index)) => index == selected_index,
            (ChatItem::Activity { calls, .. }, ItemKey::Activity(id)) => {
                calls.iter().any(|(_, call_id)| call_id == id)
            }
            _ => false,
        })
    }

    fn select_position(&mut self, items: &[ChatItem], position: usize, cx: &mut Context<Self>) {
        let Some(item) = items.get(position) else {
            return;
        };
        self.selected = Some(item.key());
        self.scroll_handle.scroll_to_item(position);
        cx.notify();
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let items = self.items(cx);
        if items.is_empty() {
            return;
        }
        let next = match self.selected_position(&items) {
            Some(position) => (position + 1).min(items.len() - 1),
            None => 0,
        };
        self.select_position(&items, next, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = self.items(cx);
        if items.is_empty() {
            return;
        }
        let previous = match self.selected_position(&items) {
            Some(position) => position.saturating_sub(1),
            None => items.len() - 1,
        };
        self.select_position(&items, previous, cx);
    }

    fn select_first(&mut self, _: &menu::SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        let items = self.items(cx);
        if !items.is_empty() {
            self.select_position(&items, 0, cx);
        }
    }

    fn select_last(&mut self, _: &menu::SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        let items = self.items(cx);
        if !items.is_empty() {
            self.select_position(&items, items.len() - 1, cx);
        }
    }

    /// The pending elicitation under the selection, if that is what is
    /// selected.
    fn selected_pending_elicitation(&self, cx: &App) -> Option<ElicitationEntryId> {
        let Some(ItemKey::Entry(index)) = &self.selected else {
            return None;
        };
        let session = self.session.as_ref()?;
        let thread = session.thread.read(cx);
        let AgentThreadEntry::Elicitation(id) = thread.entries().get(*index)? else {
            return None;
        };
        let (_, elicitation) = thread.elicitation(id)?;
        matches!(elicitation.status, ElicitationStatus::Pending { .. }).then(|| id.clone())
    }

    /// `right`/`l`: open the selected activity's detail, or move to the next
    /// option of a pending question.
    fn expand_activity(&mut self, _: &ExpandChatActivity, _: &mut Window, cx: &mut Context<Self>) {
        self.expand_or_cycle(1, cx);
    }

    /// `left`/`h`: close the selected activity's detail, or move to the
    /// previous option of a pending question.
    fn collapse_activity(
        &mut self,
        _: &CollapseChatActivity,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expand_or_cycle(-1, cx);
    }

    fn expand_or_cycle(&mut self, direction: i32, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_pending_elicitation(cx) {
            self.cycle_elicitation_choice(&id, direction, cx);
            return;
        }
        let Some(ItemKey::Activity(selected_id)) = self.selected.clone() else {
            return;
        };
        // Normalize to the run's key: the selected id may be a mid-run call
        // after runs merged.
        let items = self.items(cx);
        let key = items.iter().find_map(|item| match item {
            ChatItem::Activity { key, calls }
                if calls.iter().any(|(_, call_id)| *call_id == selected_id) =>
            {
                Some(key.clone())
            }
            _ => None,
        });
        let Some(key) = key else {
            return;
        };
        if direction > 0 {
            self.expanded_activities.insert(key);
        } else {
            self.expanded_activities.remove(&key);
        }
        cx.notify();
    }

    fn cycle_elicitation_choice(
        &mut self,
        id: &ElicitationEntryId,
        direction: i32,
        cx: &mut Context<Self>,
    ) {
        let option_count = self
            .session
            .as_ref()
            .and_then(|session| {
                let thread = session.thread.read(cx);
                let (_, elicitation) = thread.elicitation(id)?;
                match elicitation_shape(&elicitation.request) {
                    ElicitationShape::Choice { options, .. } => Some(options.len()),
                    _ => None,
                }
            })
            .unwrap_or(0);
        if option_count == 0 {
            return;
        }
        let current = self.elicitation_choices.get(id).copied().unwrap_or(0);
        let next = if direction > 0 {
            (current + 1).min(option_count - 1)
        } else {
            current.saturating_sub(1)
        };
        self.elicitation_choices.insert(id.clone(), next);
        cx.notify();
    }

    /// Answers a pending question through the thread so the turn continues.
    /// `choice` overrides the keyboard-highlighted option when the user
    /// clicked one directly.
    fn answer_elicitation(
        &mut self,
        id: &ElicitationEntryId,
        choice: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.session else {
            return;
        };
        let thread = session.thread.clone();
        let Some((_, elicitation)) = thread.read(cx).elicitation(id) else {
            return;
        };
        if !matches!(elicitation.status, ElicitationStatus::Pending { .. }) {
            return;
        }
        let response = match elicitation_shape(&elicitation.request) {
            ElicitationShape::Choice { property, options } => {
                let index = choice
                    .or_else(|| self.elicitation_choices.get(id).copied())
                    .unwrap_or(0);
                let Some((value, _)) = options.get(index) else {
                    return;
                };
                let content = BTreeMap::from([(
                    property,
                    acp::ElicitationContentValue::String(value.clone()),
                )]);
                acp::CreateElicitationResponse::new(acp::ElicitationAction::Accept(
                    acp::ElicitationAcceptAction::new().content(content),
                ))
            }
            ElicitationShape::Text { property } => {
                let Some(editor) = self.elicitation_editors.get(id) else {
                    return;
                };
                let text = editor.read(cx).text(cx);
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                let content = BTreeMap::from([(
                    property,
                    acp::ElicitationContentValue::String(text.to_string()),
                )]);
                acp::CreateElicitationResponse::new(acp::ElicitationAction::Accept(
                    acp::ElicitationAcceptAction::new().content(content),
                ))
            }
            ElicitationShape::Link { url } => {
                cx.open_url(&url);
                return;
            }
            ElicitationShape::Confirm => acp::CreateElicitationResponse::new(
                acp::ElicitationAction::Accept(acp::ElicitationAcceptAction::new()),
            ),
        };
        thread.update(cx, |thread, cx| {
            thread.respond_to_elicitation(id, response, cx)
        });
        cx.notify();
    }

    fn decline_elicitation(&mut self, id: &ElicitationEntryId, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        session.thread.update(cx, |thread, cx| {
            thread.respond_to_elicitation(
                id,
                acp::CreateElicitationResponse::new(acp::ElicitationAction::Decline),
                cx,
            )
        });
        cx.notify();
    }

    /// `enter`: connect in the connect flow, send from the message box,
    /// answer a pending question, or open the note the selected activity
    /// names (falling back to the message box when nothing is openable).
    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        match &self.view {
            PanelView::Connect(flow) => {
                let code = flow.code_editor.read(cx).text(cx);
                self.submit_invite_code(code, window, cx);
            }
            PanelView::Chat => {
                // A pending answer box takes `enter` even though it is an
                // editor, so answering never requires the mouse.
                if let Some(id) = self.pending_elicitation_for_confirm(window, cx) {
                    self.answer_elicitation(&id, None, cx);
                } else if self.message_editor.focus_handle(cx).is_focused(window) {
                    self.send_message(&SendChatMessage, window, cx);
                } else if !self.open_selected_entry(window, cx) {
                    window.focus(&self.message_editor.focus_handle(cx), cx);
                }
            }
        }
    }

    /// The elicitation `enter` should answer: the focused answer box, or a
    /// selected pending question.
    fn pending_elicitation_for_confirm(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<ElicitationEntryId> {
        for (id, editor) in self.elicitation_editors.iter() {
            if editor.focus_handle(cx).is_focused(window) {
                return Some(id.clone());
            }
        }
        if self.message_editor.focus_handle(cx).is_focused(window) {
            return None;
        }
        self.selected_pending_elicitation(cx)
    }

    /// `escape`: back out of the connect flow, stop a running turn, or move
    /// focus from the message box to the list so the arrow keys work.
    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        match &self.view {
            PanelView::Connect(_) => self.cancel_connect(cx),
            PanelView::Chat => {
                if self.is_generating(cx) {
                    self.stop_turn(&StopChatTurn, window, cx);
                } else if self.message_editor.focus_handle(cx).is_focused(window) {
                    window.focus(&self.focus_handle, cx);
                } else {
                    cx.propagate();
                }
            }
        }
    }

    fn open_entry(&mut self, _: &OpenChatEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.open_selected_entry(window, cx);
    }

    /// Opens the note the selected activity names: the first note a write
    /// touched, or failing that the first note the run touched at all.
    /// Returns whether there was one.
    fn open_selected_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(ItemKey::Activity(selected_id)) = self.selected.clone() else {
            return false;
        };
        let Some(session) = &self.session else {
            return false;
        };
        let items = self.items(cx);
        let calls = items.iter().find_map(|item| match item {
            ChatItem::Activity { calls, .. }
                if calls.iter().any(|(_, call_id)| *call_id == selected_id) =>
            {
                Some(calls.clone())
            }
            _ => None,
        });
        let Some(calls) = calls else {
            return false;
        };
        let entries = session.thread.read(cx).entries();
        let tool_calls = calls
            .iter()
            .filter_map(|(index, _)| match entries.get(*index) {
                Some(AgentThreadEntry::ToolCall(tool_call)) => Some(tool_call),
                _ => None,
            });
        let mut first_path = None;
        let mut write_path = None;
        for tool_call in tool_calls {
            let Some(location) = tool_call.locations.first() else {
                continue;
            };
            if first_path.is_none() {
                first_path = Some(location.path.clone());
            }
            if matches!(
                tool_call.kind,
                acp::ToolKind::Edit | acp::ToolKind::Delete | acp::ToolKind::Move
            ) {
                write_path = Some(location.path.clone());
                break;
            }
        }
        let Some(path) = write_path.or(first_path) else {
            return false;
        };
        self.open_note(path, window, cx);
        true
    }

    fn open_note(&self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace
                        .open_abs_path(
                            path,
                            OpenOptions {
                                visible: Some(OpenVisible::All),
                                ..Default::default()
                            },
                            window,
                            cx,
                        )
                        .detach_and_log_err(cx);
                })
                .log_err();
        });
    }

    pub fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Invite code, like THOCK-7K3M-P9QX", window, cx);
            editor
        });
        self.view = PanelView::Connect(ConnectFlow {
            code_editor: code_editor.clone(),
            busy: false,
            error: None,
        });
        window.focus(&code_editor.focus_handle(cx), cx);
        cx.notify();
    }

    fn cancel_connect(&mut self, cx: &mut Context<Self>) {
        self.view = PanelView::Chat;
        self.pending_launch = None;
        cx.notify();
    }

    fn submit_invite_code(&mut self, code: String, _window: &mut Window, cx: &mut Context<Self>) {
        let code = code.trim().to_uppercase();
        let PanelView::Connect(flow) = &mut self.view else {
            return;
        };
        if flow.busy {
            return;
        }
        if code.is_empty() {
            flow.error = Some("Enter the invite code you were given.".into());
            cx.notify();
            return;
        }
        flow.busy = true;
        flow.error = None;
        cx.notify();
        let http = cx.http_client();
        cx.spawn(async move |this, cx| {
            let base_url = cx
                .background_spawn(async move { plus::backend_url() })
                .await;
            let connected = plus::connect(&http, &base_url, &code, &plus::device_label()).await;
            let connected = match connected {
                Ok(connected) => connected,
                Err(error) => {
                    let message = match error.downcast_ref::<PlusError>() {
                        Some(error) => error.message().to_string(),
                        None => format!("Couldn't reach Thock Plus: {error:#}"),
                    };
                    return this.update(cx, |this, cx| {
                        if let PanelView::Connect(flow) = &mut this.view {
                            flow.busy = false;
                            flow.error = Some(message.into());
                        }
                        cx.notify();
                    });
                }
            };
            if let Err(error) = plus::write_credential(&connected.credential, cx).await {
                log::error!("Thock: couldn't store the Thock Plus credential: {error:#}");
                this.update(cx, |this, cx| {
                    this.show_error(
                        "Connected, but the credential couldn't be saved to your keychain; you \
                         will need to connect again next time."
                            .to_string(),
                        cx,
                    )
                })?;
            }
            this.update(cx, |this, cx| {
                this.connection = PlusConnection::Connected {
                    credential: connected.credential,
                    entitlement: connected.entitlement,
                    warned_low: false,
                };
                this.view = PanelView::Chat;
                this.set_mode(ConnectionMode::Hosted, cx);
                this.continue_pending_launch(cx);
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
    }

    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        let credential = match &self.connection {
            PlusConnection::Connected { credential, .. } => Some(credential.clone()),
            _ => None,
        };
        self.session = None;
        self.reset_transcript_state();
        self.connection = PlusConnection::Disconnected;
        cx.notify();
        let http = cx.http_client();
        cx.spawn(async move |this, cx| {
            if let Some(credential) = credential {
                let base_url = cx
                    .background_spawn(async move { plus::backend_url() })
                    .await;
                if let Err(error) = plus::disconnect(&http, &base_url, &credential).await {
                    this.update(cx, |this, cx| {
                        this.show_error(
                            format!(
                                "Disconnected here, but Thock Plus couldn't be told: {error:#}"
                            ),
                            cx,
                        )
                    })?;
                }
            }
            plus::delete_credential(cx).await?;
            this.update(cx, |_, cx| {
                cx.update_global::<ConnectionEpoch, ()>(|epoch, _| epoch.0 += 1);
            })
        })
        .detach_and_log_err(cx);
    }

    // --- rendering ---

    fn markdown_style(&self, muted: bool, window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        // Pin the prose to the same rem scale panel Labels use
        // (`TextSize::Default`, 14px at the default rem base) — the agent
        // style's `ui_font_size` is the 16px rem base itself, which reads
        // oversized beside the rest of the panel chrome. Code steps down to
        // the Small label size so commands never out-shout the prose.
        style.base_text_style.font_size = rems_from_px(14.).into();
        style.base_text_style.line_height = relative(1.55);
        style.inline_code.font_size = Some(rems_from_px(12.).into());
        style.code_block.text.font_size = Some(rems_from_px(12.).into());
        if muted {
            style.with_muted_text(cx)
        } else {
            style
        }
    }

    fn render_item(
        &self,
        position: usize,
        item: &ChatItem,
        entries: &[AgentThreadEntry],
        selected: bool,
        vault_root: Option<&Path>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let body: AnyElement = match item {
            ChatItem::Entry(index) => match entries.get(*index) {
                Some(AgentThreadEntry::UserMessage(message)) => {
                    self.render_user_message(message, window, cx)
                }
                Some(AgentThreadEntry::AssistantMessage(message)) => {
                    self.render_assistant_message(message, window, cx)
                }
                Some(AgentThreadEntry::Elicitation(id)) => self.render_elicitation(id, cx),
                Some(AgentThreadEntry::CompletedPlan(plan_entries)) => Label::new(format!(
                    "Finished a plan of {} step{}.",
                    plan_entries.len(),
                    if plan_entries.len() == 1 { "" } else { "s" }
                ))
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
                Some(AgentThreadEntry::ContextCompaction(_)) => {
                    Label::new("Tidied up the conversation to keep going.")
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .into_any_element()
                }
                // Tool calls always arrive grouped; a stale index renders
                // nothing rather than panicking.
                Some(AgentThreadEntry::ToolCall(_)) | None => div().into_any_element(),
            },
            ChatItem::Activity { key, calls } => {
                self.render_activity(key, calls, entries, vault_root, cx)
            }
        };
        let item_key = item.key();
        div()
            .id(("thock-chat-item", position))
            .w_full()
            .px_2()
            .py_1()
            .rounded_sm()
            .when(selected, |this| {
                this.bg(cx.theme().colors().element_selected)
                    .border_l_1()
                    .border_color(cx.theme().colors().border_focused)
            })
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.selected = Some(item_key.clone());
                cx.notify();
            }))
            .child(body)
            .into_any_element()
    }

    /// G6: the user's message sits in a container the eye can find.
    /// `element_background` was within noise of the panel in the default
    /// dark theme, so the bubble uses the selection surface instead.
    fn render_user_message(
        &self,
        message: &acp_thread::UserMessage,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let content: AnyElement = match message.content.markdown() {
            Some(markdown) => {
                MarkdownElement::new(markdown.clone(), self.markdown_style(false, window, cx))
                    .into_any_element()
            }
            None => Label::new(
                message
                    .content
                    .text_content(cx)
                    .unwrap_or_default()
                    .to_string(),
            )
            .into_any_element(),
        };
        h_flex()
            .w_full()
            .justify_end()
            .child(
                div()
                    .max_w(relative(0.85))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .rounded_br_sm()
                    .bg(cx.theme().colors().element_selected)
                    .child(content),
            )
            .into_any_element()
    }

    /// G5: the agent's prose is the largest thing on screen. Thought chunks
    /// leave no residue (decision 7).
    fn render_assistant_message(
        &self,
        message: &acp_thread::AssistantMessage,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut column = v_flex().w_full().gap_1();
        for chunk in &message.chunks {
            if let AssistantMessageChunk::Message { block, .. } = chunk
                && let Some(markdown) = block.markdown()
            {
                column = column.child(MarkdownElement::new(
                    markdown.clone(),
                    self.markdown_style(false, window, cx),
                ));
            }
        }
        column.into_any_element()
    }

    /// One quiet line for everything the agent did between two messages
    /// (decision 4), expanding in place to per-call rows and diffs (§5.4).
    /// While the run is still going the line reads in present tense, so a
    /// long turn is never silent (R5).
    fn render_activity(
        &self,
        key: &acp::ToolCallId,
        calls: &[(usize, acp::ToolCallId)],
        entries: &[AgentThreadEntry],
        vault_root: Option<&Path>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let tool_calls: Vec<&ToolCall> = calls
            .iter()
            .filter_map(|(index, _)| match entries.get(*index) {
                Some(AgentThreadEntry::ToolCall(tool_call)) => Some(tool_call),
                _ => None,
            })
            .collect();
        let summary_calls: Vec<SummaryCall> = tool_calls
            .iter()
            .map(|tool_call| SummaryCall {
                kind: tool_call.kind,
                note: tool_call
                    .locations
                    .first()
                    .map(|location| vault_relative_label(&location.path, vault_root)),
            })
            .collect();
        let live = tool_calls.iter().any(|tool_call| {
            matches!(
                tool_call.status,
                ToolCallStatus::Pending
                    | ToolCallStatus::InProgress
                    | ToolCallStatus::WaitingForConfirmation { .. }
            )
        });
        let summary = summarize_activity(&summary_calls, live);
        let expanded = self.expanded_activities.contains(key);
        let toggle_key = key.clone();
        let pill = h_flex()
            .id(SharedString::from(format!("thock-chat-activity-{key}")))
            .gap_1()
            .px_2()
            .py_0p5()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .rounded_full()
            .bg(cx.theme().colors().elevated_surface_background)
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(
                Label::new(summary)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.selected = Some(ItemKey::Activity(toggle_key.clone()));
                if !this.expanded_activities.remove(&toggle_key) {
                    this.expanded_activities.insert(toggle_key.clone());
                }
                cx.notify();
            }));
        let mut column = v_flex().w_full().gap_1().child(h_flex().child(pill));
        if expanded {
            let mut detail = v_flex()
                .w_full()
                .ml_2()
                .pl_2()
                .border_l_1()
                .border_color(cx.theme().colors().border_variant)
                .gap_0p5();
            for tool_call in &tool_calls {
                detail = detail.child(self.render_activity_call(tool_call, vault_root, cx));
            }
            column = column.child(detail);
        }
        column.into_any_element()
    }

    /// One row of the expansion: kind icon, verb, vault-relative path (with
    /// the diff editor beneath edits). Failed calls render as ordinary rows
    /// — no error text, no error styling (decision 8); a command shows with
    /// no output (decision 11).
    fn render_activity_call(
        &self,
        tool_call: &ToolCall,
        vault_root: Option<&Path>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (verb, icon) = call_verb_and_icon(&tool_call.kind);
        let location = tool_call.locations.first().map(|location| {
            (
                location.path.clone(),
                vault_relative_label(&location.path, vault_root),
            )
        });
        let detail: Option<AnyElement> = match &location {
            Some((_, label)) => Some(
                Label::new(label.clone())
                    .size(LabelSize::Small)
                    .into_any_element(),
            ),
            None => {
                let label_text = tool_call.label.read(cx).source().clone();
                let matches_tool_name = tool_call
                    .tool_name
                    .as_ref()
                    .is_some_and(|name| name.as_ref() == label_text.as_ref());
                if !label_text.is_empty() && !matches_tool_name {
                    // Plain text at row size — a command must not out-shout
                    // the verbs beside it.
                    let label = Label::new(label_text)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate();
                    let label = if matches!(tool_call.kind, acp::ToolKind::Execute) {
                        label.buffer_font(cx)
                    } else {
                        label
                    };
                    Some(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(label)
                            .into_any_element(),
                    )
                } else {
                    None
                }
            }
        };
        let open_path = location.as_ref().map(|(path, _)| path.clone());
        let mut column = v_flex().w_full().gap_1().child(
            h_flex()
                .id(SharedString::from(format!(
                    "thock-chat-call-{}",
                    tool_call.id
                )))
                .w_full()
                .gap_1()
                .items_center()
                .child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
                .child(Label::new(verb).size(LabelSize::Small).color(Color::Muted))
                .children(detail)
                .when_some(open_path, |this, path| {
                    this.cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_note(path.clone(), window, cx);
                        }))
                }),
        );
        for diff in tool_call.diffs() {
            if let Some(editor) = self.diff_editors.get(&diff.entity_id()) {
                column = column.child(
                    div()
                        .w_full()
                        .border_1()
                        .border_color(cx.theme().colors().border_variant)
                        .rounded_sm()
                        .overflow_hidden()
                        .child(editor.clone()),
                );
            }
        }
        column.into_any_element()
    }

    /// A question from the agent, at full prose weight (decision 9: a
    /// question, not an approval gate). Once answered it settles into a
    /// quiet line.
    fn render_elicitation(&self, id: &ElicitationEntryId, cx: &Context<Self>) -> AnyElement {
        let Some(session) = &self.session else {
            return div().into_any_element();
        };
        let Some((_, elicitation)) = session.thread.read(cx).elicitation(id) else {
            return div().into_any_element();
        };
        let message = elicitation.request.message.clone();
        if !matches!(elicitation.status, ElicitationStatus::Pending { .. }) {
            return Label::new(message)
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element();
        }
        let mut column = v_flex().w_full().gap_2().child(Label::new(message));
        match elicitation_shape(&elicitation.request) {
            ElicitationShape::Choice { options, .. } => {
                let chosen = self.elicitation_choices.get(id).copied().unwrap_or(0);
                let mut list = v_flex().w_full().gap_1();
                for (option_index, (_, title)) in options.iter().enumerate() {
                    let answer_id = id.clone();
                    let is_chosen = option_index == chosen;
                    list = list.child(
                        div()
                            .id(("thock-chat-elicit-option", option_index))
                            .w_full()
                            .px_2()
                            .py_1()
                            .border_1()
                            .rounded_md()
                            .border_color(if is_chosen {
                                cx.theme().colors().border_focused
                            } else {
                                cx.theme().colors().border_variant
                            })
                            .when(is_chosen, |this| {
                                this.bg(cx.theme().colors().element_selected)
                            })
                            .cursor_pointer()
                            .child(Label::new(title.clone()))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.answer_elicitation(&answer_id, Some(option_index), cx);
                            })),
                    );
                }
                column = column.child(list).child(
                    Label::new("←/→ to choose · Enter to answer")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                );
            }
            ElicitationShape::Text { .. } => {
                if let Some(editor) = self.elicitation_editors.get(id) {
                    column = column
                        .child(
                            div()
                                .w_full()
                                .px_2()
                                .py_1()
                                .border_1()
                                .border_color(cx.theme().colors().border)
                                .rounded_md()
                                .bg(cx.theme().colors().editor_background)
                                .child(editor.clone()),
                        )
                        .child(
                            Label::new("Enter to answer")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        );
                }
            }
            ElicitationShape::Link { url } => {
                column = column.child(
                    h_flex().child(
                        Button::new("thock-chat-elicit-link", "Open Link")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(move |_, _, _window, cx| {
                                cx.open_url(&url);
                            })),
                    ),
                );
            }
            ElicitationShape::Confirm => {
                let accept_id = id.clone();
                let decline_id = id.clone();
                column = column.child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("thock-chat-elicit-ok", "OK")
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.answer_elicitation(&accept_id, None, cx);
                                })),
                        )
                        .child(Button::new("thock-chat-elicit-skip", "Not Now").on_click(
                            cx.listener(move |this, _, _window, cx| {
                                this.decline_elicitation(&decline_id, cx);
                            }),
                        )),
                );
            }
        }
        column.into_any_element()
    }

    fn render_chat(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let Some(session) = &self.session else {
            return self.render_empty_state(cx);
        };
        let generating = self.is_generating(cx);
        let entries = session.thread.read(cx).entries();
        let items = build_items(entries.iter().map(entry_shape));
        let selected_position = self.selected_position(&items);
        let vault_root = self.vault().map(|vault| vault.root.clone());
        let header = h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .justify_between()
            .child(Label::new(session.title.clone()).size(LabelSize::Small))
            .child(
                h_flex()
                    .gap_1()
                    .when(generating, |this| {
                        this.child(
                            IconButton::new("thock-chat-stop", IconName::Stop)
                                .icon_size(IconSize::Small)
                                .tooltip(move |_window, cx| {
                                    Tooltip::for_action("Stop", &StopChatTurn, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.stop_turn(&StopChatTurn, window, cx)
                                })),
                        )
                    })
                    .children(self.render_usage_button(cx))
                    .child(
                        IconButton::new("thock-chat-new", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(move |_window, cx| {
                                Tooltip::for_action("New chat", &NewChat, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_chat(&NewChat, window, cx)
                            })),
                    ),
            );
        let mut list = v_flex()
            .id("thock-chat-list")
            .flex_1()
            .min_h_0()
            .w_full()
            .px_3()
            .py_3()
            .gap_3()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .children(items.iter().enumerate().map(|(position, item)| {
                self.render_item(
                    position,
                    item,
                    entries,
                    selected_position == Some(position),
                    vault_root.as_deref(),
                    window,
                    cx,
                )
            }));
        // §5.6: one spinner for the whole turn, only until prose streams —
        // after that the growing answer is the signal.
        let streaming_prose = matches!(
            entries.last(),
            Some(AgentThreadEntry::AssistantMessage(message))
                if message
                    .chunks
                    .iter()
                    .any(|chunk| matches!(chunk, AssistantMessageChunk::Message { .. }))
        );
        if generating && !streaming_prose {
            list = list.child(
                h_flex()
                    .px_2()
                    .py_1()
                    .child(SpinnerLabel::new().size(LabelSize::Small)),
            );
        }
        // §5.5: the one place a broken turn is allowed to say so — in plain
        // language, without error styling.
        if let Some(failure) = &session.turn_failure {
            let retryable = failure.retryable && session.last_sent.is_some();
            list = list.child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .items_center()
                    .child(
                        Label::new(failure.message.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .when(retryable, |this| {
                        this.child(
                            Button::new("thock-chat-retry", "Try Again")
                                .style(ButtonStyle::Subtle)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.retry_turn(&RetryChatTurn, window, cx)
                                })),
                        )
                    }),
            );
        }
        v_flex()
            .size_full()
            .child(header)
            .child(Divider::horizontal())
            .child(list)
            .into_any_element()
    }

    fn render_composer(&self, cx: &Context<Self>) -> AnyElement {
        let exhausted = self
            .entitlement()
            .is_some_and(|entitlement| entitlement.is_exhausted());
        v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .child(
                div()
                    .w_full()
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .rounded_md()
                    .bg(cx.theme().colors().editor_background)
                    .child(self.message_editor.clone()),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        Label::new(if exhausted {
                            "Out of allowance for this cycle"
                        } else {
                            "Enter to send · Shift-Enter for a new line"
                        })
                        .size(LabelSize::XSmall)
                        .color(if exhausted {
                            Color::Error
                        } else {
                            Color::Muted
                        }),
                    )
                    .child(
                        IconButton::new("thock-chat-send", IconName::Send)
                            .icon_size(IconSize::Small)
                            .disabled(exhausted)
                            .tooltip(move |_window, cx| {
                                Tooltip::for_action("Send", &SendChatMessage, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.send_message(&SendChatMessage, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The header toggle that stands in for the always-on allowance bar:
    /// quiet while there is plenty left, amber once the warning mark is
    /// crossed, red at zero — so the signal still arrives when it matters.
    fn render_usage_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let entitlement = self.entitlement()?;
        let (icon, color) = if entitlement.is_exhausted() {
            (IconName::SignalLow, Color::Error)
        } else if entitlement.is_running_low() {
            (IconName::SignalMedium, Color::Warning)
        } else {
            (IconName::SignalHigh, Color::Muted)
        };
        let summary: SharedString = entitlement.balance_summary().into();
        Some(
            IconButton::new("thock-chat-usage", icon)
                .icon_size(IconSize::Small)
                .icon_color(color)
                .toggle_state(self.show_usage)
                .tooltip(move |_window, cx| {
                    Tooltip::with_meta("Usage", Some(&ToggleChatUsage), summary.clone(), cx)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_usage(&ToggleChatUsage, window, cx)
                }))
                .into_any_element(),
        )
    }

    /// The allowance, as a bar the wife test can read: how much of this
    /// cycle is used, in the plan's own words, turning amber at the warning
    /// mark and red at zero. Hidden behind the header's usage toggle.
    fn render_footer(&self, cx: &Context<Self>) -> AnyElement {
        let Some(entitlement) = self.entitlement() else {
            return div().into_any_element();
        };
        let color = if entitlement.is_exhausted() {
            cx.theme().status().error
        } else if entitlement.is_running_low() {
            cx.theme().status().warning
        } else {
            cx.theme().status().info
        };
        let label_color = if entitlement.is_exhausted() {
            Color::Error
        } else if entitlement.is_running_low() {
            Color::Warning
        } else {
            Color::Muted
        };
        v_flex()
            .w_full()
            .px_3()
            .pb_2()
            .gap_1()
            .child(
                ProgressBar::new(
                    "thock-chat-allowance",
                    entitlement.used_units.max(0) as f32,
                    entitlement.allowance_units.max(1) as f32,
                    cx,
                )
                .fg_color(color)
                .bg_color(cx.theme().colors().border_variant),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        Label::new(entitlement.balance_summary())
                            .size(LabelSize::XSmall)
                            .color(label_color),
                    )
                    .child(
                        Label::new(entitlement.plan_name.clone())
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
            .into_any_element()
    }

    fn render_empty_state(&self, cx: &Context<Self>) -> AnyElement {
        let content = match &self.connection {
            PlusConnection::Loading => v_flex().items_center().gap_1().child(
                Label::new("Checking your Thock Plus connection…")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
            PlusConnection::Connected { .. } => v_flex()
                .items_center()
                .gap_1()
                .child(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::XLarge)
                        .color(Color::Muted),
                )
                .child(div().mt_1().child(Label::new("Thock Agent is ready")))
                .child(
                    div().max_w(rems(18.)).child(
                        Label::new(
                            "Type below to chat, or run a skill from the Routines panel. It \
                             works inside your notes and shows you every change as it goes.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
                )
                .child(
                    v_flex()
                        .mt_3()
                        .gap_2()
                        .w(rems(13.))
                        .child(
                            Button::new("thock-chat-start", "New Chat")
                                .style(ButtonStyle::Filled)
                                .full_width()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.new_chat(&NewChat, window, cx)
                                })),
                        )
                        .child(
                            Button::new("thock-chat-use-own", "Use My Own Agent Instead")
                                .style(ButtonStyle::Subtle)
                                .full_width()
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.set_mode(ConnectionMode::Byo, cx)
                                })),
                        ),
                ),
            PlusConnection::Disconnected => v_flex()
                .items_center()
                .gap_1()
                .child(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::XLarge)
                        .color(Color::Muted),
                )
                .child(div().mt_1().child(Label::new("Thock Agent")))
                .child(
                    div().max_w(rems(18.)).child(
                        Label::new(
                            "An assistant that works out of the box: nothing to install, no \
                             keys to paste. Part of Thock Plus. Your own agent stays available \
                             in the Agent panel.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
                )
                .child(
                    div().mt_3().w(rems(13.)).child(
                        Button::new("thock-chat-connect", "Connect Thock Agent")
                            .style(ButtonStyle::Filled)
                            .full_width()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_connect(window, cx)),
                            ),
                    ),
                ),
            PlusConnection::Revoked(message) => {
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Icon::new(IconName::Warning)
                            .size(IconSize::XLarge)
                            .color(Color::Warning),
                    )
                    .child(
                        div().max_w(rems(18.)).child(
                            Label::new(message.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                    )
                    .child(
                        v_flex()
                            .mt_3()
                            .gap_2()
                            .w(rems(13.))
                            .child(
                                Button::new("thock-chat-reconnect", "Connect Again")
                                    .style(ButtonStyle::Filled)
                                    .full_width()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_connect(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("thock-chat-fallback", "Use My Own Agent")
                                    .style(ButtonStyle::Outlined)
                                    .full_width()
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.set_mode(ConnectionMode::Byo, cx)
                                    })),
                            ),
                    )
            }
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_4()
            .child(content)
            .into_any_element()
    }

    fn render_connect(&self, flow: &ConnectFlow, cx: &Context<Self>) -> AnyElement {
        let editor = flow.code_editor.clone();
        v_flex()
            .gap_2()
            .p_3()
            .child(Label::new("Connect the Thock Agent").size(LabelSize::Large))
            .child(
                Label::new(
                    "Enter the invite code you were given. Thock sets everything else up: the \
                     assistant, its model, and its allowance. No keys, no installs.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(
                div()
                    .child(editor.clone())
                    .px_1()
                    .py_1()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .rounded_sm(),
            )
            .when_some(flow.error.clone(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .child(div().mt_1().child(Divider::horizontal()))
            .child(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new("thock-chat-connect-cancel", "Cancel")
                            .on_click(cx.listener(|this, _, _window, cx| this.cancel_connect(cx))),
                    )
                    .child(
                        Button::new(
                            "thock-chat-connect-save",
                            if flow.busy {
                                "Connecting…"
                            } else {
                                "Connect"
                            },
                        )
                        .style(ButtonStyle::Filled)
                        .disabled(flow.busy)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                let code = editor.read(cx).text(cx);
                                this.submit_invite_code(code, window, cx);
                            },
                        )),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = match &self.view {
            PanelView::Connect(flow) => flow.code_editor.focus_handle(cx).is_focused(window),
            PanelView::Chat => {
                self.message_editor.focus_handle(cx).is_focused(window)
                    || self
                        .elicitation_editors
                        .values()
                        .any(|editor| editor.focus_handle(cx).is_focused(window))
            }
        };
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add(CHAT_PANEL_KEY);
        // Single-key list navigation would shadow typing, so `editing` gates
        // it in the keymap; `menu` brings the arrow-key defaults when the
        // list has focus.
        if editing {
            key_context.add("editing");
        } else {
            key_context.add("menu");
        }

        let content: AnyElement = match (&self.view, &self.vault_status) {
            (PanelView::Connect(flow), _) => self.render_connect(flow, cx),
            (PanelView::Chat, VaultStatus::Valid(_)) => {
                let starting = self.starting.clone();
                v_flex()
                    .size_full()
                    .child(match starting {
                        Some(status) => v_flex()
                            .flex_1()
                            .min_h_0()
                            .items_center()
                            .justify_center()
                            .child(
                                Label::new(status)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .into_any_element(),
                        None => div()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_chat(window, cx))
                            .into_any_element(),
                    })
                    .when(
                        matches!(self.connection, PlusConnection::Connected { .. }),
                        |this| {
                            this.child(Divider::horizontal())
                                .when(self.memory_nudge_due, |this| {
                                    this.child(self.render_memory_nudge(cx))
                                })
                                .child(self.render_composer(cx))
                                .when(self.show_usage, |this| this.child(self.render_footer(cx)))
                        },
                    )
                    .into_any_element()
            }
            (PanelView::Chat, _) => v_flex()
                .p_3()
                .child(
                    Label::new("Open a Thock vault to chat with the Thock Agent.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
        };

        v_flex()
            .id("thock-chat-panel")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::send_message))
            .on_action(cx.listener(Self::stop_turn))
            .on_action(cx.listener(Self::new_chat))
            .on_action(cx.listener(Self::focus_input))
            .on_action(cx.listener(Self::open_entry))
            .on_action(cx.listener(Self::expand_activity))
            .on_action(cx.listener(Self::collapse_activity))
            .on_action(cx.listener(Self::retry_turn))
            .on_action(cx.listener(Self::toggle_usage))
            .on_action(cx.listener(Self::dismiss_memory_nudge))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .size_full()
            .child(content)
    }
}

impl EventEmitter<PanelEvent> for ChatPanel {}

impl Focusable for ChatPanel {
    /// Dock activation lands in whatever takes typing: the invite code field
    /// or the message box. The list is reachable with `escape`.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.view {
            PanelView::Connect(flow) => flow.code_editor.focus_handle(cx),
            PanelView::Chat => match self.connection {
                PlusConnection::Connected { .. } => self.message_editor.focus_handle(cx),
                _ => self.focus_handle.clone(),
            },
        }
    }
}

impl Panel for ChatPanel {
    fn persistent_name() -> &'static str {
        "Thock Chat Panel"
    }

    fn panel_key() -> &'static str {
        CHAT_PANEL_KEY
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
        px(480.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Thread)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Thock Agent")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        ToggleChatFocus.boxed_clone()
    }

    fn activation_priority(&self) -> u32 {
        // Must be unique across all panels; 0-10 are taken (0-3 and 5-7
        // upstream, 4 Timeline, 8 Day Planner, 9 Agent, 10 Backlog).
        11
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &'static str) -> EntryShape {
        EntryShape::Tool(acp::ToolCallId::from(id))
    }

    fn keys(items: &[ChatItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                ChatItem::Entry(index) => format!("entry:{index}"),
                ChatItem::Activity { key, calls } => {
                    format!("activity:{key}:{}", calls.len())
                }
            })
            .collect()
    }

    #[test]
    fn consecutive_tool_calls_collapse_into_one_activity() {
        let items = build_items(vec![
            EntryShape::Visible,
            tool("a"),
            tool("b"),
            tool("c"),
            EntryShape::Visible,
        ]);
        assert_eq!(keys(&items), ["entry:0", "activity:a:3", "entry:4"]);
    }

    #[test]
    fn hidden_entries_neither_render_nor_split_a_run() {
        // A thought-only assistant message between tool calls (decision 7)
        // must not break the run into two activity lines.
        let items = build_items(vec![
            tool("a"),
            EntryShape::Hidden,
            tool("b"),
            EntryShape::Visible,
            EntryShape::Hidden,
        ]);
        assert_eq!(keys(&items), ["activity:a:2", "entry:3"]);
    }

    #[test]
    fn a_visible_entry_splits_runs_and_a_trailing_run_flushes() {
        let items = build_items(vec![tool("a"), EntryShape::Visible, tool("b"), tool("c")]);
        assert_eq!(keys(&items), ["activity:a:1", "entry:1", "activity:b:2"]);
    }

    #[test]
    fn activity_key_is_stable_while_the_run_grows() {
        // R4: a run that grows mid-turn keeps its key, so expansion state
        // and selection keyed by it survive the re-render.
        let before = build_items(vec![EntryShape::Visible, tool("a")]);
        let after = build_items(vec![EntryShape::Visible, tool("a"), tool("b"), tool("c")]);
        let key_of = |items: &[ChatItem]| match &items[1] {
            ChatItem::Activity { key, .. } => key.clone(),
            _ => panic!("expected an activity"),
        };
        assert_eq!(key_of(&before), key_of(&after));
    }

    #[test]
    fn a_message_arriving_mid_run_splits_it_and_keeps_the_first_key() {
        let before = build_items(vec![tool("a"), tool("b")]);
        let after = build_items(vec![tool("a"), EntryShape::Visible, tool("b")]);
        assert_eq!(keys(&before), ["activity:a:2"]);
        assert_eq!(keys(&after), ["activity:a:1", "entry:1", "activity:b:1"]);
    }

    fn call(kind: acp::ToolKind, note: Option<&str>) -> SummaryCall {
        SummaryCall {
            kind,
            note: note.map(str::to_string),
        }
    }

    #[test]
    fn a_single_read_names_the_note() {
        let calls = [call(acp::ToolKind::Read, Some("daily/2026-09-09.md"))];
        assert_eq!(
            summarize_activity(&calls, false),
            "Looked at daily/2026-09-09.md"
        );
        assert_eq!(
            summarize_activity(&calls, true),
            "Looking at daily/2026-09-09.md…"
        );
    }

    #[test]
    fn many_reads_become_one_looking_line() {
        // G1: the screenshot's seven read cards become one line.
        let calls = [
            call(acp::ToolKind::Read, Some("daily/2026-09-08.md")),
            call(acp::ToolKind::Read, Some("daily/2026-09-09.md")),
            call(acp::ToolKind::Read, Some("daily/2026-09-10.md")),
            call(acp::ToolKind::Search, None),
            call(acp::ToolKind::Read, Some("weekly/2026-W37.md")),
        ];
        assert_eq!(
            summarize_activity(&calls, false),
            "Looked through your notes"
        );
        assert_eq!(
            summarize_activity(&calls, true),
            "Looking through your notes…"
        );
    }

    #[test]
    fn a_write_wins_over_reads_and_names_the_note() {
        // Decision 6: same footprint, different wording.
        let calls = [
            call(acp::ToolKind::Read, Some("daily/2026-09-14.md")),
            call(acp::ToolKind::Edit, Some("daily/2026-09-15.md")),
        ];
        assert_eq!(
            summarize_activity(&calls, false),
            "Updated daily/2026-09-15.md"
        );
        assert_eq!(
            summarize_activity(&calls, true),
            "Updating daily/2026-09-15.md…"
        );
    }

    #[test]
    fn writes_to_many_notes_are_counted() {
        let calls = [
            call(acp::ToolKind::Edit, Some("daily/2026-09-15.md")),
            call(acp::ToolKind::Edit, Some("weekly/2026-W38.md")),
        ];
        assert_eq!(summarize_activity(&calls, false), "Updated 2 notes");
        assert_eq!(summarize_activity(&calls, true), "Updating your notes…");
    }

    #[test]
    fn deletes_alone_say_removed() {
        let calls = [call(acp::ToolKind::Delete, Some("inbox/old.md"))];
        assert_eq!(summarize_activity(&calls, false), "Removed inbox/old.md");
    }

    #[test]
    fn execute_and_the_rest_fall_back_to_the_generic_phrase() {
        let calls = [
            call(acp::ToolKind::Execute, None),
            call(acp::ToolKind::Think, None),
        ];
        assert_eq!(
            summarize_activity(&calls, false),
            "Worked behind the scenes"
        );
        assert_eq!(
            summarize_activity(&calls, true),
            "Working behind the scenes…"
        );
    }

    #[test]
    fn an_empty_run_still_produces_a_line() {
        assert_eq!(summarize_activity(&[], false), "Worked behind the scenes");
    }

    #[test]
    fn paths_render_vault_relative() {
        let root = Path::new("/Users/someone/Thock");
        assert_eq!(
            vault_relative_label(
                Path::new("/Users/someone/Thock/daily/2026-09-09.md"),
                Some(root)
            ),
            "daily/2026-09-09.md"
        );
    }

    #[test]
    fn outside_vault_paths_show_the_file_name_only() {
        // G2: no absolute path may reach the transcript, even for the
        // outside-vault calls the sandbox should have prevented.
        let root = Path::new("/Users/someone/Thock");
        assert_eq!(
            vault_relative_label(Path::new("/etc/hosts"), Some(root)),
            "hosts"
        );
        assert_eq!(
            vault_relative_label(Path::new("/somewhere/else.md"), None),
            "else.md"
        );
    }
}
