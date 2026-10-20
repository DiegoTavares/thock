# Thock V44 - Passing notes: a companion that floats on the page

**Status:** Draft, open items resolved (2026-10-19)
**Owner:** Diego · **Date:** 2026-10-19
**Design reference:** Direction D of the agent chat study, with live mockups of every moment and the
three directions it was chosen over: <https://claude.ai/artifact/4CyjrGC85uLuU4WfGjNsB2>. The two
approved frames (`v44-passing-notes-needs-you.png`, `v44-passing-notes-talking.png`) live in the
artifact and beside this file locally; they are not checked in. §6 walks through them.
**Depends on:** `v43-almanac.md` (on the `v43-almanac` branch, not merged yet): the Thock theme tokens,
Petrona, and the Desk panel that owns the right dock. Steps 1 and 2 of §7.4 don't need it and go to
`main` first.
**Companion docs:** `v26-agent-chat-refinement.md` (the renderer this keeps: grouping, the summary
vocabulary, elicitation shapes, the failure floor), `v25-thock-plus-hosted-agent.md` decision 16 (no
approval gates), `v27-agent-session-prompt.md` (`SYSTEM.md` and the context block), `v28-agent-memory.md`
(the Reflect nudge, which becomes the first conversation Thock starts), `v33-iphone-companion.md` §13
(amber means the agent)

---

## 1. Summary

