# Thock V26: Agent Chat Refinement

**Status:** Implemented (see §8)
**Owner:** Diego · **Date:** 2026-09-14
**Companion docs:** `../VISION.md` (§4 invariants, §9 trust model, §12 roadmap), `v25-thock-plus-hosted-agent.md` (the panel this refines; decision 3 "friendlier chat look" and decision 16 "no approval gates" are the constraints here), `v5-agent-and-onboarding.md` (the terminal panel this spec deliberately leaves alone)

---

## 1. Summary

V25 Stage 1 shipped the Thock Agent chat panel. It works, and it reads like a build log: every tool
call is a full-width card, seven consecutive misses are seven cards of raw `ENOENT` against absolute
paths, a dead `Thinking it through…` label fires before every turn, and the user's own message is
right-aligned onto a background indistinguishable from the panel behind it.

V26 makes it read like a conversation. The framing decision is that **the person using this panel
does not program**, so the agent's mechanics are not the content — the agent's answer is. Everything
the agent did between two messages collapses into **one quiet line** that expands in place for anyone
curious enough to ask.

The panel keeps its own renderer. Zed's `agent_ui` is the wrong reuse target and this spec closes
that question (decision 1).

Two things the refinement is explicitly *not*: it is not a metaphor change (the transcript shape
stays — user right, agent left), and it is not a fix for the session prompt. The screenshot that
prompted this spec also shows the agent resolving "last week" to **2023**-09-04 after being told the
date, and asking for today's date at all. That is a `hosted_agent.rs` prompt-and-context defect and
gets its own spec; no amount of rendering fixes it.

## 2. Locked decisions (2026-09-14)

| # | Decision | Choice |
|---|---|---|
| 1 | Reuse vs. refine | **Refine the Thock renderer.** Reusing `agent_ui::conversation_view::thread_view` is closed. `ThreadView::new` is `pub(crate)` with 24 parameters requiring `ConversationView`, `EntryViewState`, `ThreadStore`, `ModeSelector`, `ProfileSelector` and `ModelSelectorPopover` — it is the innards of `ConversationView`, not a component. Calling it means widening visibility across the fastest-moving upstream crate, and its imports drag in `cloud_api_types`, `language_model::LanguageModelRegistry`, `agent_settings`, `agent_skills` and `sandbox` policy UI — all surfaces V12 de-Zed-ified, plus the permission flow V25 decision 16 removed. Forking it means owning ~24k lines of upstream code in the one place Thock's identity lives. This re-affirms V25 decision 3. |
| 2 | Metaphor | **Chat transcript, refined.** User messages right in a visible container, agent prose left, activity interleaved. Familiar shape; the work is in contrast, type scale and density, not in inventing a new reading model. |
| 3 | Panel relationship | **Both agent panels stay, unrelated.** `chat_panel.rs` is the Thock Plus surface; `agent_panel.rs` stays the free BYO-CLI escape hatch (V25 decision 2). V26 does not unify them, does not demote either, and does not design the chat UI to be backend-agnostic. |
| 4 | Tool activity | **One quiet line per turn, expandable in place.** All `ToolCall` entries between two assistant messages collapse to a single muted line in the flow. Expanding reveals the detail inside the transcript — no drawer, no second surface. |
| 5 | Summary line authorship | **The renderer composes it**, from the run's `ToolKind` counts. Deterministic and unit-testable, no prompt dependency, no model-dependent quality floor. The cost is a fixed vocabulary that will read as canned on unusual turns (risk R1). |
| 6 | Write signal | **Same footprint, different wording.** Read-only turns and writing turns both cost exactly one line; a turn that changed a note names the note instead of saying "looked". Writes get no card, no diff preview in the flow, and no confirmation — safety remains scope + undo (V25 decision 16), not visibility. |
| 7 | Reasoning | **Spinner while working, nothing after.** `Thinking it through…` is deleted. There is no thought disclosure and reasoning leaves no residue in the transcript. |
| 8 | Failures | **Never surface as failures.** A note that does not exist is not an error the user needs; the agent says "you don't have notes from that week" in prose. No red icons, no `ENOENT`, no absolute paths, no per-call error text. The floor is the turn itself: when `ThreadStatus` fails, one plain-language line says the turn could not finish (§5.5). |
| 9 | Elicitations | **Inline questions at full visibility.** An elicitation blocks the turn, so it cannot be minimal. An elicitation is a **question, not an approval gate** — rendering it does not reopen V25 decision 16, and the spec says so here so no future reader re-litigates it. |
| 10 | Expansion contents | **Per-call rows with vault-relative paths, plus diffs for edits.** Nothing else. |
| 11 | Terminal & tool output | **Out of scope.** `ToolCallContent::Terminal` and non-failed `ContentBlock`s stay dropped. An `Execute` call renders as a row with its command and no output, in the expansion only. Deliberate narrowing of the "functional gaps" this spec bought (§4). |
| 12 | Session prompt | **Out of scope.** The wrong-year answer and the agent asking for today's date are a `hosted_agent.rs` defect, specified separately. |

