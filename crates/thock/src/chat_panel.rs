//! The Thock Agent chat (V25 §3 items 3 to 7, refined by V26 and V44): the
//! hosted agent (Pi over ACP) in a chat that reads as a conversation, not a
//! build log. A turn shows one reply, the last message before it stopped for
//! you, and everything the agent said or did before that folds into one
//! receipt that expands in place (V44 decision 5); a ritual's kickoff is a
//! marker, not a message you typed; paths are vault-relative; per-call
//! failures never surface as errors (the agent's prose carries them); and the
//! allowance balance lives in the footer. There are no per-change approval
//! prompts (V25 decision 16): every permission the harness asks for is
//! granted. The process is not OS-confined yet (the V25 decision 11 sandbox
//! is Stage 2); safety is the prompt and the vault-guard extension keeping
//! writes inside the vault and append-only, plus the checkpoint taken before
//! each session.

use acp_thread::{
    AcpThread, AcpThreadEvent, AgentConnection, AgentThreadEntry, AssistantMessageChunk,
    ElicitationEntryId, ElicitationStatus, PermissionOptions, SelectedPermissionOutcome,
    ThreadStatus, ToolCall, ToolCallStatus,
};
use agent_client_protocol::schema::v1 as acp;
use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate, TimeDelta};
use collections::{HashMap, HashSet};
use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use gpui::{
    Action, AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle, Focusable, FontFeatures,
    FontWeight, KeyContext, Pixels, ScrollHandle, SharedString, Subscription, TextStyleRefinement,
    WeakEntity, Window, actions, div, px, relative,
};
use language::language_settings::SoftWrap;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::Project;
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use text::{Bias, Point};
use ui::prelude::*;
use ui::{Button, ButtonStyle, Label, ProgressBar, SpinnerLabel};
use util::ResultExt as _;
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
        /// Opens or closes the details of what the Thock Agent did in the
        /// selected step.
        ToggleChatActivity,
        /// Moves the keyboard focus from the message box to the chat's last
        /// message.
        FocusChatThread,
    ]
);

/// Answers the Thock Agent's question with one of its choices.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = thock)]
pub struct AnswerChatChoice(pub usize);

/// Mounts the chat over every workspace's centre (V44 §7.1) and routes its
/// workspace-level actions to it.
pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        let chat = ChatPanel::new(workspace, window, cx);
        workspace.set_center_overlay(Some(chat.clone().into()), cx);
        for_action::<ToggleChatFocus>(workspace, &chat, |chat, window, cx| {
            chat.toggle_focus(window, cx)
        });
        for_action::<ConnectThockAgent>(workspace, &chat, |chat, window, cx| {
            chat.open_thread(window, cx);
            chat.open_connect(window, cx);
        });
        for_action::<DisconnectThockAgent>(workspace, &chat, |chat, _, cx| chat.disconnect(cx));
        for_action::<UseThockAgent>(workspace, &chat, |chat, window, cx| {
            chat.set_mode(ConnectionMode::Hosted, cx);
            chat.open_thread(window, cx);
        });
        for_action::<UseOwnAgent>(workspace, &chat, |chat, _, cx| {
            chat.set_mode(ConnectionMode::Byo, cx)
        });
        workspace.register_action(|workspace, _: &NewChat, window, cx| {
            ChatPanel::launch_in_workspace(workspace, LaunchRequest::conversation(), window, cx);
        });
    })
    .detach();
}

fn for_action<A: Action>(
    workspace: &mut Workspace,
    chat: &Entity<ChatPanel>,
    handler: impl Fn(&mut ChatPanel, &mut Window, &mut Context<ChatPanel>) + Copy + 'static,
) {
    let chat = chat.clone();
    workspace.register_action(move |_, _: &A, window, cx| {
        chat.update(cx, |chat, cx| handler(chat, window, cx));
    });
}

/// True when Run actions should go to the hosted chat instead of the
/// terminal panel: an explicit `[agent] mode`, or, when nothing was chosen,
/// whether a Thock Plus connection exists on this machine.
pub fn hosted_mode_active(workspace: &Workspace, cx: &App) -> bool {
    ChatPanel::in_workspace(workspace)
        .map(|chat| chat.read(cx).effective_mode() == ConnectionMode::Hosted)
        .unwrap_or(false)
}

/// Where the chat stands (V44 §5.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Presence {
    /// The pebble, and nothing else.
    Resting,
    /// The pebble types: a turn is running.
    Working,
    /// A bubble floats up from the pebble.
    NeedsYou,
    /// The thread is open and the page has made room.
    Talking,
}

/// What waits for you while the thread is folded (§5.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Waiting {
    /// An `ask` request or an elicitation.
    Question,
    /// A reply that arrived while the thread was folded.
    Reply,
    /// A turn that couldn't finish.
    Failure,
    /// Thock Plus isn't connected and you summoned the chat (§5.11).
    Connect,
}

/// The pebble's size and its distance from the centre's corner (§5.1).
const PEBBLE: f32 = 44.;
const PEBBLE_RIGHT: f32 = 24.;
const PEBBLE_BOTTOM: f32 = 20.;
/// The thread's column, the composer in it, and the room they take (§5.3).
const THREAD_WIDTH: f32 = 360.;
const COMPOSER_WIDTH: f32 = 300.;
const INSET: f32 = 408.;
/// Below this the note is unreadable, so the thread floats instead (§5.3).
const MIN_NOTE_WIDTH: f32 = 400.;
/// How far above the bottom a Needs-you bubble sits (§5.3).
const BUBBLE_BOTTOM: f32 = 72.;

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
    /// Whether the first message is a ritual's kickoff prompt, which shows
    /// as a marker rather than as your bubble (V44 decision 7).
    kickoff: bool,
    _subscriptions: Vec<Subscription>,
}

struct TurnFailure {
    message: SharedString,
    retryable: bool,
}

/// One row of the thread after the fold (V44 decision 5). The rows are
/// derived on every read — the thread stays the source of truth — and a
/// receipt is keyed by its first tool call so expansion and selection
/// survive a run that grows while a turn streams.
enum ChatItem {
    /// "You started Wrap Today": a ritual's kickoff prompt, which you never
    /// typed (decision 7).
    Kickoff(usize),
    /// Your bubble.
    User(usize),
    /// Thock's reply: the last message before the turn stopped for you.
    Reply(usize),
    /// What the turn did, as one line that expands in place.
    Receipt {
        key: acp::ToolCallId,
        calls: Vec<(usize, acp::ToolCallId)>,
    },
    /// One row of an open receipt: a call, selectable so `enter` opens its
    /// note at its line (decision 14).
    Row {
        receipt: acp::ToolCallId,
        index: usize,
        id: acp::ToolCallId,
    },
    /// A question that stopped the turn for you.
    Question(usize),
    /// The turn is still running: its calls so far feed the live line, and
    /// its prose waits for the stop (decision 6).
    Typing {
        calls: Vec<(usize, acp::ToolCallId)>,
    },
}

