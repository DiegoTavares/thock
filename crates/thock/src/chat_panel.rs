//! The Thock Agent chat panel (V25 §3 items 3 to 7): a right-dock panel that
//! hosts the hosted agent (Pi over ACP) in a friendly chat. Thock Plus
//! supplies the gateway key and the model behind each tier; the panel shows
//! what the agent does in plain language as it works, previews every edit,
//! and keeps the allowance balance in its footer. There are no per-change
//! approval prompts (decision 16): every permission the harness asks for is
//! granted, and safety is the vault-scoped process plus the checkpoint taken
//! before each session.

use acp_thread::{
    AcpThread, AcpThreadEvent, AgentConnection, AgentThreadEntry, AssistantMessageChunk,
    SelectedPermissionOutcome, ThreadStatus, ToolCall, ToolCallContent, ToolCallStatus,
};
use agent_client_protocol::schema::v1 as acp;
use anyhow::Result;
use collections::HashMap;
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
use std::rc::Rc;
use ui::prelude::*;
use ui::{Button, ButtonStyle, Divider, Icon, IconButton, Label, ProgressBar, Tooltip};
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
    last_error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
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
    selected_entry: Option<usize>,
    /// Read-only editors over the diffs tool calls produced, keyed by the
    /// diff entity so a re-render never rebuilds them.
    diff_editors: HashMap<EntityId, Entity<Editor>>,
    /// A launch that is waiting on the connect flow or the keychain read.
    pending_launch: Option<LaunchRequest>,
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
                selected_entry: None,
                diff_editors: HashMap::default(),
                pending_launch: None,
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
                self.start_session(request, api_key, model, cx);
            }
        }
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
        self.diff_editors.clear();
        self.selected_entry = None;
        self.starting = Some("Setting up the Thock Agent…".into());
        self.view = PanelView::Chat;
        cx.notify();

        let project = self.project.clone();
        let window_handle = self.window_handle;
        let max_turns = self
            .entitlement()
            .map(|entitlement| entitlement.limits.max_turns_per_session)
            .unwrap_or(0);
        let tier = request.tier;
        cx.spawn(async move |this, cx| {
            let started = async {
                let command =
                    hosted_agent::prepare_launch(&project, tier, &model, &api_key, cx).await?;
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
            last_error: None,
            _subscriptions: vec![subscription],
        });
        if let Some(kickoff) = request.kickoff {
            self.send_text(kickoff, cx);
        }
        window.focus(&self.message_editor.focus_handle(cx), cx);
        cx.notify();
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
            AcpThreadEvent::Stopped(_) => {
                self.refresh_entitlement(cx);
            }
            AcpThreadEvent::Error => {
                if let Some(session) = &mut self.session {
                    session.last_error =
                        Some("The Thock Agent hit a problem and stopped this turn.".into());
                }
            }
            AcpThreadEvent::LoadError(error) => {
                if let Some(session) = &mut self.session {
                    session.last_error = Some(error.to_string().into());
                }
            }
            AcpThreadEvent::Refusal => {
                if let Some(session) = &mut self.session {
                    session.last_error = Some("The Thock Agent declined to do that.".into());
                }
            }
            _ => {}
        }
        cx.notify();
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
        session.last_error = None;
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
                    session.last_error = Some(format!("{error:#}").into());
                }
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
        cx.notify();
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

    fn entry_count(&self, cx: &App) -> usize {
        self.session
            .as_ref()
            .map(|session| session.thread.read(cx).entries().len())
            .unwrap_or(0)
    }

    fn select_index(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.selected_entry = index;
        if let Some(index) = index {
            self.scroll_handle.scroll_to_item(index);
        }
        cx.notify();
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.entry_count(cx);
        if count == 0 {
            return;
        }
        let next = match self.selected_entry {
            Some(index) => (index + 1).min(count - 1),
            None => 0,
        };
        self.select_index(Some(next), cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.entry_count(cx);
        if count == 0 {
            return;
        }
        let previous = match self.selected_entry {
            Some(index) => index.saturating_sub(1),
            None => count - 1,
        };
        self.select_index(Some(previous), cx);
    }

    fn select_first(&mut self, _: &menu::SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        if self.entry_count(cx) > 0 {
            self.select_index(Some(0), cx);
        }
    }

    fn select_last(&mut self, _: &menu::SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.entry_count(cx);
        if count > 0 {
            self.select_index(Some(count - 1), cx);
        }
    }

    /// `enter`: connect in the connect flow, send from the message box, or
    /// open the selected step's note from the list (falling back to the
    /// message box when the step touched nothing).
    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        match &self.view {
            PanelView::Connect(flow) => {
                let code = flow.code_editor.read(cx).text(cx);
                self.submit_invite_code(code, window, cx);
            }
            PanelView::Chat => {
                if self.message_editor.focus_handle(cx).is_focused(window) {
                    self.send_message(&SendChatMessage, window, cx);
                } else if !self.open_selected_entry(window, cx) {
                    window.focus(&self.message_editor.focus_handle(cx), cx);
                }
            }
        }
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

    /// Opens the first file the selected tool call touched. Returns whether
    /// there was one.
    fn open_selected_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.selected_entry else {
            return false;
        };
        let Some(session) = &self.session else {
            return false;
        };
        let path = match session.thread.read(cx).entries().get(index) {
            Some(AgentThreadEntry::ToolCall(tool_call)) => tool_call
                .locations
                .first()
                .map(|location| location.path.clone()),
            _ => None,
        };
        let Some(path) = path else {
            return false;
        };
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
        true
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
        self.diff_editors.clear();
        self.selected_entry = None;
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
        let style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        if muted {
            style.with_muted_text(cx)
        } else {
            style
        }
    }

    fn render_entry(
        &self,
        index: usize,
        entry: &AgentThreadEntry,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_entry == Some(index);
        let body: AnyElement = match entry {
            AgentThreadEntry::UserMessage(message) => {
                let content: AnyElement = match message.content.markdown() {
                    Some(markdown) => MarkdownElement::new(
                        markdown.clone(),
                        self.markdown_style(false, window, cx),
                    )
                    .into_any_element(),
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
                            .bg(cx.theme().colors().element_background)
                            .child(content),
                    )
                    .into_any_element()
            }
            AgentThreadEntry::AssistantMessage(message) => {
                let mut column = v_flex().w_full().gap_1();
                let mut showed_thought = false;
                for chunk in &message.chunks {
                    match chunk {
                        AssistantMessageChunk::Message { block, .. } => {
                            if let Some(markdown) = block.markdown() {
                                column = column.child(MarkdownElement::new(
                                    markdown.clone(),
                                    self.markdown_style(false, window, cx),
                                ));
                            }
                        }
                        AssistantMessageChunk::Thought { .. } => {
                            if !showed_thought {
                                showed_thought = true;
                                column = column.child(
                                    Label::new("Thinking it through…")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                );
                            }
                        }
                    }
                }
                column.into_any_element()
            }
            AgentThreadEntry::ToolCall(tool_call) => self.render_tool_call(tool_call, window, cx),
            AgentThreadEntry::Elicitation(_) => {
                Label::new("The Thock Agent asked a question this panel can't show yet.")
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element()
            }
            AgentThreadEntry::CompletedPlan(entries) => Label::new(format!(
                "Finished a plan of {} step{}.",
                entries.len(),
                if entries.len() == 1 { "" } else { "s" }
            ))
            .size(LabelSize::Small)
            .color(Color::Muted)
            .into_any_element(),
            AgentThreadEntry::ContextCompaction(_) => {
                Label::new("Tidied up the conversation to keep going.")
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element()
            }
        };
        div()
            .id(("thock-chat-entry", index))
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
                this.select_index(Some(index), cx);
            }))
            .child(body)
            .into_any_element()
    }

    /// One tool call as a sentence: what the agent is doing, to what, and
    /// how it went, with the edit itself previewed underneath.
    fn render_tool_call(
        &self,
        tool_call: &ToolCall,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (verb, icon) = match tool_call.kind {
            acp::ToolKind::Read => ("Reading", IconName::ToolSearch),
            acp::ToolKind::Edit => ("Editing", IconName::ToolPencil),
            acp::ToolKind::Delete => ("Deleting", IconName::ToolDeleteFile),
            acp::ToolKind::Move => ("Moving", IconName::ArrowRightLeft),
            acp::ToolKind::Search => ("Searching", IconName::ToolSearch),
            acp::ToolKind::Execute => ("Running", IconName::ToolTerminal),
            acp::ToolKind::Think => ("Thinking about", IconName::ToolThink),
            acp::ToolKind::Fetch => ("Looking up", IconName::ToolWeb),
            acp::ToolKind::SwitchMode => ("Switching to", IconName::ArrowRightLeft),
            _ => ("Working on", IconName::ToolHammer),
        };
        let status: AnyElement = match &tool_call.status {
            ToolCallStatus::Pending
            | ToolCallStatus::InProgress
            | ToolCallStatus::WaitingForConfirmation { .. } => Label::new("…")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
            ToolCallStatus::Completed => Icon::new(IconName::Check)
                .size(IconSize::Small)
                .color(Color::Success)
                .into_any_element(),
            ToolCallStatus::Failed => Icon::new(IconName::XCircle)
                .size(IconSize::Small)
                .color(Color::Error)
                .into_any_element(),
            ToolCallStatus::Rejected | ToolCallStatus::Canceled => Label::new("stopped")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
        };
        let failed = matches!(tool_call.status, ToolCallStatus::Failed);
        let mut column = v_flex().w_full().gap_1().child(
            h_flex()
                .w_full()
                .gap_1()
                .items_center()
                .child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
                .child(Label::new(verb).size(LabelSize::Small).color(Color::Muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(MarkdownElement::new(
                            tool_call.label.clone(),
                            self.markdown_style(true, window, cx),
                        )),
                )
                .child(status),
        );
        for content in &tool_call.content {
            match content {
                ToolCallContent::Diff(diff) => {
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
                ToolCallContent::ContentBlock(block) if failed => {
                    if let Some(markdown) = block.markdown() {
                        column = column.child(div().pl_5().child(MarkdownElement::new(
                            markdown.clone(),
                            self.markdown_style(true, window, cx),
                        )));
                    }
                }
                ToolCallContent::ContentBlock(_) | ToolCallContent::Terminal(_) => {}
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
            .px_1()
            .gap_1()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .children(
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| self.render_entry(index, entry, window, cx)),
            );
        if generating {
            list = list.child(
                Label::new("Thock Agent is working…")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
        }
        if let Some(error) = &session.last_error {
            list = list.child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .child(
                        Icon::new(IconName::Warning)
                            .size(IconSize::Small)
                            .color(Color::Warning),
                    )
                    .child(
                        Label::new(error.clone())
                            .size(LabelSize::Small)
                            .color(Color::Warning),
                    ),
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

    /// The allowance, as a bar the wife test can read: how much of this
    /// cycle is used, in the plan's own words, turning amber at the warning
    /// mark and red at zero.
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
            PanelView::Chat => self.message_editor.focus_handle(cx).is_focused(window),
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
                                .child(self.render_composer(cx))
                                .child(self.render_footer(cx))
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