## 3. Goals & success criteria

**G1 — A turn reads as a conversation.** Replay the screenshot's exchange. The transcript shows: the
user's question in a container the eye can find, one muted line reading `▸ Looked through your notes`,
and the agent's answer. Seven cards become one line; total vertical space for the agent's work drops
from roughly 40 lines to one.

**G2 — No mechanics leak.** No string rendered in the default (collapsed) transcript contains a tool
name, an absolute path, an errno, or a JSON fragment. Verifiable as a test over rendered text.

**G3 — Detail survives, one keystroke away.** Expanding the line shows every call the run made, with
vault-relative paths, and edits show their diff. Nothing that was visible before this spec becomes
unreachable, except reasoning text (decision 7) and tool output (decision 11), which are dropped
knowingly.

**G4 — The panel is fully keyboard-operable, including the new affordance.** Per `CLAUDE.md`, the new
expand/collapse has `left`/`right` and vim `h`/`l` bindings under `ThockChatPanel`, and selection
survives streaming re-render — a list that re-groups mid-turn must not throw the selection to the top.

**G5 — The agent's prose is the largest thing on screen.** Type scale is deliberate: prose at agent
body size, activity line at `LabelSize::Small` and `Color::Muted`. Today's inversion — where muted
small chrome sits beside unmuted full-size prose with nothing in between — is replaced by a scale
where the quiet things are genuinely quiet and the loud thing is the answer.

**G6 — The user's message is visible.** `render_entry` currently paints the user bubble with
`element_background`, which in the default dark theme is within noise of `panel_background`; the
result is right-aligned text floating on nothing. It gets a token with real separation, or a border.

## 4. Non-goals

- **Adopting or unhiding `agent_ui`** (decision 1). Also: no "extract a shared component from
  `thread_view` into `ui`" — that is an upstream edit in the highest-conflict crate.
- **Terminal output cards and successful tool output** (decision 11). `Execute` calls remain
  output-less. If that becomes painful in use, it is a V27 addition to the expansion, not a reopening
  of the minimalism decision.
- **Image and embedded-resource content blocks.** Rare in a Markdown vault.
- **Reasoning transparency.** Decision 7 discards it. This is a real trade against "everything is
  editable / nothing hidden" and is recorded as risk R3.
- **The session prompt, date injection and vault context** (decision 12).
- **Touching `agent_panel.rs`** (decision 3).
- **Model pickers, profile selectors, thread history UI, feedback.** Tiers stay abstract (V25
  decision 5); none of these arrive with V26.

## 5. Architecture

All work lands in `crates/thock/src/chat_panel.rs`. No file outside `crates/thock/` and `thock/` is
touched.

### 5.1 A turn-grouped view model