The Thock Agent chat is a dock panel that still reads like a coding agent. In the Wrap Today run that
started this work, Pi's npm update notice is the first line, the user's bubble holds a prompt they
never typed (`Read and execute routines/timeline/skills/wrap-today.md`), five activity pills are
interleaved with narration ("Now let me check memory/index.md to understand this person's
patterns:"), and the one question that needs an answer is the last line of a long scroll. V26 folded
back-to-back tool calls into one line, but Thock narrates between reads and every sentence breaks
the fold.

V44 makes the chat a companion that floats on the page. There is no panel and no line between the
chat and the writing area. Thock rests as a pebble in the corner of the page. While it works, the
pebble types. When it needs you, a bubble floats up from the pebble and the line it is about gets an
amber underline in your note. When you talk, the thread floats beside your text, the page makes room
for it, and older messages fade into the page at the top. Each reply carries a one-line receipt of
the work, which opens in place.

Six changes, in shipping order. The first three work inside today's dock panel, so they can ship
before the float:

1. **One reply per turn.** Everything Thock says or does before its last message in a turn folds
   into one receipt. A ritual's kickoff shows as a marker, and nothing said before the first turn
   renders.
2. **A voice that talks to you.** The prompt calls the user "you", bans narration between steps, asks
   Thock to link the lines it talks about, and gives rituals an `ask` tool so a choice arrives as chips.
3. **Bubbles in two faces.** Thock speaks in Petrona, you in Plex Sans.
4. **The float.** The chat leaves the right dock and floats over the centre of the window, with four
   kinds of presence.
5. **Underlined lines.** The lines a message is about are underlined in any open note.
6. **Conversations Thock starts.** They use the question bubble with stricter manners. The Reflect
   nudge is the first one.

## 2. Locked decisions (2026-10-19)

| # | Decision | Choice |
|---|---|---|
| 1 | Direction | **D, "Passing notes":** the study's Direction A (Texts: bubbles, typing, receipts) with two ideas from B (Marginalia: underline the line a message is about, and nothing dividing the chat from the page). A's conversation floats like its question bubble instead of sitting in a side column. Chosen over A, B (margin notes, High cost, weak for long chats) and C (the chat set as a play script). |
| 2 | Where it lives | **A floating layer over the centre pane group**, anchored bottom-right. It never covers a dock, the masthead or the status bar. No dock panel, no border, no divider. The Desk keeps the right dock to itself. |
| 3 | Presence | **Four states:** Resting (the pebble), Working (the pebble types), Needs you (a bubble), Talking (the thread). §5.2. |
| 4 | Room | **While Talking, the page makes room.** The editor narrows by the thread's width, so no line hides under the thread and the underlined lines stay visible beside it. The space it gives up has no border and the page's colour. The room comes from the right margin first, then the note down to 400px; when the note would go under that with the Desk open, the Desk collapses while the thread is open (§5.3). A bubble in Needs you floats over the page without moving it. |
| 5 | What counts as the reply | **The last assistant message before the turn stops for you:** the turn ends, or a question arrives as a request (an `ask` choice or an elicitation, §5.5). Every earlier message and every tool call in the turn folds into the turn's receipt. This extends V26 decision 4 from runs of tool calls to whole turns. |
| 6 | Streaming | **Messages arrive whole.** While Thock works the pebble types, and the reply appears when the turn stops. A message that appears never vanishes into a fold. (Risk R1.) |
| 7 | Ritual kickoff | **A marker, not your bubble:** "You started **Wrap Today** · 17:42". The kickoff prompt still goes to the agent unchanged. |
| 8 | Startup chatter | **Nothing before the first turn renders.** This is the whole fix for Pi's version notice, which is pi-acp's own check and out of the flag's reach (§5.10), and it covers whatever a runtime prints next. |
| 9 | Who said what | **Typeface, then side.** Thock in Petrona on the left, you in Plex Sans on the right. No avatars or names on bubbles. |
| 10 | Underline | **Straight amber underline**, drawn as an editor text highlight. Wavy is rejected because it reads as a spelling or diagnostic squiggle. Hand-drawn is deferred (High cost). Amber still means the agent (V33 §13). |
| 11 | Which lines | **Lines Thock links in its reply, and lines a tool call changed.** Never guessed from prose. A link that doesn't resolve to exactly one line draws nothing. |
| 12 | Quick replies | **Only for choices raised as a request:** the `ask` tool Thock gives rituals, or an ACP elicitation (§5.5). A choice becomes chips with number keys. A question asked in prose gets the reply field. |
| 13 | Conversations Thock starts | **Same bubble as a question**, with stricter manners (§5.8). When and why Thock starts one belongs to the upcoming feature's own spec. |
| 14 | The work, expanded | **Rows only:** a verb and a vault-relative path, with changes in amber. V26's diff previews leave the expansion, because the page now shows the change (the tick, the underline). `enter` opens a row's note at its line. This revises V26 decision 10. |
| 15 | Scope | **The hosted chat (`chat_panel.rs`) only.** The BYO terminal panel (`agent_panel.rs`) is untouched, as in V26 decision 3. |
| 16 | Keys | **`thock::ToggleChatFocus` (`cmd-alt-n` / `ctrl-alt-n`) stays the summon.** The key context stays `ThockChatPanel`, so existing bindings carry over. `escape` folds and no longer stops a running turn. |
| 17 | Opener frequency | **`"openers": "rare"` or `"off"` in the `thock` block of `settings.json`**, beside V43's fonts: how often a person may be interrupted is theirs, not the vault's. |
| 18 | Keep open | **Deferred.** Talking is already a chrome-less column until it folds; a pin comes back if people miss it (R5). |
| 19 | Motion | **The bubble rises and fades in; nothing else moves.** The pebble doesn't morph, and underlines appear at once. |
| 20 | The rail | **A ritual's row shows an amber dot while its session is Working or Needs you** (§5.13). It is in the approved frame, so it ships with the float. |

## 3. Goals & success criteria

- **G1: A ritual reads as a conversation.** Replaying the Wrap Today run shows a marker, one receipt
  and the reply, where today it shows twelve entries (§6.1).
- **G2: Writing is undisturbed.** While you type in a note, nothing floats up. A question or an
  opener waits as a dot on the pebble until you pause, and no bubble ever covers the caret's line.
- **G3: A question needs no navigation.** It is visible without opening anything and is answered
  with `cmd-alt-n` and one key, after which focus is back in your note.
- **G4: The page shows what is meant.** A question about a line in an open note underlines that line.
- **G5: Nothing divides the chat from the page.** No border, rule or panel background. Bubbles, chips
  and the composer are the only filled shapes.
- **G6: Keyboard-complete.** Every state is reachable and operable from the keyboard, with vim motions
  (CLAUDE.md "Panels").
- **G7: No mechanics leak.** V26 G2 stands: no tool names, absolute paths, errno, JSON or runtime
  notices in the default view. Verified as a test over rendered text.

## 4. Non-goals

- **When Thock starts a conversation.** V44 defines how an opener looks and behaves. Triggers,
  schedules and the decision logic belong to the upcoming feature's spec.
- **Hand-drawn underlines** and other drawn marks, such as circles and arrows.
- **Thread history**, several threads, search, or pinning the thread open.
- **The BYO terminal panel** (`agent_panel.rs`).
- **The phone.** The phone's Ask sheet (V35) keeps its own design. Parity is a follow-up.
- **Model, profile or tier pickers.** V25 decision 5 still holds.

## 5. Design

### 5.1 Anatomy

```
 centre pane group, between the rail and the Desk
┌─────────────────────────────────────────────┬───────────────────────────────────┐
│ Day planner                                  │            TODAY · 17:42          │ ← fades into
│ ☐ Workout                                    │   ▸ You started Wrap Today         │   the page
│ ☑ 09:45–10:30 Investigate John's report      │ ╭───────────────────────────────╮ │
│   ‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾     │ │ Evening, Diego. Seven of      │ │ ← Thock,
│ ☐ 14:00–17:00 Resolve Maestro issues         │ │ eleven done.                  │ │   Petrona
│   ‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾‾       │ ╰───────────────────────────────╯ │
│ …                                            │ Read 6 notes ›                    │ ← receipt
│                                              │                    ╭─────────╮    │
│                                              │                    │ Tick it │    │ ← you,
│                                              │                    ╰─────────╯    │   Plex Sans
│                                              │ ( Message Thock            esc )  (T)│ ← composer,
└─────────────────────────────────────────────┴───────────────────────────────────┘   pebble
   the editor, narrowed while Talking              the room it gave up: no border
```

| Part | What it is |
| --- | --- |
| Pebble | A 44px rounded shape with slightly uneven corner radii (22, 20, 21, 19px), a raised fill, a soft shadow and the Thock **T** in amber. It is always in the bottom-right corner of the centre, 24px from the right and 20px from the bottom. A 12px amber dot sits on it when something waits for you. |
| Typing | A small bubble left of the pebble with three dots that breathe, and one line under it in V26's live vocabulary ("Looking through your notes…"). |
| Bubble | One message. Thock's sit on the left, yours on the right. Radius 18px, with the corner nearest the speaker at 7px. |
| Receipt | One line under the last bubble of a turn, composed by V26's `summarize_activity` ("Read 6 notes ›", "Changed 2 notes ›"). |
| Thread | Bubbles in a 360px column, bottom-aligned above the composer. A 96px gradient from the page colour to transparent covers its top, so older messages fade into the page. |
| Composer | A 300px pill-shaped message field left of the pebble, only while Talking. |
| Veil | A soft halo of the page's own colour behind a bubble that overlaps text, drawn as a wide blurred shadow in `editor.background` (GPUI has no radial gradient). It has no edge. |

### 5.2 Presence

| State | On screen | Focus | Enters when | Leaves when |
| --- | --- | --- | --- | --- |
| **Resting** | The pebble at 70% opacity | wherever you are | the app opens; `escape`; focus goes to a note with nothing waiting | a turn starts (→ Working); `cmd-alt-n` (→ Talking); something waits for you (→ Needs you) |
| **Working** | The pebble with the typing bubble | unchanged | a turn starts: a ritual you launched, or a message you sent | the turn stops (→ Needs you if folded, or the reply appears in the thread if open) |
| **Needs you** | A bubble floats up from the pebble, the lines it is about are underlined, and the pebble has a dot | unchanged until `cmd-alt-n` focuses the bubble | Thock asks a question, starts a conversation, or replies while you were writing | you answer (→ Working, focus back where it was); `cmd-alt-n` again (→ Talking); `escape` or typing (→ folds into the dot) |
| **Talking** | The thread, the composer and the pebble. The editor narrows to make room | the composer | `cmd-alt-n` from Resting or Working, or from a focused bubble | `escape`, or focus goes to a note (→ Resting, or the dot if something still waits) |

The pause rule keeps G2. Something that waits for you floats up only after **2 seconds without an
edit** in any note, and an opener only after 8 (§5.8). An edit is something you typed (an editor
transaction), not a note reloading after Thock wrote to it: Thock's own tick must never read as you
typing (§7.1). If you start typing while a bubble is up, it
folds into the pebble's dot, and it doesn't float up again on its own. `cmd-alt-n` brings it back.
A reply that arrived while you were writing floats up once at the next pause, folds into the dot
after 8 seconds, and the dot clears when you open the thread.

### 5.3 Placement and room

- The layer covers the centre pane group only: between the left and right docks, under the masthead,
  above the bottom dock and the status bar.
- **Talking** takes the right 408px of the centre: the 360px thread, 24px from the right edge, and
  a 24px gap. The room comes from, in order:
  1. the right margin of centred layout, which is the strip's own width when it is wider than 408px
     (then nothing moves);
  2. the note, down to 400px wide. Soft wrap (`editor_width`, V43 §7.1) re-wraps it, the same
     reflow a dock causes today;
  3. the Desk. If the note would go under 400px with the Desk open, the right dock closes for as
     long as the thread is open and reopens when it folds. On a window wide enough for both, the
     Desk stays;
  4. if the note would still go under 400px, the thread floats over the page with the veil behind
     it, and nothing moves.

  The freed strip is painted `editor.background` and has no border, and folding gives the width
  back. The numbers at the shipped defaults (rail 240px, Desk 320px, margins 0.2 of the centre):
  at 1440px the centre is 880px and the note 528px; Talking with the Desk open would leave 296px,
  so the Desk collapses and the note is 552px. From about 1570px the Desk stays. **Check on the
  first round of manual tests** that the Desk coming and going reads well; if it doesn't, the
  fallback is to collapse the Desk whenever Talking opens, at every width (R2).
- **Needs you** never moves the page. The bubble sits 72px above the bottom, right-aligned with the
  pebble. It steps up to clear the caret's line and any line it underlines. If it can't clear them,
  it takes the Talking room for as long as it is up.
- With split panes the layer sits over the rightmost pane. The inset narrows the whole centre
  group, so the panes share the loss by their flex ratios.

### 5.4 The thread

- **Turns.** A turn runs from your message (or a ritual's kickoff) until it stops for you. A turn
  renders as: the marker (for a kickoff) or your bubble, Thock's reply as one or more bubbles split
  at blank lines, and the receipt. Consecutive bubbles from the same side group with 3px between
  them, and turns are 10px apart.
- **Day marks.** A centred `TODAY · 17:42` above the first turn of a day, and before a turn that
  starts more than 30 minutes after the previous one. Thread entries carry no time, so the chat
  records when it sees each turn start.
- **Markers.** A ritual kickoff renders as a centred line with the ritual's name in SemiBold: "▸ You
  started **Wrap Today**". The ritual comes from `LaunchRequest::title`. Openers render as Thock's
  bubble with a caption: *Thock* · 17:40.
- **Receipts.** One per turn that called a tool, under the turn's last bubble. The vocabulary is
  V26's (§7, item 1), unchanged. `.` or a click expands it in place into rows: the verb, then the
  vault-relative path in Lilex. Writes are listed after reads, and their verbs are amber:

  ```
  Read 6 notes ›
     read    routines/timeline/skills/wrap-today.md
     read    daily/20261019_Oct-Mon.md
     ticked  John's report · daily/20261019_Oct-Mon.md
  ```

- **Live turn.** While a turn runs, the thread shows the typing bubble at its foot instead of the
  pebble's. The reply replaces it when the turn stops.
- **Selection.** `j`/`k` move a selection through bubbles and receipt rows (V26's selection model,
  keyed by entry, survives streaming re-groups). A selected bubble lights up the lines it is about
  (§5.6).

### 5.5 Questions

A question arrives in one of three ways, and all of them put Thock in Needs you.

1. **A choice from the `ask` tool.** Thock's Pi extension registers `ask` (`question`, and two to
   nine `options`, short labels like "Tick it"). The tool calls Pi's `ctx.ui.select`, and pi-acp
   forwards that as an ACP permission request whose tool call id starts with `pi-ui-` and whose
   `rawInput` carries `method: "select"`, the title and the options, one permission option
   `choice-N` per label. The chat never auto-grants a `pi-ui-*` request (decision 16 still grants
   every other one): it shows Thock's last message, the question, and one chip per option, and
   answering authorizes the chosen option, which is what the tool returns to the model. The
   model's `ask` call and the `pi-ui-*` entry are hidden from the thread and the receipt. Rituals
   use `ask` for every choice (`wrap-today.md` steps 5 and 6); free text stays prose, because
   pi-acp cancels `input` requests.

   | Options | Chips | Keys |
   | --- | --- | --- |
   | two | one chip each | `y` / `n`, `left`/`right` and `enter` |
   | three to nine | one chip each, in order | `1`–`9`, `left`/`right` and `enter` |

2. **An elicitation** (V26 §8 shapes). Pi doesn't raise them today; the shapes stay for an agent
   that does. The bubble shows Thock's last message, then the elicitation's own message, then chips:

   | Shape | Chips | Keys |
   | --- | --- | --- |
   | Choice (`enum`/`oneOf`) | one chip per option, in order | `1`–`9`; `y`/`n` when there are exactly two; `left`/`right` and `enter` as in V26 |
   | Text | a one-line answer field inside the bubble | type, then `enter` |
   | URL | an **Open link** chip | `enter` |
   | Anything richer | **OK** and **Not now** | `y` / `n` |

3. **A turn that ends on a question in prose.** The bubble shows the reply and a reply field, and
   `enter` sends. This is the path when the model asks without `ask` (R8).

Answering from the bubble sends the answer, puts Thock back in Working, and returns focus to where it
was, usually your note. The next question floats up the same way at the next pause.

**A line that isn't on screen.** If a question is about a line in a note that isn't open, or is
scrolled out of view, the bubble quotes the line at its top, the way a reply quotes a message in
Telegram or iMessage. `alt-enter` (`thock::OpenChatEntry`) opens the note at that line.

### 5.6 Underlines

**Where they come from.**

- **Links in a reply.** Thock links the lines it talks about, GitHub-style, with a vault-relative
  path and a 1-based line: `[John's report](daily/20261019_Oct-Mon.md#L12)`. The thread renders the
  link text as plain prose, with no link colour, and the line gets the underline. The chat shows
  plain words and the page shows where they point.
- **Tool calls that changed a line.** An edit's `ToolCallLocation` (`path`, `line`) marks that line.

**Resolution.** A link counts only if all of these hold. Otherwise it draws nothing and is never
guessed:

1. The path is vault-relative, stays inside the vault after normalization (no `..`, no absolute
   path), and names an existing Markdown file.
2. The line exists, and its text contains the link text, compared case-insensitively with whitespace
   folded and Markdown markers (`- [ ]`, `**`) ignored.
3. If the line's text doesn't match, because the note changed since Thock read it, the panel
   searches the file for lines that do match. It uses the line only if exactly one matches.

A resolved line becomes an anchor range in each open editor showing that buffer, so it follows your
edits. If the line is deleted, its underline goes with it.

**How they look.** The style is a 1.5px straight underline in `text.accent` from the first
non-marker character to the end of the line. Lines from the latest message are at 100%. Lines from
earlier messages in the same conversation are at 35%. Hovering a bubble, or selecting it with
`j`/`k`, brings its lines to 100% and dims the rest. Underlines clear when Thock goes back to Resting.

**Limits.** A `#` or `##` heading drawn as a V43 replace block can't take a text highlight. A
mention of a heading quotes it in the bubble instead. Underlines appear at once, and a draw-in
animation is out of scope.

### 5.7 Typography and colour

Sizes are at the shipped defaults (`ui_font_size` 16), in rems so they follow the user's setting.
GPUI has no letter-spacing, so uppercase labels are plain uppercase, as in V43 §4.1.

| Token | Family / cut | Size | Colour | Example |
| --- | --- | --- | --- | --- |
| `chat.thock` | Petrona Regular | 0.9375rem (15px), line height 1.42 | `text` | Evening, Diego. Seven of eleven done. |
| `chat.you` | Plex Sans Regular | 0.875rem (14px), line height 1.45 | `text` | Just Maestro and Cuegui |
| `chat.caption` | Petrona Medium Italic + Plex Sans Regular | 13.5px + 11px | `text` + `text.placeholder` | *Thock* · Wrap Today |
| `chat.day` | Plex Sans SemiBold, uppercase, `tnum` | 0.6875rem (11px) | `text.placeholder` | TODAY · 17:42 |
| `chat.marker` | Plex Sans Regular, ritual in SemiBold | 0.75rem (12px) | `text.muted` | ▸ You started **Wrap Today** |
| `chat.receipt` | Plex Sans Regular | 0.71875rem (11.5px) | `text.muted`, `text` on hover | Read 6 notes › |
| `chat.work` | Lilex Regular | 0.6875rem (11px) | `text.muted`, change verbs `text.accent` | ticked  John's report |
| `chat.chip` | Plex Sans SemiBold + `ui::KeyBinding` | 0.78125rem (12.5px) | `text` | `y` Tick it |
| `chat.status` | Petrona Medium Italic | 0.75rem (12px) | `text.muted` | Looking through your notes… |

| Surface | Fill |
| --- | --- |
| Thock's bubble | `text` at 8% over `editor.background`, computed, so One Dark stays coherent (`#292622` in Thock Dark) |
| Your bubble | `element.selected` (amberSoft in Thock Dark), as V26 G6 |
| Chip | `element.selected` |
| Composer | `surface.background`, with a soft shadow and no border |
| Pebble | `text` at 6% over `editor.background`, with a soft shadow |
| Dot and underline | `text.accent` |

### 5.8 Conversations Thock starts

Thock will soon open some conversations itself. In V44 an opener is a Needs-you bubble with a caption
and stricter manners:

- It floats up only after **8 seconds without an edit**, never while a menu or modal is open, and
  never while another bubble is up.
- It says one thing and asks one question, with chips when the answer is a choice.
- Left alone for 60 seconds, it folds into the pebble's dot. `escape` answers "Not now".
- At most one opener waits at a time. A newer one replaces an unanswered older one.
- How often openers may happen is the user's to set: `"openers": "rare"` (the default) or `"off"`
  in the `thock` block of `settings.json`, beside V43's `body_font` (decision 17). `ThockSettings`
  reads it, and a change applies at once.

The first opener already exists. V28's Reflect nudge (`render_memory_nudge`, "Thock has a few things
to file about you.") stops being a line above the composer and becomes an opener: "I've noted a few
things about you that I haven't filed yet. File them now?", with chips **`y` File them**
(`thock::Reflect`) and **`n` Not now** (`thock::DismissMemoryNudge`).

### 5.9 Voice

Changes to `crates/thock/assets/hosted-agent/SYSTEM.md` and the context block in `hosted_agent.rs`:

| Today | Change |
| --- | --- |
| The context block says "This person's vault…", "what past sessions learned about this person", and Thock repeats it: "to understand this person's patterns" | Address the user as "you" in instructions about them. Add: "Talk to them as *you*, or by the name in `profile.md`. Never describe them in the third person." |
| "Don't end your turn by announcing what you are about to do" | Extend it: "Don't narrate between steps either. Work silently and say it once, when you're done or when you need them." |
| No convention for pointing at lines | Add: "When you talk about a line in a note, link it: `[the words](daily/2026-10-19.md#L12)`, with the vault-relative path and the line number you read." |
| Questions are free prose | Add: "Ask one question at a time, at the end of your message. When the answer is a choice, use the `ask` tool with short labels as the options; write the question in prose only when the answer is free text." `wrap-today.md` steps 5 and 6 say to use `ask` for their yes/no and all/none/some questions. |

### 5.10 Startup chatter

Pi printed "New version available: v1.3.0 (installed v0.85.1). Run: npm i -g
@earendil-works/pi-coding-agent" as the first assistant message, although `hosted_agent.rs` sets
`PI_SKIP_VERSION_CHECK=1`. The notice is pi-acp's, not Pi's: on every `session/new` the adapter
runs `pi --version` and `npm view @earendil-works/pi-coding-agent version` (800ms timeout) and,
with `quietStartup` on, that line is the whole startup prelude, sent as one assistant message
right after `session/new` returns and before the first prompt. The flag is Pi's own and never
reaches that code; `hosted_agent.rs` keeps setting it, for Pi.

Decision 8 is the fix: assistant content that arrives before the first user turn or kickoff is
not rendered. The test that asserts the variable stays; the new test feeds a prelude message
before the kickoff and checks that the thread shows nothing. A fix at the source was declined
(decision log 9), so the adapter's startup lookup stays as a cost of up to 800ms before the
first turn.

### 5.11 Connection, allowance, failures, stop

- **Not connected.** The pebble shows a hollow ring. `cmd-alt-n` opens a bubble: "I'm not connected
  yet." with chips **Connect Thock Plus** (`thock::ConnectThockAgent`) and **Use your own agent**
  (`thock::UseOwnAgent`). The invite-code field of today's connect flow appears inside the bubble,
  and `enter` submits it.
- **Allowance.** `thock::ToggleChatUsage` shows one line above the composer. When the allowance is
  exhausted, the composer is disabled with today's copy, "Out of allowance for this cycle".
- **Failures.** V26 §5.5 stands. A failed turn shows one muted line with a **`r` Try again** chip
  (`thock::RetryChatTurn`). If the thread is folded, the line arrives as a Needs-you bubble.
- **Stop.** The typing bubble shows a stop button on hover and focus, and `cmd-.` / `ctrl-.` in the
  chat runs `thock::StopChatTurn`. `escape` no longer stops a turn, because it now means "back to the
  page".
- **New chat.** `o` in the thread, or `thock::NewChat`, starts a fresh thread under a new day mark.
- **BYO mode.** When the workspace uses its own agent (`hosted_mode_active` is false), the pebble is
  hidden and nothing in this spec applies.

### 5.12 Keys

The chat keeps the key context `ThockChatPanel`, with `menu`. The bubble adds `choice` while a
choice is pending and `choice_pair` when it has exactly two options.

| Keys | Action (`thock::` unless noted) | Where | Change |
| --- | --- | --- | --- |
| `cmd-alt-n` / `ctrl-alt-n` | `ToggleChatFocus` | anywhere | Resting or Working → Talking. Needs you → focus the bubble, then a second press opens Talking. Focused → fold, focus back to the page |
| `escape` | `menu::Cancel` | chat | Folds and returns focus to the page. No longer stops a turn. With vim, the composer's first `escape` leaves insert mode |
| `enter` | `menu::Confirm` | chat | Send from the composer, answer with the highlighted chip, or open the selected row |
| `1`–`9` | `AnswerChatChoice` (index) | bubble, `choice` | new |
| `y` / `n` | `AnswerChatChoice` 0 / 1 | bubble, `choice_pair` | new |
| `left`/`right`, `h`/`l` | `ExpandChatActivity` / `CollapseChatActivity`, or move between chips | chat | as V26 |
| `.` | `ToggleChatActivity` | thread, not editing | new: opens or closes the selected receipt |
| `up`/`down`, `k`/`j`, `g g`/`shift-g` | `menu::SelectPrevious`/`SelectNext`/`SelectFirst`/`SelectLast` | thread, not editing | as V26. The selected bubble lights its lines |
| `shift-tab` | `FocusChatThread` | composer | new: select the last bubble |
| `tab`, `i`, `a` | `FocusChatInput` | thread, not editing | as V26 |
| `alt-enter`, `g space` | `OpenChatEntry` | chat | Opens the selected row's note, or the quoted line, at its line |
| `cmd-.` / `ctrl-.` | `StopChatTurn` | chat, including the composer | new binding, under both `ThockChatPanel` and `ThockChatPanel > Editor`, where it shadows `editor::ToggleCodeActions` |
| `r` | `RetryChatTurn` | chat, after a failure | new binding |
| `o` | `NewChat` | thread, not editing | as V26 |

Every new action has a doc comment written for a note-taker, so it reads well in the command
palette. Keymap changes land in `default-macos.json`, `default-linux.json` and `vim.json` together.

### 5.13 The rail while a ritual runs

A ritual's row in the Routines rail shows a 6px `text.accent` dot at its right edge while the hosted
session it launched is Working or Needs you (§5.2), and clears it when that session goes back to
Resting or another launch replaces it. The rail learns it from the chat and never asks the agent.
The rendered row is what the test checks.

## 6. Examples

### 6.1 The fold, on the run from the screenshot

| # | What the agent sent | What V44 renders |
| --- | --- | --- |
| 1 | "New version available: v1.3.0 … Run: npm i -g …" | nothing (decision 8) |
| 2 | your message: "Read and execute routines/timeline/skills/wrap-today.md" | marker: ▸ You started **Wrap Today** · 17:42 |
| 3 | read `routines/timeline/skills/wrap-today.md` | folds into the receipt |
| 4 | "I'll walk through this step by step. Step 1: Locate today's note" | folds |
| 5 | search: your notes | folds |
| 6 | "Now let me check the sources configuration for code repositories:" | folds |
| 7 | read `routines/timeline/sources.md` | folds |
| 8 | "Let me pull in today's code activity and scan recent days:" | folds |
| 9 | search: your notes | folds |
| 10 | "Now let me check memory/index.md to understand this person's patterns:" | folds |
| 11 | read `memory/index.md` | folds |
| 12 | "Based on what I've found, I need to ask about one task … Should I mark that task as done?" | the reply, then the receipt: Read 6 notes › |

Twelve entries become three, with no change to the agent. With §5.9 the reply itself gets shorter:
"John's report looks handled. You locked jaime4103 and filed a ticket with your findings. Shall I
tick it?"

### 6.2 Needs you

_(Frame: `v44-passing-notes-needs-you.png`, in the artifact.)_

It is 17:42. Diego starts Wrap Today from the rail and keeps writing under *Received a report from
Jonathan on OpenCue*.

1. The pebble types, and **Wrap Today** in the rail shows a running dot. Nothing else moves.
2. At step 5 Thock asks whether John's report is done. Diego is still typing, so only the pebble's
   dot appears.
3. He pauses for 2 seconds. The bubble floats up with the caption *Thock* · Wrap Today, Thock's two
   bubbles, and the chips **`y` Tick it** and **`n` Not yet**. *09:45–10:30 Investigate John's
   report* gets an amber underline, because the reply linked it. The caret stays in his note.
4. `cmd-alt-n` focuses the bubble and `y` answers. Focus is back in the note, and the pebble types
   again.

### 6.3 Talking

_(Frame: `v44-passing-notes-talking.png`, in the artifact.)_

Diego presses `cmd-alt-n` to read the whole exchange. The editor narrows, and the thread floats in
the room it gave up, with no border between them.

- The top fades into the page: the day mark and the marker are going, and the earliest bubbles
  follow as the thread grows.
- Each turn ends with its receipt: "Read 6 notes ›", "Changed today's note ›", "Changed 2 notes ›".
- The latest message, "Done. Both are under Soon…", linked *Resolve Maestro issues* and *Review work
  on Cuegui optimizations*, so those are underlined at 100%. *Investigate John's report* and
  *Workout* were linked by earlier messages and sit at 35%. John's box is ticked, because the tick
  was the turn's one change to his text.
- Hovering "Ticked. Three still open…" brings *Workout*, *Resolve Maestro issues* and *Review work on
  Cuegui optimizations* to 100%.
- `escape` folds the thread, the editor takes its width back, and the underlines clear.

### 6.4 An opener

```
                                   Thock · 17:40
                         ╭───────────────────────────────────────────╮
                         │ Evening, Diego. Today isn't wrapped yet,  │
                         │ and four tasks are still open. Want to go │
                         │ through them now?                         │
                         ╰───────────────────────────────────────────╯
                          ( w  Wrap today )  ( l  Later )             (T)•
```

The four open tasks are underlined while the bubble is up. **`w` Wrap today** launches the ritual,
and **`l` Later** or 60 seconds of silence folds the opener into the dot. The trigger, "today isn't
wrapped by 17:40", belongs to the openers spec. V44 only draws it.

### 6.5 A line that isn't on screen

```
╭───────────────────────────────────────────╮
│ ☐ Review the Q3 budget · Fri 16 Oct       │  ← quote: alt-enter opens Friday's note at the line
│                                           │
│ This one has carried over three days.     │
│ Move it to Someday?                       │
╰───────────────────────────────────────────╯
 ( y  Move it )  ( n  Keep it )
```

## 7. Architecture

### 7.1 The chat leaves the dock

`ChatPanel` keeps its module, its state (session, thread, elicitations, selection) and its V26
renderer, but stops implementing `Panel`, which frees activation priority 11. Today it is loaded in
`thock_app.rs` with `ChatPanel::load` and added to the right dock. Instead, a workspace observer in
`chat_panel::init`, the shape of the masthead's (V43 §6.5), creates it once per workspace and mounts
it in the workspace's new centre overlay slot (§7.2).

Every `workspace.panel::<ChatPanel>()`, `focus_panel` and `toggle_panel_focus` call goes through
one helper, `ChatPanel::in_workspace`, which downcasts the overlay. The calls are in `init`,
`hosted_mode_active` and `ChatPanel::launch_in_workspace`, and `AgentPanel::launch_in_workspace`
reaches the last one when it hands a hosted launch to the chat. Focus
moves between the bubble, the thread and the composer, and folding returns focus to the item that
had it before, which is usually the active editor.

Folding on typing subscribes to the active editor's `EditorEvent::Edited` while the chat is open:
the editor's own transactions, not `BufferEdited`, which also fires when a note reloads after the
agent wrote to it. An edit in a vault note folds Talking and Needs you as §5.2 says, and drives
the pause timer.

The Desk cascade (§5.3) closes the right dock through the workspace, deferred as the Panels rule
says, and remembers that it did, so folding reopens only a dock the chat closed.

`ask` requests reach `handle_thread_event` as `ToolAuthorizationRequested` for a tool call whose id
starts with `pi-ui-`; those go to the question bubble instead of `authorize_tool_call`, and the
answer is `thread.authorize_tool_call` with the chosen `choice-N` option. The `ask` tool itself is
`crates/thock/assets/hosted-agent/ask.ts`, installed beside `vault-guard.ts`.

### 7.2 Inherited crate: `workspace`

Two small additions to `crates/workspace/src/workspace.rs`. They belong there because they are
generic layout hooks, and Thock code mounts into them:

1. **A centre overlay slot.** An `Option<AnyView>` field, `set_center_overlay` and `center_overlay`.
   It is rendered as an absolute child of the `h_flex` that holds the centre and its padding, so it
   spans exactly the centre group: never the docks, the masthead or the status bar. None of the
   existing floating layers fits. Modals are centred and take focus, toasts are transient, and
   `render_notifications` is positioned over the whole content area, including the right dock, so it
   would cover the Desk.
2. **A centre inset.** `set_center_inset(Option<Pixels>)` reserves width on the right of the centre
   and renders it as a plain `editor.background` strip. It has none of the `border_l_1` that the
   centred-layout padding uses. It composes with centred layout without measuring anything: the
   strip is the right padding's element with `min_w(inset)`, so the inset is taken from the right
   padding first, and the strip is `w(inset)` when centred layout is off.

The four `BottomDockLayout` arms build the same `h_flex` around `render_center`; one helper
replaces them, and the slot and the inset live in it.

### 7.3 Underlines

Mentions use the highlight key Thock already owns in the editor, `HighlightKey::ThockMarkdownConceal`,
with two slots that the conceal layer never touches: `MENTION_SLOT` for latest lines and
`MENTION_PAST_SLOT` for earlier ones. Both are defined in the chat module, above the conceal layer's
`HIGHLIGHT_SLOTS`. The style is `HighlightStyle { underline: Some(UnderlineStyle { thickness: px(1.5),
color: Some(accent), wavy: false }), .. }`, with the alpha set per slot. The `editor` crate is
unchanged.

Links come from the message's parsed Markdown events (`MarkdownTag::Link`), not a regex, and the
thread's `MarkdownStyle.link` is the base text style, so link text reads as prose. Resolution (§5.6)
is a pure function from (link, vault root, file text) to `Option<u32>`, so it is unit-tested
without a window. The chat applies resolved lines to every visible editor whose buffer is
that path, and re-applies them when the set of visible editors changes.

### 7.4 Order of work

Each step ships alone and updates VISION.md §12 when it lands.

1. The fold, the kickoff marker and the startup rule, in today's dock panel (`chat_panel.rs`).
2. The voice changes (`SYSTEM.md`, `hosted_agent.rs`), the `ask` tool and its chips, and Wrap
   Today's two questions moved to it.
3. Bubbles, receipts and the type split, still in the dock.
4. The float: the `workspace` slot and inset, mounting, the four states, the pause rule, the Desk
   cascade, the rail's dot and the keys. The manual check of §5.3 happens here.
5. Underlines and the quote fallback.
6. Openers, starting with the Reflect nudge, and the `openers` setting.

Steps 1 and 2 don't touch anything V43 adds and go to `main` first. Step 3 onwards needs
`v43-almanac` merged: Petrona, the theme tokens and the Desk.

**Inherited crates touched:** `workspace` (§7.2), plus the macOS, Linux and vim keymaps.

## 8. Tests

- **Fold:** `build_items` over synthetic turns. Narration between tool calls folds. The last message
  before the turn stops is the reply. An elicitation or a `pi-ui-*` request mid-turn ends the
  visible turn. Content before the first turn is dropped, including a prelude message that arrives
  before the kickoff (§5.10). A kickoff becomes a marker. V26's grouping and vocabulary tests still
  pass.
- **Choices:** a fake agent raises a `pi-ui-*` permission request with three options. The bubble
  shows three chips, `2` authorizes `choice-1`, nothing is auto-granted, and neither the `ask` call
  nor the request shows in the thread or counts in the receipt. With two options, `y` answers
  `choice-0`. Every other permission request is still granted at once. `ask.ts` is checked the way
  `vault-guard.ts` is: the prompt and the extension agree on the tool's name, and
  `wrap-today.md` steps 5 and 6 name `ask`.
- **No leak (G7):** rendered text of the §6.1 run contains no tool name, absolute path, `npm` or
  `ENOENT`.
- **Mentions:** link parsing (`#L12`, missing line, text without a link). Rejection of `..`,
  absolute paths, paths outside the vault and non-Markdown files. Exact-line, moved-line,
  ambiguous and no-match resolution. An edit above the line moves the anchor, and deleting the line
  drops it.
- **Placement:** a pure function from (centre width, Desk width, margin, caret line rect, underlined
  rects, bubble size) to the inset, whether the Desk collapses, and the bubble position. It covers
  the §5.3 cascade at 1440px and 1920px, the 400px float, stepping up over the caret, and splits.
  A keystroke test checks that folding reopens the Desk only when Talking closed it.
- **The rail:** a ritual's row renders the dot while its session is Working and not after the turn
  stops with nothing pending.
- **Keys** (`VisualTestContext` + `simulate_keystrokes`, under `MultiWorkspace::test_new`):
  `cmd-alt-n` from a note opens Talking with the composer focused. `escape` folds, returns focus to
  the editor and removes the inset. With a pending two-option choice, `cmd-alt-n` `y` answers option
  0 and focus is back in the editor. `1`/`2` answer. `j`/`k` move the selection, which survives a
  streaming update. `.` toggles a receipt. Typing in the note while Talking folds. Vim `j`/`k` match
  the arrows.
- **Pause rule:** with GPUI executor timers, a question raised mid-typing floats up 2 seconds after
  the last edit. An opener waits 8 seconds and folds after 60, and `"openers": "off"` floats none.
  Typing folds a bubble, and it doesn't float up again by itself. A buffer reload without an editor
  transaction neither folds nor restarts the timer.
- **Workspace:** the overlay's bounds equal the centre group's with the right dock open. The inset
  narrows the centre by its width and has no border element.
- **Voice:** the generated context block contains no "this person", and `SYSTEM.md` carries the
  linking and no-narration rules. Each check is a test over the generated text.

## 9. Risks & retests

- **R1, whole messages feel slow.** Long ad-hoc answers wait behind the typing dots. Retest after a
  week of use. If it drags, stream the trailing message once a turn has gone 2 seconds without a tool
  call.
- **R2, the reflow is jarring.** Opening the thread re-wraps the note, and on a laptop the Desk
  closes and reopens with it. Both are checked on the first round of manual tests. If the Desk
  coming and going looks worse than it reads, collapse it whenever Talking opens, at every width;
  if the reflow itself is the problem, the fallback is the 400px rule for every width: float over
  the page with the veil.
- **R3, missing or wrong links.** If the model doesn't link, nothing is underlined and the design
  degrades to Texts. Measure the share of questions about a task that carry a resolvable link. Tune
  the prompt before adding any guessing.
- **R4, openers become Clippy.** The research is clear that interruptions have to be earned. Keep
  Rare as the default and the pause and fade rules strict, and retest with the feature's first
  trigger.
- **R5, losing the column.** People who liked a chat that stays open lose it. Talking is a column
  without chrome. A "keep open" toggle is deferred until this retest says people miss it (decision 18).
- **R6, losing diff previews** (decision 14). Revisit if users want the detail of a change and the
  page doesn't show it.
- **R7, headings.** A mention of a V43 heading block shows as a quote, not an underline (§5.6).
- **R8, the model asks in prose instead of `ask`.** The reply field still works (§5.5, item 3), so
  nothing is lost but the chips. Measure it with R3 and tune the prompt and the rituals' wording
  before anything else.

## 10. Open items

None. The five from the first draft were resolved on 2026-10-19 (decision log 7 to 13). Phone
parity for the Ask sheet stays a follow-up (§4).

## 11. Decision log (2026-10-19)

1. **Direction D over A, B and C**, from Diego's review of the study. A felt polished and friendly
   without being unconventional. B contributed the underline and the missing divider. The thread
   floats like A's question bubble.
2. **The chat leaves the dock**, so the Desk keeps the right dock and the two never compete.
3. **The page makes room while Talking.** V43 wraps notes at the editor's edge, so a thread over the
   page would cover text, and the underlined lines have to stay visible.
4. **Messages arrive whole**, so nothing that appears disappears into a fold.
5. **Straight underlines first.** The editor already supports them, and wavy reads as an error.
6. **The key context and the summon key stay**, so V26's bindings and tests carry over.
7. **The Desk collapses only when the window can't fit both** (§5.3). At 1440px with the rail and
   the Desk open the note would drop to 296px, so Talking would always have floated; on wider
   windows the Desk stays. The approved frames show no Desk, which is what a laptop will show.
8. **Choices go through an `ask` tool, not elicitation.** pi-acp 0.0.33 and 0.0.34 raise no
   elicitations, but they forward a Pi extension's `select` and `confirm` as permission requests,
   and the chat already receives those; it only has to stop auto-granting the `pi-ui-*` ones.
   `input` is cancelled by the adapter, so free text stays prose.
9. **The version notice is fixed in the UI only.** It is pi-acp's own `npm view` check, which
   `PI_SKIP_VERSION_CHECK` can't reach; a source fix would mean a change to a third-party package
   or keeping `pi` off the adapter's PATH, and decision 8 already hides it.
10. **The opener setting lives in `settings.json`**, a person's preference like V43's fonts.
11. **Keep-open is deferred** until R5 says people miss a pinned column.
12. **Motion is the bubble's rise and fade only.** Opacity and position are cheap in GPUI; a morph
    and an underline draw-in are not.
13. **The rail's running dot ships with the float**, because it is in the approved frame.