impl ChatItem {
    /// The selection key; the typing bubble can't be selected.
    fn key(&self) -> Option<ItemKey> {
        match self {
            ChatItem::Kickoff(index)
            | ChatItem::User(index)
            | ChatItem::Reply(index)
            | ChatItem::Question(index) => Some(ItemKey::Entry(*index)),
            ChatItem::Receipt { key, .. } => Some(ItemKey::Receipt(key.clone())),
            ChatItem::Row { id, .. } => Some(ItemKey::Call(id.clone())),
            ChatItem::Typing { .. } => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ItemKey {
    Entry(usize),
    Receipt(acp::ToolCallId),
    Call(acp::ToolCallId),
}

/// How one thread entry takes part in the fold.
enum EntryShape {
    User,
    /// Prose from Thock.
    Assistant,
    Tool(acp::ToolCallId),
    /// A question that stops the turn until it is answered.
    Question {
        answered: bool,
    },
    /// Renders nothing and folds into nothing: a thought-only message, a
    /// plan, a compaction note.
    Hidden,
}

/// A question still waiting for you: an elicitation, or an `ask` request.
#[derive(Clone)]
enum PendingQuestion {
    Elicitation(ElicitationEntryId),
    Ask(acp::ToolCallId),
}

/// pi-acp carries a Pi extension's `select` as a permission request whose
/// tool call id starts with this; the chat shows it as chips (V44 §5.5).
const ASK_REQUEST_PREFIX: &str = "pi-ui-";

fn is_ask_request(id: &acp::ToolCallId) -> bool {
    id.0.starts_with(ASK_REQUEST_PREFIX)
}

/// The model's own `ask` tool call, which the request above answers for it.
/// It has nothing to show and nothing to count.
fn is_ask_call(tool_call: &ToolCall) -> bool {
    tool_call
        .raw_input
        .as_ref()
        .is_some_and(|input| input.get("question").is_some() && input.get("options").is_some())
}

/// The chips an `ask` request offers, in order.
fn ask_options(options: &PermissionOptions) -> Vec<&acp::PermissionOption> {
    match options {
        PermissionOptions::Flat(options) => options.iter().collect(),
        _ => Vec::new(),
    }
}

fn entry_shape(entry: &AgentThreadEntry, thread: &AcpThread) -> EntryShape {
    match entry {
        AgentThreadEntry::UserMessage(_) => EntryShape::User,
        AgentThreadEntry::AssistantMessage(message) => {
            if message
                .chunks
                .iter()
                .any(|chunk| matches!(chunk, AssistantMessageChunk::Message { .. }))
            {
                EntryShape::Assistant
            } else {
                EntryShape::Hidden
            }
        }
        AgentThreadEntry::ToolCall(tool_call) if is_ask_request(&tool_call.id) => {
            EntryShape::Question {
                answered: !matches!(
                    tool_call.status,
                    ToolCallStatus::WaitingForConfirmation { .. }
                ),
            }
        }
        AgentThreadEntry::ToolCall(tool_call) if is_ask_call(tool_call) => EntryShape::Hidden,
        AgentThreadEntry::ToolCall(tool_call) => EntryShape::Tool(tool_call.id.clone()),
        AgentThreadEntry::Elicitation(id) => EntryShape::Question {
            answered: thread.elicitation(id).is_none_or(|(_, elicitation)| {
                !matches!(elicitation.status, ElicitationStatus::Pending { .. })
            }),
        },
        AgentThreadEntry::CompletedPlan(_) | AgentThreadEntry::ContextCompaction(_) => {
            EntryShape::Hidden
        }
    }
}

/// Folds the thread into rows (V44 §5.4). A turn runs from your message (or
/// the kickoff) until it stops for you: your next message, a question, or
/// the end of the thread. Only its last message is the reply; every earlier
/// message and every tool call folds into its receipt. While the thread is
/// `live` the open turn shows the typing bubble instead, and nothing before
/// your first message renders (decision 8).
fn build_items(
    shapes: impl IntoIterator<Item = EntryShape>,
    ritual_kickoff: bool,
    live: bool,
) -> Vec<ChatItem> {
    let mut items = Vec::new();
    let mut started = false;
    let mut reply = None;
    let mut calls: Vec<(usize, acp::ToolCallId)> = Vec::new();
    let mut waiting = false;
    let flush = |items: &mut Vec<ChatItem>,
                 reply: &mut Option<usize>,
                 calls: &mut Vec<(usize, acp::ToolCallId)>| {
        if let Some(index) = reply.take() {
            items.push(ChatItem::Reply(index));
        }
        if let Some((_, first)) = calls.first() {
            items.push(ChatItem::Receipt {
                key: first.clone(),
                calls: std::mem::take(calls),
            });
        }
    };
    for (index, shape) in shapes.into_iter().enumerate() {
        match shape {
            EntryShape::User => {
                flush(&mut items, &mut reply, &mut calls);
                let kickoff = ritual_kickoff && !started;
                started = true;
                waiting = false;
                items.push(if kickoff {
                    ChatItem::Kickoff(index)
                } else {
                    ChatItem::User(index)
                });
            }
            _ if !started => {}
            EntryShape::Assistant => {
                reply = Some(index);
                waiting = false;
            }
            EntryShape::Tool(id) => {
                calls.push((index, id));
                waiting = false;
            }
            EntryShape::Question { answered } => {
                flush(&mut items, &mut reply, &mut calls);
                items.push(ChatItem::Question(index));
                waiting = !answered;
            }
            EntryShape::Hidden => {}
        }
    }
    if live && started && !waiting {
        items.push(ChatItem::Typing { calls });
    } else {
        flush(&mut items, &mut reply, &mut calls);
    }
    items
}

/// The tool calls of a receipt, as the summary line needs them.
fn summary_calls(
    calls: &[(usize, acp::ToolCallId)],
    entries: &[AgentThreadEntry],
    vault_root: Option<&Path>,
) -> Vec<SummaryCall> {
    calls
        .iter()
        .filter_map(|(index, _)| match entries.get(*index) {
            Some(AgentThreadEntry::ToolCall(tool_call)) => Some(SummaryCall {
                kind: tool_call.kind,
                note: tool_call
                    .locations
                    .first()
                    .map(|location| vault_relative_label(&location.path, vault_root)),
            }),
            _ => None,
        })
        .collect()
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

/// The verb of a receipt row (V44 §5.4) and whether the call changed a
/// note, which sets the verb in amber.
fn row_verb(kind: &acp::ToolKind) -> (&'static str, bool) {
    match kind {
        acp::ToolKind::Read => ("read", false),
        acp::ToolKind::Search => ("searched", false),
        acp::ToolKind::Fetch => ("looked up", false),
        acp::ToolKind::Edit => ("changed", true),
        acp::ToolKind::Delete => ("removed", true),
        acp::ToolKind::Move => ("moved", true),
        acp::ToolKind::Execute => ("ran", false),
        acp::ToolKind::Think => ("thought", false),
        _ => ("did", false),
    }
}

fn is_change(entry: Option<&AgentThreadEntry>) -> bool {
    matches!(
        entry,
        Some(AgentThreadEntry::ToolCall(tool_call)) if row_verb(&tool_call.kind).1
    )
}

/// The face Thock speaks in (V44 §5.7); yours is the UI face.
const THOCK_FACE: &str = "Petrona";

/// The text of a reply: its prose chunks, in order.
fn reply_text(message: &acp_thread::AssistantMessage, cx: &App) -> String {
    message
        .chunks
        .iter()
        .filter_map(|chunk| match chunk {
            AssistantMessageChunk::Message { block, .. } => block.text_content(cx),
            _ => None,
        })
        .collect::<Vec<_>>()
        .concat()
}

/// A reply split at blank lines, one bubble each (V44 §5.4). Blank lines
/// inside a fenced code block don't split.
fn paragraphs(source: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    for line in source.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if line.trim().is_empty() && !in_fence {
            if !current.trim().is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
            current.clear();
            continue;
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

/// The bubbles of one reply, rebuilt when its text changes.
struct ReplyBubbles {
    source: String,
    paragraphs: Vec<Entity<Markdown>>,
}

/// The day marks above turns (V44 §5.4): the first turn, a turn more than
/// thirty minutes after the previous one, and a turn on a new day. Turns
/// are keyed by the entry that started them.
fn day_marks(
    turn_times: &BTreeMap<usize, DateTime<Local>>,
    today: NaiveDate,
) -> BTreeMap<usize, String> {
    let mut marks = BTreeMap::new();
    let mut previous: Option<DateTime<Local>> = None;
    for (&index, &time) in turn_times {
        let gap = previous.is_none_or(|previous| {
            time.date_naive() != previous.date_naive() || time - previous > TimeDelta::minutes(30)
        });
        if gap {
            let day = if time.date_naive() == today {
                "TODAY".to_string()
            } else {
                time.format("%A").to_string().to_uppercase()
            };
            marks.insert(index, format!("{day} · {}", time.format("%H:%M")));
        }
        previous = Some(time);
    }
    marks
}

/// Lining-up digits for the day marks.
fn tabular_numerals() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".to_string(), 1)]))
}

/// A key drawn as a keycap: beside a chip, or `esc` in the composer.
fn keycap(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_1()
        .rounded(px(4.))
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .text_size(rems_from_px(10.))
        .line_height(rems_from_px(14.))
        .text_color(cx.theme().colors().text_muted)
        .child(text.into())
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
    /// The thread is open: Talking (§5.2).
    thread_open: bool,
    /// What waits for you while the thread is folded (§5.2).
    waiting: Option<Waiting>,
    /// Whether what waits floats as a bubble, or sits folded in the
    /// pebble's dot.
    bubble_up: bool,
    /// Where focus was before the chat took it, given back on fold.
    previous_focus: Option<FocusHandle>,
    /// The centre group's bounds from the last paint, for the room rules
    /// (§5.3).
    centre_bounds: Bounds<Pixels>,
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
    /// The selected row, keyed so it survives the re-fold while a turn
    /// streams: receipts by tool call id, everything else by its entry index
    /// (entries only append while a turn streams).
    selected: Option<ItemKey>,
    /// The receipts whose rows are open, by receipt key.
    expanded_receipts: HashSet<acp::ToolCallId>,
    /// The highlighted option per pending choice elicitation.
    elicitation_choices: HashMap<ElicitationEntryId, usize>,
    /// Single-line editors for pending free-text elicitations.
    elicitation_editors: HashMap<ElicitationEntryId, Entity<Editor>>,
    /// The highlighted chip per pending `ask` request.
    ask_choices: HashMap<acp::ToolCallId, usize>,
    /// The label you picked per answered `ask` request, shown as your
    /// bubble.
    ask_answers: HashMap<acp::ToolCallId, SharedString>,
    /// Thock's replies as bubbles (§5.4), by entry index.
    bubbles: HashMap<usize, ReplyBubbles>,
    /// When each turn started, by the entry that started it: your message,
    /// or the question you answered. The thread keeps no times of its own.
    turn_times: BTreeMap<usize, DateTime<Local>>,
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
                editor.set_placeholder_text("Message Thock", window, cx);
                editor.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
                editor
            });
            let focus_handle = cx.focus_handle();
            // Focus leaving for a note folds the thread (§5.2). Nothing is
            // given back then: the note has the focus already.
            let focus_out =
                cx.on_focus_out(&focus_handle, window, |this: &mut Self, _, window, cx| {
                    if !this.focus_handle.contains_focused(window, cx) {
                        this.fold_quietly(cx);
                    }
                });
            let mut this = Self {
                workspace: weak_workspace,
                window_handle: window.window_handle(),
                project,
                focus_handle,
                thread_open: false,
                waiting: None,
                bubble_up: false,
                previous_focus: None,
                centre_bounds: Bounds::default(),
                vault_status: VaultStatus::NotAVault,
                connection: PlusConnection::Loading,
                mode: None,
                view: PanelView::Chat,
                session: None,
                starting: None,
                message_editor,
                scroll_handle: ScrollHandle::new(),
                selected: None,
                expanded_receipts: HashSet::default(),
                elicitation_choices: HashMap::default(),
                elicitation_editors: HashMap::default(),
                ask_choices: HashMap::default(),
                ask_answers: HashMap::default(),
                bubbles: HashMap::default(),
                turn_times: BTreeMap::new(),
                show_usage: false,
                pending_launch: None,
                memory_nudge_due: false,
                _subscriptions: vec![project_subscription, focus_out],
            };
            this.refresh_vault_status(cx);
            this.load_connection(cx);
            this
        })
    }

    /// The chat mounted over `workspace`'s centre, once `init` has run.
    pub fn in_workspace(workspace: &Workspace) -> Option<Entity<Self>> {
        workspace.center_overlay()?.downcast::<Self>().ok()
    }

    /// Launches `request` with the hosted agent, routing through the connect
    /// flow first when Thock Plus isn't connected. A ritual runs behind the
    /// pebble; a conversation opens the thread.
    pub fn launch_in_workspace(
        workspace: &mut Workspace,
        request: LaunchRequest,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let Some(chat) = Self::in_workspace(workspace) else {
            log::warn!("Thock: the chat isn't mounted yet; launch dropped");
            return;
        };
        chat.update(cx, |chat, cx| {
            if !request.is_ritual() {
                chat.open_thread(window, cx);
            }
            chat.launch(request, window, cx);
        });
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

    fn presence(&self, cx: &App) -> Presence {
        if self.thread_open {
            Presence::Talking
        } else if self.bubble_up && self.waiting.is_some() {
            Presence::NeedsYou
        } else if self.is_generating(cx) || self.starting.is_some() {
            Presence::Working
        } else {
            Presence::Resting
        }
    }

    /// `cmd-alt-n` (§5.12): opens the thread, focuses a waiting bubble, or
    /// folds the chat when it already has focus.
    pub fn toggle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focus_handle.contains_focused(window, cx);
        match self.presence(cx) {
            Presence::Talking if focused => self.fold(window, cx),
            Presence::Talking => self.focus_thread(window, cx),
            Presence::NeedsYou if focused => self.open_thread(window, cx),
            Presence::NeedsYou => self.focus_bubble(window, cx),
            Presence::Resting | Presence::Working => {
                let unconnected = matches!(
                    self.connection,
                    PlusConnection::Disconnected | PlusConnection::Revoked(_)
                );
                if unconnected && self.session.is_none() {
                    // Not connected (§5.11): the bubble offers the ways in.
                    self.needs_you(Waiting::Connect, cx);
                    self.focus_bubble(window, cx);
                } else {
                    self.open_thread(window, cx);
                }
            }
        }
    }