`render_chat` today iterates `AgentThreadEntry` one-to-one with `render_entry`, and `selected_entry`
is an index into that flat list. V26 inserts a derived grouping between the thread and the renderer:
a run of consecutive `ToolCall` entries collapses into one `ActivityRun`, while `UserMessage`,
`AssistantMessage`, `Elicitation`, `CompletedPlan` and `ContextCompaction` stay one-to-one.

The grouping is **derived on read, never stored** — the thread remains the source of truth, so a
re-parse or a streaming update cannot desynchronize it. Expansion state and selection are keyed by
something stable across re-grouping (the first `acp::ToolCallId` in the run), not by list index;
keying by index is what would break G4 when a run grows mid-turn.

### 5.2 The summary line

Composed from the run's `ToolKind` counts, in priority order: any `Edit`/`Delete`/`Move` in the run
makes it a writing turn and the line names the note (or counts them, past one); otherwise
`Read`/`Search` make it a looking turn; `Execute` and the rest fall back to a generic phrase. The
vocabulary is a small closed set, chosen for a note-taker rather than an engineer, and is covered by
unit tests over synthetic runs rather than by eyeballing the panel.

`ToolCall` already carries what this needs: `kind`, `status`, `locations` (for the path) and
`raw_input`. `label` — which Pi sets to the bare tool name, the source of today's `Reading read` —
becomes a last resort, and is suppressed entirely when it equals `tool_name`.

### 5.3 Paths

Every path shown anywhere is relative to the vault root. `/Users/dtavares/Thock/daily/2026-09-09.md`
renders as `daily/2026-09-09.md`. Outside-vault paths should not occur under the V25 sandbox; when
one does, it shows its file name only.

### 5.4 The expansion

One row per call: kind icon, the derived sentence, vault-relative path. Edits render the existing
diff editor — `diff_editors` is already wired in the panel and only moves behind the disclosure.
Failed calls render as ordinary rows with no error text and no error styling (decision 8); the fact
that a read found nothing is carried by the agent's prose, and by the row simply existing.

### 5.5 Status, not errors

Per-call failure is invisible. Turn-level failure is not: when `ThreadStatus` ends in a failed state,
the transcript shows one plain-language line saying the agent could not finish, with a retry
affordance. This is the floor that keeps decision 8 from producing a panel where a broken turn looks
identical to a silent one.

### 5.6 Reasoning

The `AssistantMessageChunk::Thought` arm stops emitting an entry. While `ThreadStatus` is running and
no prose has streamed yet, the panel shows a single spinner at the foot of the transcript. It is one
element for the whole turn, not one per thought chunk — today's `showed_thought` latch exists only to
de-duplicate a label that is being deleted.

### 5.7 Elicitations

`AgentThreadEntry::Elicitation` stops rendering `"The Thock Agent asked a question this panel can't
show yet."` and renders the question with its options, at full prose weight, answerable by keyboard.
Answering resolves the elicitation through `acp_thread` and the turn continues.

### 5.8 Keyboard

Existing `ThockChatPanel` bindings are unchanged. Added: `right`/`l` expands the selected activity
line, `left`/`h` collapses it, `enter` on an expanded row opens the note it names (reusing
`OpenChatEntry`). Every one of these is a named `thock::` action so it appears in the command palette,
per `CLAUDE.md`.

## 6. Known risks & retests

**R1 — Canned vocabulary.** A renderer-composed summary (decision 5) has a fixed phrase set; an
unusual turn will get a vague line. Retest after a week of dogfooding: if more than a small fraction
of turns land on the generic fallback, the answer is a wider vocabulary, not handing the sentence to
the model.

**R2 — Invisible writes.** Decision 6 means a turn that edited a note costs one collapsed line, and
decision 8 means nothing turns red. If the model does not mention an edit in prose, the user's only
signal is that line's wording. The safety net is the V25 session checkpoint and undo, and the VISION
invariant that writes are append-or-insert-section. Retest: after real use, does anyone report being
surprised by a change? If so, the cheapest fix is wording, then a diff in the flow — in that order.