    /// Opens the thread (Talking): the page makes room (§5.3) and the keys
    /// land where they are useful, on a waiting question or in the composer.
    pub fn open_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_focus(window, cx);
        self.thread_open = true;
        self.bubble_up = false;
        if self.waiting == Some(Waiting::Connect) {
            self.waiting = None;
        }
        self.sync_room(cx);
        self.focus_thread(window, cx);
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    fn focus_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.view {
            PanelView::Connect(flow) => window.focus(&flow.code_editor.focus_handle(cx), cx),
            PanelView::Chat if self.latest_pending_question(cx).is_some() => {
                window.focus(&self.focus_handle, cx)
            }
            PanelView::Chat => window.focus(&self.message_editor.focus_handle(cx), cx),
        }
    }

    fn focus_bubble(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_focus(window, cx);
        match &self.view {
            PanelView::Connect(flow) => window.focus(&flow.code_editor.focus_handle(cx), cx),
            PanelView::Chat => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    /// Where to give focus back on fold: wherever it is now, unless the chat
    /// already has it.
    fn remember_focus(&mut self, window: &Window, cx: &App) {
        if !self.focus_handle.contains_focused(window, cx) {
            self.previous_focus = window.focused(cx);
        }
    }

    /// Folds the thread or a bubble and hands focus back to the page (§5.2).
    fn fold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had_focus = self.focus_handle.contains_focused(window, cx);
        self.fold_quietly(cx);
        if had_focus && let Some(previous) = self.previous_focus.take() {
            window.focus(&previous, cx);
        }
    }

    /// Folds without touching focus: the thread closes, a bubble becomes the
    /// pebble's dot, and the page takes its width back.
    fn fold_quietly(&mut self, cx: &mut Context<Self>) {
        self.thread_open = false;
        self.bubble_up = false;
        if self.waiting == Some(Waiting::Connect) {
            self.waiting = None;
        }
        self.sync_room(cx);
        cx.notify();
    }

    /// Something waits for you (§5.2): with the thread folded it floats up
    /// as a bubble, and the pebble gets its dot.
    fn needs_you(&mut self, waiting: Waiting, cx: &mut Context<Self>) {
        self.waiting = Some(waiting);
        if !self.thread_open {
            self.bubble_up = true;
        }
        cx.notify();
    }

    /// Nothing waits any more: a turn started, or you answered.
    fn settled(&mut self, cx: &mut Context<Self>) {
        self.waiting = None;
        self.bubble_up = false;
        cx.notify();
    }

    /// Asks the page for the thread's room (§5.3). Deferred: the workspace
    /// may be mid-update when a chat action reaches it.
    fn sync_room(&self, cx: &mut Context<Self>) {
        let fits = self.centre_bounds.size.width - px(INSET) >= px(MIN_NOTE_WIDTH);
        let inset = (self.thread_open && fits).then_some(px(INSET));
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    if workspace.center_inset() != inset {
                        workspace.set_center_inset(inset, cx);
                    }
                })
                .log_err();
        });
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
        self.publish_entitlement(cx);
        cx.notify();
    }

    /// Shares the entitlement with the surfaces that ride the same grant
    /// (V41 §7.3) whenever the connection changes.
    fn publish_entitlement(&self, cx: &mut App) {
        plus::set_current_entitlement(self.entitlement().cloned(), cx);
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
    /// it survives restarts.
    fn note_session_for_memory_nudge(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault() else {
            return;
        };
        let root = vault.root.clone();
        let nudge_after = vault.config.memory.nudge_after_sessions;
        let fs = self.project.read(cx).fs().clone();
        cx.spawn(async move |this, cx| {
            let due = cx
                .background_spawn(async move {
                    crate::memory::note_session_started(&fs, &root, nudge_after).await
                })
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
        let fs = self.project.read(cx).fs().clone();
        cx.background_spawn(async move { crate::memory::dismiss_nudge(&fs, &root).await })
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
            kickoff: request.is_ritual(),
            _subscriptions: vec![subscription],
        });
        if let Some(kickoff) = request.kickoff {
            self.send_text(kickoff, cx);
        }
        if self.thread_open {
            self.focus_thread(window, cx);
        }
        cx.notify();
    }

    /// Drops everything keyed to a session's transcript when it goes away.
    fn reset_transcript_state(&mut self) {
        self.bubbles.clear();
        self.turn_times.clear();
        self.selected = None;
        self.expanded_receipts.clear();
        self.elicitation_choices.clear();
        self.elicitation_editors.clear();
        self.ask_choices.clear();
        self.ask_answers.clear();
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
                if is_ask_request(id) {
                    self.prepare_ask(thread, id, window, cx);
                } else {
                    self.authorize_tool_call(thread, id, cx);
                }
            }
            AcpThreadEvent::NewEntry | AcpThreadEvent::EntryUpdated(_) => {
                self.sync_bubbles(thread, cx);
                if thread.read(cx).status() == ThreadStatus::Generating {
                    self.scroll_handle.scroll_to_bottom();
                }
            }
            AcpThreadEvent::ElicitationRequested(id) => {
                self.prepare_elicitation(thread, id, window, cx);
            }
            AcpThreadEvent::Stopped(_) => {
                self.refresh_entitlement(cx);
                // A reply that lands while the thread is folded floats up
                // (§5.2); a turn that stopped on a question already has.
                let replied = matches!(
                    self.items(cx).last(),
                    Some(ChatItem::Reply(_) | ChatItem::Receipt { .. })
                );
                if replied && !self.thread_open && self.waiting.is_none() {
                    self.needs_you(Waiting::Reply, cx);
                }
            }
            // V26 decision 8 has a floor (§5.5): failures never get error
            // styling or raw detail, but a turn that could not finish says
            // so in plain language. The mechanics go to the log.
            AcpThreadEvent::Error => {
                self.turn_failed("The Thock Agent couldn't finish that.", true, cx);
            }
            AcpThreadEvent::LoadError(error) => {
                log::error!("Thock: the agent session failed to load: {error}");
                self.turn_failed("The Thock Agent couldn't finish that.", true, cx);
            }
            AcpThreadEvent::Refusal => {
                self.turn_failed("The Thock Agent decided not to do that.", false, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn turn_failed(&mut self, message: &'static str, retryable: bool, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session {
            session.turn_failure = Some(TurnFailure {
                message: message.into(),
                retryable,
            });
        }
        if !self.thread_open {
            self.needs_you(Waiting::Failure, cx);
        }
    }

    /// A question arrived (§5.5): it is selected and, when the chat has
    /// focus, the list takes it so the chips' keys work. Otherwise it floats
    /// up and focus stays where it is (§5.2).
    fn question_arrived(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.focus_handle, cx);
        }
        self.needs_you(Waiting::Question, cx);
        self.scroll_handle.scroll_to_bottom();
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
            }
            ElicitationShape::Text { .. } => {
                let editor = cx.new(|cx| {
                    let mut editor = Editor::single_line(window, cx);
                    editor.set_placeholder_text("Type your answer…", window, cx);
                    editor
                });
                if self.focus_handle.contains_focused(window, cx) {
                    window.focus(&editor.focus_handle(cx), cx);
                }
                self.elicitation_editors.insert(id.clone(), editor);
            }
            ElicitationShape::Link { .. } | ElicitationShape::Confirm => {}
        }
        self.question_arrived(window, cx);
    }

    /// An `ask` request blocks the turn like an elicitation does, so it
    /// arrives selected with its first chip highlighted and the list
    /// focused; it is never answered on the person's behalf. A request
    /// without flat options can't be shown as chips, so it falls back to
    /// decision 16.
    fn prepare_ask(
        &mut self,
        thread: &Entity<AcpThread>,
        id: &acp::ToolCallId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry_index = thread
            .read(cx)
            .tool_call(id)
            .and_then(|(index, tool_call)| {
                let ToolCallStatus::WaitingForConfirmation { options, .. } = &tool_call.status
                else {
                    return None;
                };
                (!ask_options(options).is_empty()).then_some(index)
            });
        let Some(entry_index) = entry_index else {
            self.authorize_tool_call(thread, id, cx);
            return;
        };
        self.selected = Some(ItemKey::Entry(entry_index));
        self.ask_choices.insert(id.clone(), 0);
        self.question_arrived(window, cx);
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

    /// Keeps a bubble per paragraph of every reply (§5.4), rebuilt only when
    /// a reply's text changed since the last look.
    fn sync_bubbles(&mut self, thread: &Entity<AcpThread>, cx: &mut Context<Self>) {
        let languages = self.project.read(cx).languages().clone();
        let replies: Vec<(usize, String)> = thread
            .read(cx)
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                AgentThreadEntry::AssistantMessage(message) => {
                    Some((index, reply_text(message, cx)))
                }
                _ => None,
            })
            .collect();
        for (index, source) in replies {
            if self
                .bubbles
                .get(&index)
                .is_some_and(|bubbles| bubbles.source == source)
            {
                continue;
            }
            let paragraphs = paragraphs(&source)
                .into_iter()
                .map(|paragraph| {
                    cx.new(|cx| Markdown::new(paragraph.into(), Some(languages.clone()), None, cx))
                })
                .collect();
            self.bubbles
                .insert(index, ReplyBubbles { source, paragraphs });
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
        let turn_start = session.thread.read(cx).entries().len();
        let turn = session
            .thread
            .update(cx, |thread, cx| thread.send(vec![text.as_str().into()], cx));
        self.turn_times.insert(turn_start, Local::now());
        self.settled(cx);
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
        self.open_thread(window, cx);
        self.launch(LaunchRequest::conversation(), window, cx);
    }

    fn focus_input(&mut self, _: &FocusChatInput, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.message_editor.focus_handle(cx), cx);
    }

    /// `shift-tab` from the composer: the selection lands on the last row.
    fn focus_thread_list(
        &mut self,
        _: &FocusChatThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.select_last(&menu::SelectLast, window, cx);
    }

    /// `.`: opens or closes the selected receipt.
    fn toggle_activity(&mut self, _: &ToggleChatActivity, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ItemKey::Receipt(selected_id)) = self.selected.clone() else {
            return;
        };
        let items = self.items(cx);
        let key = items.iter().find_map(|item| match item {
            ChatItem::Receipt { key, calls }
                if calls.iter().any(|(_, call_id)| *call_id == selected_id) =>
            {
                Some(key.clone())
            }
            _ => None,
        });
        let Some(key) = key else {
            return;
        };
        if !self.expanded_receipts.remove(&key) {
            self.expanded_receipts.insert(key);
        }
        cx.notify();
    }

    /// The thread as you see it (V44 §5.4), folded fresh from the thread on
    /// every use so it can never desynchronize.
    fn items(&self, cx: &App) -> Vec<ChatItem> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let thread = session.thread.read(cx);
        let entries = thread.entries();
        let mut items = build_items(
            entries.iter().map(|entry| entry_shape(entry, thread)),
            session.kickoff,
            thread.status() == ThreadStatus::Generating,
        );
        // An open receipt shows its rows, reads before changes (§5.4).
        let mut position = 0;
        while position < items.len() {
            if let Some(ChatItem::Receipt { key, calls }) = items.get(position)
                && self.expanded_receipts.contains(key)
            {
                let receipt = key.clone();
                let mut calls = calls.clone();
                calls.sort_by_key(|(index, _)| is_change(entries.get(*index)));
                let rows: Vec<ChatItem> = calls
                    .into_iter()
                    .map(|(index, id)| ChatItem::Row {
                        receipt: receipt.clone(),
                        index,
                        id,
                    })
                    .collect();
                position += rows.len();
                items.splice(position + 1 - rows.len()..position + 1 - rows.len(), rows);
            }
            position += 1;
        }
        items
    }

    /// What the thread says, one row per line, for the tests that check
    /// nothing mechanical leaks (G7).
    #[cfg(test)]
    fn visible_text(&self, cx: &App) -> String {
        let Some(session) = &self.session else {
            return String::new();
        };
        let thread = session.thread.read(cx);
        let entries = thread.entries();
        let vault_root = self.vault().map(|vault| vault.root.clone());
        let entry_text = |index: usize| -> String {
            match entries.get(index) {
                Some(AgentThreadEntry::UserMessage(message)) => message
                    .content
                    .text_content(cx)
                    .unwrap_or_default()
                    .to_string(),
                Some(AgentThreadEntry::AssistantMessage(message)) => reply_text(message, cx),
                Some(AgentThreadEntry::Elicitation(id)) => thread
                    .elicitation(id)
                    .map(|(_, elicitation)| elicitation.request.message.clone())
                    .unwrap_or_default(),
                Some(AgentThreadEntry::ToolCall(tool_call)) => {
                    match self.ask_answers.get(&tool_call.id) {
                        Some(answer) => answer.to_string(),
                        None => tool_call.label.read(cx).source().to_string(),
                    }
                }
                _ => String::new(),
            }
        };
        self.items(cx)
            .iter()
            .map(|item| match item {
                ChatItem::Kickoff(_) => format!("You started {}", session.title),
                ChatItem::User(index) | ChatItem::Reply(index) | ChatItem::Question(index) => {
                    entry_text(*index)
                }
                ChatItem::Receipt { calls, .. } => {
                    summarize_activity(&summary_calls(calls, entries, vault_root.as_deref()), false)
                }
                ChatItem::Row { index, .. } => match entries.get(*index) {
                    Some(AgentThreadEntry::ToolCall(tool_call)) => {
                        let (verb, _) = row_verb(&tool_call.kind);
                        let note = tool_call
                            .locations
                            .first()
                            .map(|location| {
                                vault_relative_label(&location.path, vault_root.as_deref())
                            })
                            .unwrap_or_default();
                        format!("  {verb} {note}").trim_end().to_string()
                    }
                    _ => String::new(),
                },
                ChatItem::Typing { calls } => {
                    summarize_activity(&summary_calls(calls, entries, vault_root.as_deref()), true)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The chips of the newest pending question, for the tests.
    #[cfg(test)]
    fn visible_chips(&self, cx: &App) -> Vec<String> {
        self.latest_pending_question(cx)
            .and_then(|question| self.question_choices(&question, cx))
            .map(|(choices, _)| choices.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    /// Where the selection sits in `items`. A receipt matches when it
    /// *contains* the selected call, not only when it starts with it, so a
    /// run that merges or splits mid-turn keeps the selection (V26 R4).
    fn selected_position(&self, items: &[ChatItem]) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        items.iter().position(|item| match (item, selected) {
            (ChatItem::Receipt { calls, .. }, ItemKey::Receipt(id)) => {
                calls.iter().any(|(_, call_id)| call_id == id)
            }
            (ChatItem::Row { id, .. }, ItemKey::Call(selected_id)) => id == selected_id,
            (item, ItemKey::Entry(selected_index)) => {
                item.key() == Some(ItemKey::Entry(*selected_index))
            }
            _ => false,
        })
    }

    /// The rows the keys can land on; the typing bubble is skipped.
    fn selectable_positions(items: &[ChatItem]) -> Vec<usize> {
        items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.key().is_some())
            .map(|(position, _)| position)
            .collect()
    }

    fn select_position(&mut self, items: &[ChatItem], position: usize, cx: &mut Context<Self>) {
        let Some(key) = items.get(position).and_then(ChatItem::key) else {
            return;
        };
        self.selected = Some(key);
        self.scroll_handle.scroll_to_item(position);
        cx.notify();
    }

    fn select_by_offset(&mut self, offset: isize, cx: &mut Context<Self>) {
        let items = self.items(cx);
        let positions = Self::selectable_positions(&items);
        if positions.is_empty() {
            return;
        }
        let current = self
            .selected_position(&items)
            .and_then(|position| positions.iter().position(|p| *p == position));
        let next = match current {
            Some(index) => {
                (index as isize + offset).clamp(0, positions.len() as isize - 1) as usize
            }
            None if offset > 0 => 0,
            None => positions.len() - 1,
        };
        if let Some(position) = positions.get(next) {
            self.select_position(&items, *position, cx);
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select_by_offset(1, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_by_offset(-1, cx);
    }

    fn select_first(&mut self, _: &menu::SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        let items = self.items(cx);
        if let Some(position) = Self::selectable_positions(&items).first() {
            self.select_position(&items, *position, cx);
        }
    }

    fn select_last(&mut self, _: &menu::SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        let items = self.items(cx);
        if let Some(position) = Self::selectable_positions(&items).last() {
            self.select_position(&items, *position, cx);
        }
    }

    /// The question under the selection, if it is still waiting for an
    /// answer.
    fn selected_pending_question(&self, cx: &App) -> Option<PendingQuestion> {
        let Some(ItemKey::Entry(index)) = &self.selected else {
            return None;
        };
        self.pending_question_at(*index, cx)
    }

    /// The newest question still waiting for an answer, which the number
    /// keys answer wherever the selection sits.
    fn latest_pending_question(&self, cx: &App) -> Option<PendingQuestion> {
        let session = self.session.as_ref()?;
        let count = session.thread.read(cx).entries().len();
        (0..count)
            .rev()
            .find_map(|index| self.pending_question_at(index, cx))
    }

    fn pending_question_at(&self, index: usize, cx: &App) -> Option<PendingQuestion> {
        let session = self.session.as_ref()?;
        let thread = session.thread.read(cx);
        match thread.entries().get(index)? {
            AgentThreadEntry::Elicitation(id) => {
                let (_, elicitation) = thread.elicitation(id)?;
                matches!(elicitation.status, ElicitationStatus::Pending { .. })
                    .then(|| PendingQuestion::Elicitation(id.clone()))
            }
            AgentThreadEntry::ToolCall(tool_call) if is_ask_request(&tool_call.id) => matches!(
                tool_call.status,
                ToolCallStatus::WaitingForConfirmation { .. }
            )
            .then(|| PendingQuestion::Ask(tool_call.id.clone())),
            _ => None,
        }
    }

    /// The chips of a pending question, in order, with the highlighted one.
    /// `None` for a question answered by typing or a click rather than a
    /// chip.
    fn question_choices(
        &self,
        question: &PendingQuestion,
        cx: &App,
    ) -> Option<(Vec<SharedString>, usize)> {
        let session = self.session.as_ref()?;
        let thread = session.thread.read(cx);
        match question {
            PendingQuestion::Elicitation(id) => {
                let (_, elicitation) = thread.elicitation(id)?;
                let ElicitationShape::Choice { options, .. } =
                    elicitation_shape(&elicitation.request)
                else {
                    return None;
                };
                let chosen = self.elicitation_choices.get(id).copied().unwrap_or(0);
                Some((
                    options.into_iter().map(|(_, title)| title.into()).collect(),
                    chosen,
                ))
            }
            PendingQuestion::Ask(id) => {
                let (_, tool_call) = thread.tool_call(id)?;
                let ToolCallStatus::WaitingForConfirmation { options, .. } = &tool_call.status
                else {
                    return None;
                };
                let chosen = self.ask_choices.get(id).copied().unwrap_or(0);
                Some((
                    ask_options(options)
                        .into_iter()
                        .map(|option| option.name.clone().into())
                        .collect(),
                    chosen,
                ))
            }
        }
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
        if let Some(question) = self.selected_pending_question(cx) {
            self.cycle_choice(&question, direction, cx);
            return;
        }
        // `left` on a row closes its receipt and lands on it.
        if let Some(ItemKey::Call(call_id)) = self.selected.clone() {
            if direction < 0 {
                let items = self.items(cx);
                let receipt = items.iter().find_map(|item| match item {
                    ChatItem::Row { receipt, id, .. } if *id == call_id => Some(receipt.clone()),
                    _ => None,
                });
                if let Some(receipt) = receipt {
                    self.expanded_receipts.remove(&receipt);
                    self.selected = Some(ItemKey::Receipt(receipt));
                    cx.notify();
                }
            }
            return;
        }
        let Some(ItemKey::Receipt(selected_id)) = self.selected.clone() else {
            return;
        };
        // Normalize to the receipt's key: the selected id may be a mid-run
        // call after runs merged.
        let items = self.items(cx);
        let key = items.iter().find_map(|item| match item {
            ChatItem::Receipt { key, calls }
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
            self.expanded_receipts.insert(key);
        } else {
            self.expanded_receipts.remove(&key);
        }
        cx.notify();
    }

    /// `left`/`right` move the highlighted chip of a pending question.
    fn cycle_choice(&mut self, question: &PendingQuestion, direction: i32, cx: &mut Context<Self>) {
        let Some((choices, current)) = self.question_choices(question, cx) else {
            return;
        };
        if choices.is_empty() {
            return;
        }
        let next = if direction > 0 {
            (current + 1).min(choices.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        match question {
            PendingQuestion::Elicitation(id) => {
                self.elicitation_choices.insert(id.clone(), next);
            }
            PendingQuestion::Ask(id) => {
                self.ask_choices.insert(id.clone(), next);
            }
        }
        cx.notify();
    }

    /// Answers a question. The answer starts the next turn, so its time is
    /// kept for the day marks, and a bubble that was answered folds, with
    /// focus going back to where it was (§5.5).
    fn answer_question(
        &mut self,
        question: &PendingQuestion,
        choice: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let turn_start = self.session.as_ref().and_then(|session| {
            let thread = session.thread.read(cx);
            match question {
                PendingQuestion::Elicitation(id) => thread.elicitation(id).map(|(index, _)| index),
                PendingQuestion::Ask(id) => thread.tool_call(id).map(|(index, _)| index),
            }
        });
        match question {
            PendingQuestion::Elicitation(id) => self.answer_elicitation(id, choice, cx),
            PendingQuestion::Ask(id) => self.answer_ask(id, choice, cx),
        }
        if let Some(turn_start) = turn_start {
            self.turn_times.insert(turn_start, Local::now());
        }
        self.settled(cx);
        if !self.thread_open {
            self.fold(window, cx);
        }
    }

    /// Answers an `ask` request by authorizing the chosen option, which is
    /// what pi-acp hands back to the tool as the person's pick (§5.5).
    fn answer_ask(&mut self, id: &acp::ToolCallId, choice: Option<usize>, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let thread = session.thread.clone();
        let index = choice
            .or_else(|| self.ask_choices.get(id).copied())
            .unwrap_or(0);
        let chosen = thread.read(cx).tool_call(id).and_then(|(_, tool_call)| {
            let ToolCallStatus::WaitingForConfirmation { options, .. } = &tool_call.status else {
                return None;
            };
            ask_options(options).get(index).map(|option| {
                (
                    option.option_id.clone(),
                    option.kind,
                    SharedString::from(option.name.clone()),
                )
            })
        });
        let Some((option_id, kind, name)) = chosen else {
            return;
        };
        self.ask_answers.insert(id.clone(), name);
        self.ask_choices.remove(id);
        thread.update(cx, |thread, cx| {
            thread.authorize_tool_call(
                id.clone(),
                SelectedPermissionOutcome::new(option_id, kind),
                cx,
            );
        });
        cx.notify();
    }

    /// `1`–`9`, or `y`/`n` for a pair: answers the selected question, or
    /// the newest one waiting, with that chip.
    fn answer_choice(
        &mut self,
        action: &AnswerChatChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(question) = self
            .selected_pending_question(cx)
            .or_else(|| self.latest_pending_question(cx))
        else {
            if self.waiting == Some(Waiting::Connect) {
                self.pick_connect_choice(action.0, window, cx);
            }
            return;
        };
        let Some((choices, _)) = self.question_choices(&question, cx) else {
            return;
        };
        if action.0 < choices.len() {
            self.answer_question(&question, Some(action.0), window, cx);
        }
    }

    /// The two ways in when Thock Plus isn't connected (§5.11).
    fn connect_choices() -> [SharedString; 2] {
        ["Connect Thock Plus".into(), "Use your own agent".into()]
    }

    fn pick_connect_choice(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match index {
            0 => self.open_connect(window, cx),
            1 => {
                self.set_mode(ConnectionMode::Byo, cx);
                self.fold(window, cx);
            }
            _ => {}
        }
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
                if let Some(question) = self.pending_question_for_confirm(window, cx) {
                    self.answer_question(&question, None, window, cx);
                } else if self.message_editor.focus_handle(cx).is_focused(window) {
                    self.send_message(&SendChatMessage, window, cx);
                } else if !self.open_selected_entry(window, cx) {
                    window.focus(&self.message_editor.focus_handle(cx), cx);
                }
            }
        }
    }

    /// The question `enter` should answer: the focused answer box, or a
    /// selected pending question.
    fn pending_question_for_confirm(&self, window: &Window, cx: &App) -> Option<PendingQuestion> {
        for (id, editor) in self.elicitation_editors.iter() {
            if editor.focus_handle(cx).is_focused(window) {
                return Some(PendingQuestion::Elicitation(id.clone()));
            }
        }
        if self.message_editor.focus_handle(cx).is_focused(window) {
            return None;
        }
        self.selected_pending_question(cx)
    }

    /// `escape` (§5.12): back to the page. The connect flow closes with it,
    /// and a running turn keeps running (`StopChatTurn` is `cmd-.`).
    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.view, PanelView::Connect(_)) {
            self.cancel_connect(cx);
        }
        self.fold(window, cx);
    }

    fn open_entry(&mut self, _: &OpenChatEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.open_selected_entry(window, cx);
    }

    /// Opens the note the selected row or receipt names: a row's note at its
    /// line, or a receipt's first note a write touched, failing that the
    /// first note the turn touched at all. Returns whether there was one.
    fn open_selected_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        if let Some(ItemKey::Call(call_id)) = self.selected.clone() {
            let location = session
                .thread
                .read(cx)
                .tool_call(&call_id)
                .and_then(|(_, tool_call)| tool_call.locations.first().cloned());
            let Some(location) = location else {
                return false;
            };
            self.open_note(location.path, location.line, window, cx);
            return true;
        }
        let Some(ItemKey::Receipt(selected_id)) = self.selected.clone() else {
            return false;
        };
        let items = self.items(cx);
        let calls = items.iter().find_map(|item| match item {
            ChatItem::Receipt { calls, .. }
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
        self.open_note(path, None, window, cx);
        true
    }

    /// Opens a note, at `line` (1-based) when there is one. Deferred: the
    /// workspace may be mid-update when a panel action reaches it.
    fn open_note(
        &self,
        path: PathBuf,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let item = workspace
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
            if let (Some(line), Some(editor)) = (line, item.downcast::<Editor>()) {
                editor.update_in(cx, |editor, window, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let point =
                        snapshot.clip_point(Point::new(line.saturating_sub(1), 0), Bias::Left);
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()).nav_history(true),
                        window,
                        cx,
                        |selections| selections.select_ranges([point..point]),
                    );
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
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
        // With the thread folded the flow floats up as a bubble (§5.11).
        if self.thread_open {
            window.focus(&code_editor.focus_handle(cx), cx);
        } else {
            self.needs_you(Waiting::Connect, cx);
            self.focus_bubble(window, cx);
        }
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
                this.publish_entitlement(cx);
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
        self.publish_entitlement(cx);
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

    /// Thock's bubbles are set in Petrona (§5.7). Links read as plain prose,
    /// because the page shows where they point (§5.6).
    fn thock_markdown_style(&self, window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        style.base_text_style.font_family = THOCK_FACE.into();
        style.base_text_style.font_size = rems_from_px(15.).into();
        style.base_text_style.line_height = relative(1.42);
        style.inline_code.font_size = Some(rems_from_px(12.5).into());
        style.code_block.text.font_size = Some(rems_from_px(12.).into());
        style.link = TextStyleRefinement {
            color: Some(cx.theme().colors().text),
            ..Default::default()
        };
        style
    }

    /// Your bubbles are set in the UI face (§5.7).
    fn you_markdown_style(&self, window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        style.base_text_style.font_size = rems_from_px(14.).into();
        style.base_text_style.line_height = relative(1.45);
        style.inline_code.font_size = Some(rems_from_px(12.).into());
        style.code_block.text.font_size = Some(rems_from_px(12.).into());
        style
    }

    fn render_item(
        &self,
        position: usize,
        item: &ChatItem,
        entries: &[AgentThreadEntry],
        selected: bool,
        vault_root: Option<&Path>,
        day_marks: &BTreeMap<usize, String>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        // A row whose entry isn't the kind its item promises (a stale index
        // mid-stream) renders nothing rather than panicking.
        let body: AnyElement = match item {
            ChatItem::Kickoff(_) => self.render_kickoff(cx),
            ChatItem::User(index) => match entries.get(*index) {
                Some(AgentThreadEntry::UserMessage(message)) => {
                    self.render_user_message(message, window, cx)
                }
                _ => div().into_any_element(),
            },
            ChatItem::Reply(index) => match entries.get(*index) {
                Some(AgentThreadEntry::AssistantMessage(message)) => {
                    self.render_reply(*index, message, window, cx)
                }
                _ => div().into_any_element(),
            },
            ChatItem::Question(index) => match entries.get(*index) {
                Some(AgentThreadEntry::Elicitation(id)) => self.render_elicitation(id, cx),
                Some(AgentThreadEntry::ToolCall(tool_call)) => self.render_ask(tool_call, cx),
                _ => div().into_any_element(),
            },
            ChatItem::Receipt { key, calls } => {
                self.render_receipt(key, calls, entries, vault_root, cx)
            }
            ChatItem::Row { index, .. } => self.render_row(*index, entries, vault_root, cx),
            ChatItem::Typing { calls } => self.render_typing(calls, entries, vault_root, cx),
        };
        // Bubbles of one side sit 3px apart, turns 10px (§5.4).
        let space_above = match item {
            ChatItem::Row { .. } => px(2.),
            ChatItem::Receipt { .. } => px(3.),
            _ => px(10.),
        };
        let day_mark = match item {
            ChatItem::Kickoff(index) | ChatItem::User(index) => day_marks.get(index).cloned(),
            _ => None,
        };
        let item_key = item.key();
        v_flex()
            .id(("thock-chat-item", position))
            .w_full()
            .pt(space_above)
            .children(day_mark.map(|label| self.render_day_mark(label, cx)))
            .child(
                div()
                    .w_full()
                    .pl_1()
                    .border_l_2()
                    .border_color(if selected {
                        cx.theme().colors().border_focused
                    } else {
                        gpui::transparent_black()
                    })
                    .child(body),
            )
            .when_some(item_key, |this, item_key| {
                this.on_click(cx.listener(move |this, _, _window, cx| {
                    this.selected = Some(item_key.clone());
                    cx.notify();
                }))
            })
            .into_any_element()
    }

    /// `TODAY · 17:42`, centred above a turn (§5.4).
    fn render_day_mark(&self, label: String, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .justify_center()
            .pb_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(rems_from_px(11.))
                    .line_height(rems_from_px(14.))
                    .font_features(tabular_numerals())
                    .text_color(cx.theme().colors().text_placeholder)
                    .child(label),
            )
            .into_any_element()
    }

    /// The marker a ritual's kickoff leaves (decision 7): the prompt went to
    /// the agent unchanged, but it isn't something you typed.
    fn render_kickoff(&self, cx: &Context<Self>) -> AnyElement {
        let title = self
            .session
            .as_ref()
            .map(|session| session.title.clone())
            .unwrap_or_default();
        h_flex()
            .w_full()
            .justify_center()
            .gap_1()
            .text_size(rems_from_px(12.))
            .line_height(rems_from_px(16.))
            .text_color(cx.theme().colors().text_muted)
            .child("▸ You started")
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .into_any_element()
    }

    /// The typing bubble: the turn is still running, and its calls so far
    /// give the live line while the prose waits for the stop (decision 6).
    fn render_typing(
        &self,
        calls: &[(usize, acp::ToolCallId)],
        entries: &[AgentThreadEntry],
        vault_root: Option<&Path>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let summary = summarize_activity(&summary_calls(calls, entries, vault_root), true);
        h_flex()
            .gap_2()
            .items_center()
            .child(SpinnerLabel::new().size(LabelSize::Small))
            .child(self.status_line(summary, cx))
            .into_any_element()
    }

    /// One line in Thock's voice but not its weight: the live line, or the
    /// settled form of a question (`chat.status`).
    fn status_line(&self, text: impl Into<SharedString>, cx: &Context<Self>) -> Div {
        div()
            .font_family(THOCK_FACE)
            .font_weight(FontWeight::MEDIUM)
            .italic()
            .text_size(rems_from_px(12.))
            .line_height(rems_from_px(17.))
            .text_color(cx.theme().colors().text_muted)
            .child(text.into())
    }

    fn render_user_message(
        &self,
        message: &acp_thread::UserMessage,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let content: AnyElement = match message.content.markdown() {
            Some(markdown) => {
                MarkdownElement::new(markdown.clone(), self.you_markdown_style(window, cx))
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
        self.render_you(content, cx)
    }

    /// Your bubble (§5.7): the selection surface, on the right, with the
    /// corner nearest you tightened.
    fn render_you(&self, content: AnyElement, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .justify_end()
            .child(
                div()
                    .max_w(relative(0.85))
                    .px_3()
                    .py_2()
                    .rounded(px(18.))
                    .rounded_br(px(7.))
                    .bg(cx.theme().colors().element_selected)
                    .text_size(rems_from_px(14.))
                    .line_height(relative(1.45))
                    .child(content),
            )
            .into_any_element()
    }

    /// Chips for a choice (§5.5): a keycap, then the label. The highlighted
    /// one is what `enter` picks; `pick` answers with the chip's index.
    fn render_chips(
        &self,
        choices: &[SharedString],
        chosen: usize,
        pick: impl Fn(&mut Self, usize, &mut Window, &mut Context<Self>) + Clone + 'static,
        cx: &Context<Self>,
    ) -> AnyElement {
        let keycaps: Vec<Option<String>> = if choices.len() == 2 {
            vec![Some("y".to_string()), Some("n".to_string())]
        } else {
            (1..=choices.len())
                .map(|number| (number <= 9).then(|| number.to_string()))
                .collect()
        };
        let mut row = h_flex().flex_wrap().gap_1p5();
        for (index, label) in choices.iter().enumerate() {
            let pick = pick.clone();
            let is_chosen = index == chosen;
            row = row.child(
                h_flex()
                    .id(("thock-chat-chip", index))
                    .gap_1p5()
                    .items_center()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(if is_chosen {
                        cx.theme().colors().border_focused
                    } else {
                        gpui::transparent_black()
                    })
                    .bg(cx.theme().colors().element_selected)
                    .cursor_pointer()
                    .children(
                        keycaps
                            .get(index)
                            .cloned()
                            .flatten()
                            .map(|key| keycap(key, cx)),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(rems_from_px(12.5))
                            .line_height(rems_from_px(18.))
                            .child(label.clone()),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| pick(this, index, window, cx)),
                    ),
            );
        }
        row.into_any_element()
    }

    /// A question from the `ask` tool (§5.5): the question, then one chip
    /// per option. Once answered, the chip you picked is your bubble.
    fn render_ask(&self, tool_call: &ToolCall, cx: &Context<Self>) -> AnyElement {
        let question = tool_call.label.read(cx).source().clone();
        let ToolCallStatus::WaitingForConfirmation { options, .. } = &tool_call.status else {
            return match self.ask_answers.get(&tool_call.id) {
                Some(answer) => self.render_you(Label::new(answer.clone()).into_any_element(), cx),
                None => self.status_line(question, cx).into_any_element(),
            };
        };
        let chosen = self.ask_choices.get(&tool_call.id).copied().unwrap_or(0);
        let choices: Vec<SharedString> = ask_options(options)
            .into_iter()
            .map(|option| option.name.clone().into())
            .collect();
        let id = tool_call.id.clone();
        v_flex()
            .w_full()
            .items_start()
            .gap_2()
            .child(self.thock_bubble(div().child(question).into_any_element(), cx))
            .child(self.render_chips(
                &choices,
                chosen,
                move |this, index, window, cx| {
                    this.answer_question(&PendingQuestion::Ask(id.clone()), Some(index), window, cx)
                },
                cx,
            ))
            .into_any_element()
    }

    /// One of Thock's bubbles (§5.7): ink at 8% over the page, on the left,
    /// with the corner nearest Thock tightened.
    fn thock_bubble(&self, content: AnyElement, cx: &Context<Self>) -> Div {
        div()
            .max_w(relative(0.92))
            .px_3()
            .py_2()
            .rounded(px(18.))
            .rounded_bl(px(7.))
            .bg(cx.theme().colors().text.opacity(0.08))
            .font_family(THOCK_FACE)
            .text_size(rems_from_px(15.))
            .line_height(relative(1.42))
            .child(content)
    }

    /// Thock's reply, one bubble per paragraph (§5.4). Until the bubbles
    /// are built the whole message is one bubble.
    fn render_reply(
        &self,
        index: usize,
        message: &acp_thread::AssistantMessage,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let style = self.thock_markdown_style(window, cx);
        let mut column = v_flex().w_full().items_start().gap(px(3.));
        match self.bubbles.get(&index) {
            Some(bubbles) => {
                for markdown in &bubbles.paragraphs {
                    column = column.child(self.thock_bubble(
                        MarkdownElement::new(markdown.clone(), style.clone()).into_any_element(),
                        cx,
                    ));
                }
            }
            None => {
                for chunk in &message.chunks {
                    if let AssistantMessageChunk::Message { block, .. } = chunk
                        && let Some(markdown) = block.markdown()
                    {
                        column = column.child(
                            self.thock_bubble(
                                MarkdownElement::new(markdown.clone(), style.clone())
                                    .into_any_element(),
                                cx,
                            ),
                        );
                    }
                }
            }
        }
        column.into_any_element()
    }

    /// One quiet line for everything a turn did (decision 5), under the
    /// reply; `.`, `right` or a click opens its rows in place.
    fn render_receipt(
        &self,
        key: &acp::ToolCallId,
        calls: &[(usize, acp::ToolCallId)],
        entries: &[AgentThreadEntry],
        vault_root: Option<&Path>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let summary = summarize_activity(&summary_calls(calls, entries, vault_root), false);
        let expanded = self.expanded_receipts.contains(key);
        let toggle_key = key.clone();
        h_flex()
            .id(SharedString::from(format!("thock-chat-receipt-{key}")))
            .gap_1()
            .cursor_pointer()
            .text_size(rems_from_px(11.5))
            .line_height(rems_from_px(16.))
            .text_color(cx.theme().colors().text_muted)
            .hover(|this| this.text_color(cx.theme().colors().text))
            .child(summary)
            .child(if expanded { "⌄" } else { "›" })
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.selected = Some(ItemKey::Receipt(toggle_key.clone()));
                if !this.expanded_receipts.remove(&toggle_key) {
                    this.expanded_receipts.insert(toggle_key.clone());
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// One row of an open receipt (decision 14): the verb, amber when it
    /// changed a note, then the vault-relative path in the mono face.
    /// Failed calls render as ordinary rows, with no error text or styling.
    fn render_row(
        &self,
        index: usize,
        entries: &[AgentThreadEntry],
        vault_root: Option<&Path>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(AgentThreadEntry::ToolCall(tool_call)) = entries.get(index) else {
            return div().into_any_element();
        };
        let (verb, change) = row_verb(&tool_call.kind);
        let location = tool_call.locations.first().cloned();
        let detail: Option<SharedString> = match &location {
            Some(location) => Some(vault_relative_label(&location.path, vault_root).into()),
            None => {
                let label = tool_call.label.read(cx).source().clone();
                let is_tool_name = tool_call
                    .tool_name
                    .as_ref()
                    .is_some_and(|name| name.as_ref() == label.as_ref());
                (!label.is_empty() && !is_tool_name).then_some(label)
            }
        };
        h_flex()
            .id(SharedString::from(format!(
                "thock-chat-row-{}",
                tool_call.id
            )))
            .w_full()
            .pl_4()
            .gap_2()
            .items_baseline()
            .font_family(".ZedMono")
            .text_size(rems_from_px(11.))
            .line_height(rems_from_px(16.))
            .text_color(cx.theme().colors().text_muted)
            .child(
                div()
                    .w(px(64.))
                    .flex_shrink_0()
                    .when(change, |this| {
                        this.text_color(cx.theme().colors().text_accent)
                    })
                    .child(verb),
            )
            .children(detail.map(|detail| div().flex_1().min_w_0().truncate().child(detail)))
            .when_some(location, |this, location| {
                this.cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_note(location.path.clone(), location.line, window, cx);
                    }))
            })
            .into_any_element()
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
            return self.status_line(message, cx).into_any_element();
        }
        let mut column = v_flex()
            .w_full()
            .items_start()
            .gap_2()
            .child(self.thock_bubble(div().child(message).into_any_element(), cx));
        match elicitation_shape(&elicitation.request) {
            ElicitationShape::Choice { options, .. } => {
                let chosen = self.elicitation_choices.get(id).copied().unwrap_or(0);
                let titles: Vec<SharedString> =
                    options.into_iter().map(|(_, title)| title.into()).collect();
                let answer_id = id.clone();
                column = column.child(self.render_chips(
                    &titles,
                    chosen,
                    move |this, index, window, cx| {
                        this.answer_question(
                            &PendingQuestion::Elicitation(answer_id.clone()),
                            Some(index),
                            window,
                            cx,
                        )
                    },
                    cx,
                ));
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
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.answer_question(
                                        &PendingQuestion::Elicitation(accept_id.clone()),
                                        None,
                                        window,
                                        cx,
                                    );
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

    /// The rows of the thread from `from` on, the way the thread shows them.
    fn render_items(&self, from: usize, window: &Window, cx: &Context<Self>) -> Vec<AnyElement> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let entries = session.thread.read(cx).entries();
        let items = self.items(cx);
        let selected_position = self.selected_position(&items);
        let vault_root = self.vault().map(|vault| vault.root.clone());
        let day_marks = day_marks(&self.turn_times, Local::now().date_naive());
        items
            .iter()
            .enumerate()
            .skip(from)
            .map(|(position, item)| {
                self.render_item(
                    position,
                    item,
                    entries,
                    selected_position == Some(position),
                    vault_root.as_deref(),
                    &day_marks,
                    window,
                    cx,
                )
            })
            .collect()
    }

    /// V26 §5.5: the one place a broken turn is allowed to say so, in plain
    /// language without error styling, with `r` to try again.
    fn render_failure(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let session = self.session.as_ref()?;
        let failure = session.turn_failure.as_ref()?;
        let retryable = failure.retryable && session.last_sent.is_some();
        Some(
            v_flex()
                .w_full()
                .items_start()
                .gap_1p5()
                .pt(px(10.))
                .child(self.status_line(failure.message.clone(), cx))
                .when(retryable, |this| {
                    this.child(
                        h_flex()
                            .id("thock-chat-retry")
                            .gap_1p5()
                            .items_center()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(cx.theme().colors().element_selected)
                            .cursor_pointer()
                            .child(keycap("r", cx))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(rems_from_px(12.5))
                                    .child("Try again"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.retry_turn(&RetryChatTurn, window, cx)
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// The thread (§5.4): every row, scrolled to the end, with older rows
    /// fading into the page at the top.
    fn render_thread(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let page = cx.theme().colors().editor_background;
        let list = v_flex()
            .id("thock-chat-list")
            .max_h_full()
            .w_full()
            .px_1()
            .pt(px(96.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .children(self.render_items(0, window, cx))
            .children(self.render_failure(cx));
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(v_flex().size_full().justify_end().child(list))
            .child(div().absolute().top_0().left_0().right_0().h(px(96.)).bg(
                gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(page, 0.),
                    gpui::linear_color_stop(page.opacity(0.), 1.),
                ),
            ))
            .into_any_element()
    }

    /// Talking (§5.2): the thread in its column, the composer beside the
    /// pebble, and the lines that sit above the composer.
    fn render_talking(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let connected = matches!(self.connection, PlusConnection::Connected { .. });
        v_flex()
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(PEBBLE_RIGHT))
            .w(px(THREAD_WIDTH))
            .child(match &self.view {
                PanelView::Connect(flow) => self.render_connect(flow, cx),
                PanelView::Chat if !connected => self.render_unconnected(cx),
                PanelView::Chat => self.render_thread(window, cx),
            })
            .when(self.memory_nudge_due && connected, |this| {
                this.child(self.render_memory_nudge(cx))
            })
            .when(self.show_usage, |this| this.child(self.render_footer(cx)))
            .child(
                h_flex()
                    .w_full()
                    .pt_2()
                    .pb(px(PEBBLE_BOTTOM))
                    .child(div().w(px(COMPOSER_WIDTH)).child(self.render_composer(cx))),
            )
            .into_any_element()
    }

    /// Working with the thread folded (§5.2): the pebble types, with the
    /// live line under the dots.
    fn render_working(&self, cx: &Context<Self>) -> AnyElement {
        let status: SharedString = match &self.starting {
            Some(status) => status.clone(),
            None => self.live_line(cx).into(),
        };
        v_flex()
            .absolute()
            .right(px(PEBBLE_RIGHT + PEBBLE + 12.))
            .bottom(px(PEBBLE_BOTTOM))
            .items_end()
            .gap_1()
            .child(
                self.thock_bubble(
                    SpinnerLabel::new()
                        .size(LabelSize::Small)
                        .into_any_element(),
                    cx,
                ),
            )
            .child(self.status_line(status, cx))
            .into_any_element()
    }

    /// What the open turn is doing, in V26's live vocabulary.
    fn live_line(&self, cx: &App) -> String {
        let Some(session) = &self.session else {
            return summarize_activity(&[], true);
        };
        let thread = session.thread.read(cx);
        let vault_root = self.vault().map(|vault| vault.root.clone());
        match self.items(cx).last() {
            Some(ChatItem::Typing { calls }) => summarize_activity(
                &summary_calls(calls, thread.entries(), vault_root.as_deref()),
                true,
            ),
            _ => summarize_activity(&[], true),
        }
    }

    /// Needs you (§5.2): a bubble floats up from the pebble with what waits,
    /// under a caption saying who and which ritual.
    fn render_needs_you(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let title = self
            .session
            .as_ref()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "Thock".into());
        let caption = h_flex()
            .gap_1()
            .items_baseline()
            .child(
                div()
                    .font_family(THOCK_FACE)
                    .font_weight(FontWeight::MEDIUM)
                    .italic()
                    .text_size(rems_from_px(13.5))
                    .child("Thock"),
            )
            .child(
                div()
                    .text_size(rems_from_px(11.))
                    .text_color(cx.theme().colors().text_placeholder)
                    .child(format!("· {title}")),
            );
        let content: AnyElement = match (&self.view, self.waiting) {
            (PanelView::Connect(flow), _) => self.render_connect(flow, cx),
            (_, Some(Waiting::Connect)) => self.render_unconnected(cx),
            (_, Some(Waiting::Failure)) => self
                .render_failure(cx)
                .unwrap_or_else(|| div().into_any_element()),
            (_, Some(Waiting::Question)) => {
                // The question, and the reply it followed when there is one.
                let items = self.items(cx);
                let question = items
                    .iter()
                    .rposition(|item| matches!(item, ChatItem::Question(_)))
                    .unwrap_or(0);
                let from = question
                    .checked_sub(1)
                    .filter(|before| matches!(items.get(*before), Some(ChatItem::Reply(_))))
                    .unwrap_or(question);
                v_flex()
                    .w_full()
                    .children(self.render_items(from, window, cx))
                    .into_any_element()
            }
            (_, Some(Waiting::Reply)) => {
                let items = self.items(cx);
                let from = items
                    .iter()
                    .rposition(|item| matches!(item, ChatItem::Reply(_)))
                    .unwrap_or(0);
                v_flex()
                    .w_full()
                    .children(self.render_items(from, window, cx))
                    .into_any_element()
            }
            (_, None) => div().into_any_element(),
        };
        v_flex()
            .absolute()
            .right(px(PEBBLE_RIGHT))
            .bottom(px(BUBBLE_BOTTOM))
            .w(px(THREAD_WIDTH))
            .items_start()
            .gap_1()
            .child(caption)
            .child(content)
            .into_any_element()
    }

    /// Not connected (§5.11): one bubble, and the two ways in as chips.
    fn render_unconnected(&self, cx: &Context<Self>) -> AnyElement {
        let text: SharedString = match &self.connection {
            PlusConnection::Revoked(message) => message.clone(),
            PlusConnection::Loading => "Checking your Thock Plus connection…".into(),
            _ => "I'm not connected yet.".into(),
        };
        v_flex()
            .w_full()
            .items_start()
            .gap_2()
            .child(self.thock_bubble(div().child(text).into_any_element(), cx))
            .child(self.render_chips(
                &Self::connect_choices(),
                0,
                |this, index, window, cx| this.pick_connect_choice(index, window, cx),
                cx,
            ))
            .into_any_element()
    }

    /// The pebble (§5.1): always in the corner, with its dot when something
    /// waits and a hollow ring until Thock Plus is connected.
    fn render_pebble(&self, presence: Presence, cx: &Context<Self>) -> AnyElement {
        let connected = matches!(self.connection, PlusConnection::Connected { .. });
        let dot = self.waiting.is_some() && presence != Presence::NeedsYou && !self.thread_open;
        let accent = cx.theme().colors().text_accent;
        div()
            .id("thock-chat-pebble")
            .absolute()
            .right(px(PEBBLE_RIGHT))
            .bottom(px(PEBBLE_BOTTOM))
            .size(px(PEBBLE))
            .rounded_tl(px(22.))
            .rounded_tr(px(20.))
            .rounded_br(px(21.))
            .rounded_bl(px(19.))
            .bg(cx.theme().colors().text.opacity(0.06))
            .shadow_md()
            .when(!connected, |this| {
                this.border_2().border_color(accent.opacity(0.6))
            })
            .opacity(if presence == Presence::Resting && !dot {
                0.7
            } else {
                1.
            })
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(
                div()
                    .font_family(THOCK_FACE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(rems_from_px(22.))
                    .text_color(accent)
                    .child("T"),
            )
            .when(dot, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(-2.))
                        .right(px(-2.))
                        .size(px(12.))
                        .rounded_full()
                        .bg(accent),
                )
            })
            .on_click(cx.listener(|this, _, window, cx| this.toggle_focus(window, cx)))
            .into_any_element()
    }

    fn render_composer(&self, cx: &Context<Self>) -> AnyElement {
        let exhausted = self
            .entitlement()
            .is_some_and(|entitlement| entitlement.is_exhausted());
        // The composer is a pill with no border (§5.1): the field, and `esc`
        // as the way back to the page.
        v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_end()
                    .rounded(px(18.))
                    .bg(cx.theme().colors().surface_background)
                    .shadow_sm()
                    .child(div().flex_1().min_w_0().child(self.message_editor.clone()))
                    .child(div().pb_0p5().child(keycap("esc", cx))),
            )
            .when(exhausted, |this| {
                this.child(
                    Label::new("Out of allowance for this cycle")
                        .size(LabelSize::XSmall)
                        .color(Color::Error),
                )
            })
            .into_any_element()
    }

    /// The allowance, as a bar the wife test can read: how much of this
    /// cycle is used, in the plan's own words, turning amber at the warning
    /// mark and red at zero. One line above the composer, behind
    /// `ToggleChatUsage` (§5.11).
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

    /// The connect flow (§5.11), inside a bubble: the invite code field,
    /// `enter` connects and `escape` backs out.
    fn render_connect(&self, flow: &ConnectFlow, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .items_start()
            .gap_2()
            .child(
                self.thock_bubble(
                    div()
                        .child(
                            "Enter the invite code you were given. I set everything else up: \
                            the model and the allowance, no keys, no installs.",
                        )
                        .into_any_element(),
                    cx,
                ),
            )
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .rounded(px(18.))
                    .bg(cx.theme().colors().surface_background)
                    .shadow_sm()
                    .child(flow.code_editor.clone()),
            )
            .when_some(flow.error.clone(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .child(
                Label::new(if flow.busy {
                    "Connecting…"
                } else {
                    "Enter to connect · Esc to go back"
                })
                .size(LabelSize::XSmall)
                .color(Color::Muted),
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
            // The chips' keys: `1`–`9`, and `y`/`n` for a pair (§5.12).
            let chips = self
                .latest_pending_question(cx)
                .and_then(|question| self.question_choices(&question, cx))
                .map(|(choices, _)| choices.len())
                .or_else(|| (self.waiting == Some(Waiting::Connect)).then_some(2));
            if let Some(count) = chips {
                key_context.add("choice");
                if count == 2 {
                    key_context.add("choice_pair");
                }
            }
        }

        let presence = self.presence(cx);
        // BYO mode and a folder that isn't a vault have no chat (§5.11).
        let hidden = self.mode == Some(ConnectionMode::Byo)
            || !matches!(self.vault_status, VaultStatus::Valid(_));
        let bounds_probe = {
            let this = cx.entity();
            gpui::canvas(
                move |bounds, _window, cx| {
                    this.update(cx, |this, _| this.centre_bounds = bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
        };
        // The layer spans the centre, but only the chat's own shapes take the
        // mouse and the focus: the page underneath stays yours.
        let chat = div()
            .id("thock-chat")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::send_message))
            .on_action(cx.listener(Self::stop_turn))
            .on_action(cx.listener(Self::new_chat))
            .on_action(cx.listener(Self::focus_input))
            .on_action(cx.listener(Self::focus_thread_list))
            .on_action(cx.listener(Self::open_entry))
            .on_action(cx.listener(Self::expand_activity))
            .on_action(cx.listener(Self::collapse_activity))
            .on_action(cx.listener(Self::toggle_activity))
            .on_action(cx.listener(Self::retry_turn))
            .on_action(cx.listener(Self::toggle_usage))
            .on_action(cx.listener(Self::dismiss_memory_nudge))
            .on_action(cx.listener(Self::answer_choice))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .absolute()
            .right_0()
            .bottom_0()
            .when(presence == Presence::Talking, |this| {
                this.top_0().w(px(THREAD_WIDTH + PEBBLE_RIGHT))
            })
            .child(match presence {
                Presence::Resting => div().into_any_element(),
                Presence::Working => self.render_working(cx),
                Presence::NeedsYou => self.render_needs_you(window, cx),
                Presence::Talking => self.render_talking(window, cx),
            })
            .child(self.render_pebble(presence, cx));
        div()
            .size_full()
            .relative()
            .child(bounds_probe)
            .when(!hidden, |this| this.child(chat))
    }
}

impl Focusable for ChatPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EntryShape::{Assistant, Hidden, User};
    use acp_thread::StubAgentConnection;
    use chrono::TimeZone as _;
    use fs::FakeFs;
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use workspace::MultiWorkspace;

    fn tool(id: &'static str) -> EntryShape {
        EntryShape::Tool(acp::ToolCallId::from(id))
    }

    fn question(answered: bool) -> EntryShape {
        EntryShape::Question { answered }
    }

    fn keys(items: &[ChatItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                ChatItem::Kickoff(index) => format!("kickoff:{index}"),
                ChatItem::User(index) => format!("user:{index}"),
                ChatItem::Reply(index) => format!("reply:{index}"),
                ChatItem::Question(index) => format!("question:{index}"),
                ChatItem::Receipt { key, calls } => format!("receipt:{key}:{}", calls.len()),
                ChatItem::Row { index, .. } => format!("row:{index}"),
                ChatItem::Typing { calls } => format!("typing:{}", calls.len()),
            })
            .collect()
    }

    #[test]
    fn a_reply_splits_into_bubbles_at_blank_lines() {
        assert_eq!(
            paragraphs("Evening, Diego.\n\nSeven of eleven done.\n"),
            ["Evening, Diego.", "Seven of eleven done."]
        );
        assert_eq!(
            paragraphs("- one\n- two\n\n\n\nThree"),
            ["- one\n- two", "Three"]
        );
        assert_eq!(
            paragraphs("Before\n\n```\ncode\n\nmore code\n```\n\nAfter"),
            ["Before", "```\ncode\n\nmore code\n```", "After"]
        );
        assert!(paragraphs("  \n\n").is_empty());
    }

    #[test]
    fn day_marks_open_a_day_and_a_pause_longer_than_thirty_minutes() {
        let at = |day: u32, hour: u32, minute: u32| {
            Local
                .with_ymd_and_hms(2026, 10, day, hour, minute, 0)
                .single()
                .unwrap()
        };
        let times = BTreeMap::from([
            (0, at(19, 17, 42)),
            (4, at(19, 17, 50)),
            (9, at(19, 18, 25)),
            (12, at(20, 9, 5)),
        ]);
        let marks = day_marks(&times, NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        assert_eq!(
            marks,
            BTreeMap::from([
                (0, "TODAY · 17:42".to_string()),
                (9, "TODAY · 18:25".to_string()),
                (12, "TUESDAY · 09:05".to_string()),
            ])
        );
    }

    #[test]
    fn narration_between_tool_calls_folds_into_one_receipt() {
        // §6.1: the run's entries become a marker, the reply and a receipt.
        let items = build_items(
            vec![
                User,
                tool("a"),
                Assistant,
                tool("b"),
                Assistant,
                tool("c"),
                Assistant,
            ],
            true,
            false,
        );
        assert_eq!(keys(&items), ["kickoff:0", "reply:6", "receipt:a:3"]);
    }

    #[test]
    fn nothing_before_your_first_message_renders() {
        // Decision 8: a runtime's prelude before the first turn is dropped.
        let items = build_items(vec![Assistant, tool("x"), User, Assistant], false, false);
        assert_eq!(keys(&items), ["user:2", "reply:3"]);
    }

    #[test]
    fn a_question_ends_the_visible_turn_and_its_answer_starts_the_next() {
        let items = build_items(
            vec![
                User,
                tool("a"),
                Assistant,
                question(true),
                tool("b"),
                Assistant,
            ],
            false,
            false,
        );
        assert_eq!(
            keys(&items),
            [
                "user:0",
                "reply:2",
                "receipt:a:1",
                "question:3",
                "reply:5",
                "receipt:b:1"
            ]
        );
    }

    #[test]
    fn a_live_turn_types_instead_of_showing_its_prose() {
        // Decision 6: the reply appears when the turn stops.
        let items = build_items(vec![User, tool("a"), Assistant], false, true);
        assert_eq!(keys(&items), ["user:0", "typing:1"]);
        let items = build_items(vec![User], false, true);
        assert_eq!(keys(&items), ["user:0", "typing:0"]);
    }

    #[test]
    fn a_pending_question_stops_the_turn_rather_than_typing() {
        let items = build_items(
            vec![User, tool("a"), Assistant, question(false)],
            false,
            true,
        );
        assert_eq!(
            keys(&items),
            ["user:0", "reply:2", "receipt:a:1", "question:3"]
        );
    }

    #[test]
    fn a_turn_without_prose_still_shows_its_receipt() {
        let items = build_items(vec![User, tool("a"), tool("b")], false, false);
        assert_eq!(keys(&items), ["user:0", "receipt:a:2"]);
    }

    #[test]
    fn hidden_entries_fold_into_nothing() {
        let items = build_items(
            vec![User, Hidden, tool("a"), Hidden, Assistant, Hidden],
            false,
            false,
        );
        assert_eq!(keys(&items), ["user:0", "reply:4", "receipt:a:1"]);
    }

    #[test]
    fn a_receipt_keeps_its_key_while_the_run_grows() {
        // V26 R4: expansion and selection keyed by the receipt survive the
        // re-fold of a growing turn.
        let before = build_items(vec![User, tool("a")], false, false);
        let after = build_items(
            vec![User, tool("a"), Assistant, tool("b"), tool("c")],
            false,
            false,
        );
        let key_of = |items: &[ChatItem]| {
            items.iter().find_map(|item| match item {
                ChatItem::Receipt { key, .. } => Some(key.clone()),
                _ => None,
            })
        };
        assert_eq!(key_of(&before), key_of(&after));
        assert_eq!(key_of(&after), Some(acp::ToolCallId::from("a")));
    }

    #[test]
    fn only_a_ritual_kickoff_becomes_a_marker() {
        let items = build_items(vec![User, Assistant, User, Assistant], true, false);
        assert_eq!(keys(&items), ["kickoff:0", "reply:1", "user:2", "reply:3"]);
        let items = build_items(vec![User, Assistant], false, false);
        assert_eq!(keys(&items), ["user:0", "reply:1"]);
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
            init(cx);
            // The shipped keymap, so a missing or shadowed binding fails here.
            let bindings = match KeymapFile::load(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            ) {
                KeymapFileLoadResult::Success { key_bindings }
                | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
                KeymapFileLoadResult::JsonParseFailure { error } => panic!("bad keymap: {error}"),
            };
            cx.bind_keys(bindings);
        });
    }

    struct Setup {
        workspace: Entity<Workspace>,
        panel: Entity<ChatPanel>,
        project: Entity<Project>,
        root: PathBuf,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    /// Fourteen numbered lines, so a row can open the note at line 12.
    const NOTE: &str = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n\
                        line 9\nline 10\nline 11\nline 12\nline 13\nline 14\n";

    /// A vault with today's note, the chat mounted over the centre by
    /// `init`'s observer, and no session yet.
    async fn setup(cx: &mut TestAppContext) -> Setup {
        init_test(cx);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({
                ".thock": { "config.toml": "schema = 1\n" },
                "daily": { "2026-10-19.md": NOTE },
            }),
        )
        .await;
        let project = Project::test(fs, [root.as_path()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        cx.run_until_parked();
        let panel = workspace.read_with(&cx, |workspace, _| {
            ChatPanel::in_workspace(workspace).expect("the chat is mounted over the centre")
        });
        Setup {
            workspace,
            panel,
            project,
            root,
            cx,
            _vault_dir: vault_dir,
        }
    }

    /// Opens today's note in the centre and puts the focus in it.
    async fn open_note(setup: &mut Setup) -> Entity<Editor> {
        let path = setup.root.join("daily/2026-10-19.md");
        let project_path = setup
            .project
            .read_with(&setup.cx, |project, cx| {
                project.find_project_path(&path, cx)
            })
            .expect("the note is in the vault");
        let editor = setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();
        setup.cx.focus(&editor);
        setup.cx.run_until_parked();
        editor
    }

    fn is_focused<V: Focusable + 'static>(setup: &mut Setup, entity: &Entity<V>) -> bool {
        setup.cx.update(|window, cx| {
            entity
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        })
    }

    fn presence(setup: &Setup) -> Presence {
        setup
            .panel
            .read_with(&setup.cx, |panel, cx| panel.presence(cx))
    }

    /// Pretends Thock Plus is connected, so the summon key opens the thread
    /// rather than the connect bubble (§5.11).
    fn connect(setup: &mut Setup) {
        let entitlement: Entitlement = serde_json::from_value(json!({
            "user_id": "u1",
            "status": "active",
            "plan_id": "plus",
            "plan_name": "Thock Plus",
            "allowance_units": 100,
            "remaining_units": 100,
        }))
        .expect("a valid entitlement");
        setup.panel.update(&mut setup.cx, |panel, cx| {
            panel.connection = PlusConnection::Connected {
                credential: "test".to_string(),
                entitlement,
                warned_low: false,
            };
            cx.notify();
        });
    }

    fn inset(setup: &Setup) -> Option<Pixels> {
        setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.center_inset())
    }

    fn editor_width(setup: &Setup, editor: &Entity<Editor>) -> Pixels {
        editor.read_with(&setup.cx, |editor, _| {
            editor
                .last_bounds()
                .map(|bounds| bounds.size.width)
                .unwrap_or_default()
        })
    }

    fn edit(id: &'static str, path: PathBuf, line: u32) -> acp::SessionUpdate {
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(id, "Edit")
                .kind(acp::ToolKind::Edit)
                .status(acp::ToolCallStatus::Completed)
                .locations(vec![acp::ToolCallLocation::new(path).line(line)]),
        )
    }

    #[gpui::test]
    async fn an_open_receipt_lists_its_rows_and_enter_opens_the_note_at_its_line(
        cx: &mut TestAppContext,
    ) {
        let mut setup = setup(cx).await;
        let connection = Rc::new(StubAgentConnection::new());
        let note = setup.root.join("daily/2026-10-19.md");
        let updates = vec![
            edit("edit-1", note.clone(), 12),
            read("read-1", note.clone()),
            says("Ticked."),
        ];
        start_wrap_today(&mut setup, connection, None, updates).await;
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nTicked.\nUpdated daily/2026-10-19.md"
        );

        // The reply floated up; the first summon focuses it, the second opens
        // the thread, and `shift-tab` leaves the composer for the last row.
        setup
            .cx
            .simulate_keystrokes("ctrl-alt-n ctrl-alt-n shift-tab");
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::Talking);
        setup.cx.simulate_keystrokes("right");
        // Reads before changes (§5.4), whatever order the calls came in.
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nTicked.\nUpdated daily/2026-10-19.md\n  read \
             daily/2026-10-19.md\n  changed daily/2026-10-19.md"
        );
        setup.cx.simulate_keystrokes("down down left");
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nTicked.\nUpdated daily/2026-10-19.md",
            "left on a row closes its receipt"
        );

        setup.cx.simulate_keystrokes("right down down enter");
        setup.cx.run_until_parked();
        let cursor_row = setup
            .workspace
            .update_in(&mut setup.cx, |workspace, _, cx| {
                let editor = workspace
                    .active_item_as::<Editor>(cx)
                    .expect("the note opened");
                editor.update(cx, |editor, cx| {
                    let snapshot = editor.display_snapshot(cx);
                    editor.selections.newest::<Point>(&snapshot).head().row
                })
            });
        assert_eq!(cursor_row, 11, "the cursor sits on the row's line");
    }

    /// Attaches a stub agent session to the panel as a Wrap Today launch.
    /// `prelude` is sent before the kickoff, the way pi-acp's startup notice
    /// is, and `updates` play during the kickoff turn, which then ends.
    async fn start_wrap_today(
        setup: &mut Setup,
        connection: Rc<StubAgentConnection>,
        prelude: Option<&str>,
        updates: Vec<acp::SessionUpdate>,
    ) -> acp::SessionId {
        let work_dirs = setup
            .project
            .read_with(&setup.cx, |project, cx| project.default_path_list(cx));
        let thread = setup
            .cx
            .update(|_, cx| {
                connection
                    .clone()
                    .new_session(setup.project.clone(), work_dirs, cx)
            })
            .await
            .unwrap();
        let session_id = thread.read_with(&setup.cx, |thread, _| thread.session_id().clone());
        if let Some(prelude) = prelude {
            setup.cx.update(|_, cx| {
                connection.send_update(session_id.clone(), says(prelude), cx);
            });
        }
        connection.set_next_prompt_updates(updates);
        setup.panel.update_in(&mut setup.cx, |panel, window, cx| {
            panel.attach_session(
                LaunchRequest::run_skill(
                    "Wrap Today",
                    "routines/timeline/skills/wrap-today.md",
                    ModelTier::Default,
                ),
                connection,
                thread,
                0,
                window,
                cx,
            );
        });
        setup.cx.run_until_parked();
        session_id
    }

    fn visible(setup: &Setup) -> String {
        setup
            .panel
            .read_with(&setup.cx, |panel, cx| panel.visible_text(cx))
    }

    fn says(text: &str) -> acp::SessionUpdate {
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(text.into()))
    }

    fn read(id: &'static str, path: PathBuf) -> acp::SessionUpdate {
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(id, "Read")
                .kind(acp::ToolKind::Read)
                .status(acp::ToolCallStatus::Completed)
                .locations(vec![acp::ToolCallLocation::new(path)]),
        )
    }

    fn search(id: &'static str) -> acp::SessionUpdate {
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(id, "Search")
                .kind(acp::ToolKind::Search)
                .status(acp::ToolCallStatus::Completed),
        )
    }

    const NOTICE: &str = "New version available: v1.3.0 (installed v0.85.1). Run: `npm i -g \
                          @earendil-works/pi-coding-agent`";

    #[gpui::test]
    async fn a_ritual_run_reads_as_a_marker_a_reply_and_a_receipt(cx: &mut TestAppContext) {
        // §6.1: the twelve entries of the Wrap Today run from the screenshot.
        let mut setup = setup(cx).await;
        let connection = Rc::new(StubAgentConnection::new());
        let root = setup.root.clone();
        let updates = vec![
            read(
                "read-1",
                root.join("routines/timeline/skills/wrap-today.md"),
            ),
            says("I'll walk through this step by step. Step 1: Locate today's note"),
            search("search-1"),
            says("Now let me check the sources configuration for code repositories:"),
            read("read-2", root.join("routines/timeline/sources.md")),
            says("Let me pull in today's code activity and scan recent days:"),
            search("search-2"),
            says("Now let me check memory/index.md to understand this person's patterns:"),
            read("read-3", root.join("memory/index.md")),
            says(
                "Based on what I've found, I need to ask about one task. Should I mark it as done?",
            ),
        ];
        start_wrap_today(&mut setup, connection, Some(NOTICE), updates).await;
        let text = visible(&setup);
        assert_eq!(
            text,
            "You started Wrap Today\n\
             Based on what I've found, I need to ask about one task. Should I mark it as done?\n\
             Looked through your notes"
        );
        // G7: no mechanics in the default view.
        for leak in [
            "npm",
            "New version",
            "let me check",
            "Read and execute",
            "ENOENT",
            "/Users",
        ] {
            assert!(!text.contains(leak), "{leak:?} leaked into {text:?}");
        }
    }

    #[gpui::test]
    async fn the_reply_waits_for_the_turn_to_stop(cx: &mut TestAppContext) {
        // Decision 6: while Thock works the thread types, and the prose that
        // streamed appears whole when the turn stops.
        let mut setup = setup(cx).await;
        let connection = Rc::new(StubAgentConnection::new());
        let session_id = start_wrap_today(&mut setup, connection.clone(), None, vec![]).await;
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nWorking behind the scenes…"
        );
        let note = setup.root.join("daily/2026-10-19.md");
        setup.cx.update(|_, cx| {
            connection.send_update(session_id.clone(), read("read-1", note), cx);
            connection.send_update(session_id.clone(), says("Done. Seven of eleven."), cx);
        });
        setup.cx.run_until_parked();
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nLooking at daily/2026-10-19.md…"
        );
        connection.end_turn(session_id, acp::StopReason::EndTurn);
        setup.cx.run_until_parked();
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nDone. Seven of eleven.\nLooked at daily/2026-10-19.md"
        );
    }

    fn visible_chips(setup: &Setup) -> Vec<String> {
        setup
            .panel
            .read_with(&setup.cx, |panel, cx| panel.visible_chips(cx))
    }

    fn generating(setup: &Setup) -> bool {
        setup
            .panel
            .read_with(&setup.cx, |panel, cx| panel.is_generating(cx))
    }

    /// The permission request pi-acp raises for an extension's `select`.
    fn ask_request(id: &'static str, question: &str) -> acp::SessionUpdate {
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(id, question)
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .raw_input(json!({ "method": "select", "title": question })),
        )
    }

    /// The model's own call of the `ask` tool.
    fn ask_call(id: &'static str, question: &str, options: &[&str]) -> acp::SessionUpdate {
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(id, "ask")
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::InProgress)
                .raw_input(json!({ "question": question, "options": options })),
        )
    }

    fn choices(labels: &[&str]) -> PermissionOptions {
        PermissionOptions::Flat(
            labels
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    acp::PermissionOption::new(
                        format!("choice-{index}"),
                        *label,
                        acp::PermissionOptionKind::AllowOnce,
                    )
                })
                .collect(),
        )
    }

    /// The §6.2 question, raised through the `ask` tool with `labels` as
    /// its chips: the turn is waiting on it when this returns.
    async fn ask_question(setup: &mut Setup, labels: &[&str]) -> Rc<StubAgentConnection> {
        let connection = Rc::new(StubAgentConnection::new().with_permission_requests(
            HashMap::from_iter([(acp::ToolCallId::from("pi-ui-1"), choices(labels))]),
        ));
        let updates = vec![
            says("John's report looks handled. Shall I tick it?"),
            ask_call("ask-1", "Tick John's report?", labels),
            ask_request("pi-ui-1", "Tick John's report?"),
        ];
        start_wrap_today(setup, connection.clone(), None, updates).await;
        connection
    }

    #[gpui::test]
    async fn an_ask_request_shows_chips_and_a_number_key_answers_it(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        ask_question(&mut setup, &["Tick it", "Not yet", "Later"]).await;
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nJohn's report looks handled. Shall I tick it?\nTick John's report?"
        );
        assert_eq!(visible_chips(&setup), ["Tick it", "Not yet", "Later"]);
        // Never granted on your behalf: the turn is still waiting, as a
        // bubble that took no focus.
        assert!(generating(&setup));
        assert_eq!(presence(&setup), Presence::NeedsYou);

        setup.cx.simulate_keystrokes("ctrl-alt-n 2");
        setup.cx.run_until_parked();
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nJohn's report looks handled. Shall I tick it?\nNot yet"
        );
        assert!(visible_chips(&setup).is_empty());
        assert!(!generating(&setup));
    }

    #[gpui::test]
    async fn a_question_floats_up_without_taking_focus_and_y_answers_it(cx: &mut TestAppContext) {
        // §6.2 and G3: visible without opening anything, answered with the
        // summon key and one key, and focus is back in the note after.
        let mut setup = setup(cx).await;
        let editor = open_note(&mut setup).await;
        ask_question(&mut setup, &["Tick it", "Not yet"]).await;
        assert_eq!(presence(&setup), Presence::NeedsYou);
        assert!(
            is_focused(&mut setup, &editor),
            "a bubble takes no focus by itself"
        );

        setup.cx.simulate_keystrokes("ctrl-alt-n");
        let panel = setup.panel.clone();
        assert!(is_focused(&mut setup, &panel));
        setup.cx.simulate_keystrokes("y");
        setup.cx.run_until_parked();
        assert!(
            visible(&setup).ends_with("\nTick it"),
            "{}",
            visible(&setup)
        );
        assert!(!generating(&setup));
        assert!(
            is_focused(&mut setup, &editor),
            "focus goes back to the note"
        );
        assert_eq!(presence(&setup), Presence::Resting);
    }

    #[gpui::test]
    async fn escape_folds_a_bubble_into_the_dot(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let editor = open_note(&mut setup).await;
        ask_question(&mut setup, &["Tick it", "Not yet"]).await;
        setup.cx.simulate_keystrokes("ctrl-alt-n escape");
        setup.cx.run_until_parked();
        // The question still waits, folded into the dot, and the turn runs.
        assert_eq!(presence(&setup), Presence::Working);
        setup.panel.read_with(&setup.cx, |panel, _| {
            assert_eq!(panel.waiting, Some(Waiting::Question));
            assert!(!panel.bubble_up);
        });
        assert!(is_focused(&mut setup, &editor));
        // It doesn't float up again by itself; the summon key brings the
        // thread, with the question in it.
        setup.cx.simulate_keystrokes("ctrl-alt-n");
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::Talking);
        assert_eq!(visible_chips(&setup), ["Tick it", "Not yet"]);
    }

    #[gpui::test]
    async fn the_summon_key_opens_the_thread_and_the_page_makes_room(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        connect(&mut setup);
        let editor = open_note(&mut setup).await;
        let width_before = editor_width(&setup, &editor);
        assert_eq!(presence(&setup), Presence::Resting);

        setup.cx.simulate_keystrokes("ctrl-alt-n");
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::Talking);
        let composer = setup
            .panel
            .read_with(&setup.cx, |panel, _| panel.message_editor.clone());
        assert!(is_focused(&mut setup, &composer), "the composer has focus");
        assert_eq!(inset(&setup), Some(px(INSET)));
        assert_eq!(editor_width(&setup, &editor), width_before - px(INSET));

        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::Resting);
        assert!(
            is_focused(&mut setup, &editor),
            "focus goes back to the note"
        );
        assert_eq!(inset(&setup), None);
        assert_eq!(editor_width(&setup, &editor), width_before);
    }

    #[gpui::test]
    async fn focus_leaving_for_a_note_folds_the_thread(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        connect(&mut setup);
        let editor = open_note(&mut setup).await;
        setup.cx.simulate_keystrokes("ctrl-alt-n");
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::Talking);
        setup.cx.focus(&editor);
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::Resting);
        assert_eq!(inset(&setup), None);
    }

    #[gpui::test]
    async fn unconnected_the_summon_key_offers_the_ways_in(cx: &mut TestAppContext) {
        // §5.11: no credential, so the bubble asks how to connect; `n` picks
        // your own agent and the chat folds.
        let mut setup = setup(cx).await;
        let editor = open_note(&mut setup).await;
        setup.cx.simulate_keystrokes("ctrl-alt-n");
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::NeedsYou);
        setup.panel.read_with(&setup.cx, |panel, _| {
            assert_eq!(panel.waiting, Some(Waiting::Connect));
        });
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert_eq!(presence(&setup), Presence::Resting);
        assert!(is_focused(&mut setup, &editor));
    }

    #[gpui::test]
    async fn dot_opens_and_closes_the_selected_receipt(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let connection = Rc::new(StubAgentConnection::new());
        let note = setup.root.join("daily/2026-10-19.md");
        start_wrap_today(
            &mut setup,
            connection,
            None,
            vec![read("read-1", note), says("Done.")],
        )
        .await;
        setup
            .cx
            .simulate_keystrokes("ctrl-alt-n ctrl-alt-n shift-tab");
        setup.cx.run_until_parked();
        assert!(visible(&setup).ends_with("Looked at daily/2026-10-19.md"));
        setup.cx.simulate_keystrokes(".");
        assert!(visible(&setup).ends_with("\n  read daily/2026-10-19.md"));
        setup.cx.simulate_keystrokes(".");
        assert!(visible(&setup).ends_with("Looked at daily/2026-10-19.md"));
    }

    #[gpui::test]
    async fn every_other_permission_request_is_still_granted(cx: &mut TestAppContext) {
        // Decision 16 stands for the harness's own prompts.
        let mut setup = setup(cx).await;
        let connection = Rc::new(StubAgentConnection::new().with_permission_requests(
            HashMap::from_iter([(
                acp::ToolCallId::from("edit-1"),
                PermissionOptions::Flat(vec![
                    acp::PermissionOption::new(
                        "allow",
                        "Allow",
                        acp::PermissionOptionKind::AllowOnce,
                    ),
                    acp::PermissionOption::new(
                        "reject",
                        "Reject",
                        acp::PermissionOptionKind::RejectOnce,
                    ),
                ]),
            )]),
        ));
        let note = setup.root.join("daily/2026-10-19.md");
        let updates = vec![
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new("edit-1", "Edit")
                    .kind(acp::ToolKind::Edit)
                    .status(acp::ToolCallStatus::Completed)
                    .locations(vec![acp::ToolCallLocation::new(note)]),
            ),
            says("Ticked."),
        ];
        start_wrap_today(&mut setup, connection, None, updates).await;
        assert_eq!(
            visible(&setup),
            "You started Wrap Today\nTicked.\nUpdated daily/2026-10-19.md"
        );
        assert!(visible_chips(&setup).is_empty());
        assert!(!generating(&setup));
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