**R3 — Discarded reasoning.** Decision 7 drops thought content permanently. It trades against
"everything is editable, nothing hidden". Accepted because the reasoning of a hosted model is not a
file the user owns, and a second collapsed widget per turn works against the whole point of the
refinement. Revisit only if users ask why the agent did something and the transcript cannot answer.

**R4 — Grouping during streaming.** A run that grows while expanded, or an assistant message that
arrives mid-run and splits it, are the two cases that will produce flicker or lost selection.
Covered by tests that drive `AcpThreadEvent` updates and assert selection stability.

**R5 — Silence reads as broken.** With reasoning gone, failures invisible and tool work collapsed, a
long turn shows a spinner and nothing else. §5.5's turn-level status is the mitigation; if spinner
time is routinely long, the activity line should appear live during the turn rather than only after.

## 7. Open items — all resolved at implementation

1. **Summary vocabulary** — implemented in `summarize_activity` and locked by unit tests: writes name
   the note (`Updated daily/2026-09-15.md`, `Removed …` when the run only deletes, `Updated N notes`
   past one), a single read names the note (`Looked at …`), any other looking turn is
   `Looked through your notes`, and everything else falls back to `Worked behind the scenes`. Each
   phrase has a present-tense form for live runs. R1 retest still applies after dogfooding.
2. **User bubble token (G6)** — `element_selected`, with an asymmetric radius (`rounded_lg` +
   `rounded_br_sm`) pointing at the sender. `element_background` was within noise of
   `panel_background` in the default dark theme; the selection surface is two steps up everywhere.
3. **Live or on completion (R5)** — **live**: the activity line renders during the turn in present
   tense ("Looking through your notes…") and settles into past tense when the run completes, so a
   long turn is never a bare spinner.
4. **Turn separators** — none. The alternating bubble/prose silhouette plus spacing carries the
   rhythm; revisit only if long sessions lose their shape in practice.

## 8. Implementation notes (2026-09-14)

All work landed in `crates/thock/src/chat_panel.rs` plus three mechanical keymap additions
(`right`/`left` under `ThockChatPanel && !editing` in `default-macos.json` / `default-linux.json`,
`l`/`h` in the existing `vim.json` block). The visual direction is "Bubble with the Hairline
expansion" from the V26 design exploration: a filled user bubble, a pill-shaped activity line, and
an expansion that hangs rows and diffs off a single hairline rule.

Deltas and details a future reader should know:

- **Grouping** is `build_items` over per-entry shapes: consecutive `ToolCall`s form a run; a
  thought-only (or still-empty) `AssistantMessage` renders nothing *and does not split the run*, so
  interleaved reasoning cannot shatter one activity into many. Selection matches an activity when it
  *contains* the selected call id, not only when it starts with it, so runs merging mid-turn keep
  the selection (R4).
- **Elicitations** support the shapes Pi actually sends: a one-string-property form renders as a
  choice list (`enum`/`oneOf`) or a single-line answer box; URL elicitations render an "Open Link"
  button; anything richer degrades to OK / Not Now. A pending question arrives pre-selected with
  keyboard focus placed so `left`/`right` (or `h`/`l`) move the highlighted option and `enter`
  answers — the same keys that drive activity expansion.
- **Turn failure floor (§5.5)**: `AcpThreadEvent::Error`/`LoadError` and send failures set one
  muted plain-language line with a Try Again button (`thock::RetryChatTurn` re-sends the last
  message); `Refusal` gets its own wording with no retry. Raw errors go to the log only.
- The summary line, grouping, key stability, and vault-relative path rules (including the
  file-name-only fallback for outside-vault paths, G2) are covered by unit tests in `chat_panel.rs`.
