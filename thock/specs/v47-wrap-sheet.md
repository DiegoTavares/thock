# Thock V47 - Sort the day's loose ends in one sheet

**Status:** Implemented (2026-10-28). The tool call id assumption in §5.2 is confirmed (R1).
**Owner:** Diego · **Date:** 2026-10-28
**Companion docs:** `v44-passing-notes.md` §5.5 (questions, the `ask` tool, the pi-acp bridge),
`v41-inbox-desk-view.md` (destination chips), `crates/thock/assets/routines/timeline/skills/wrap-today.md`
steps 5–7

---

## 1. Summary

Wrap Today asks where each loose end of the day goes, and `ask` can only pose one single-choice
question. The skill gets around this in two ways, and both hurt:

- **All of them / None** throws away the per-item decision. Most days the right answer is mixed:
  one task is done, two go to Soon, one to Someday, one stays.
- **Let me pick** turns into one question per task. A day with eight open tasks asks eight
  times, and a wrap with one finished task to confirm, two reminders and five open tasks asks
  close to a dozen times.

The same shape shows up three times in one wrap: step 5 (*Tick it / Not yet*, per task), step 6
(*Let me pick*, per reminder) and step 7 (*Let me pick*, per task).

V47 gives the Thock Agent a second question tool, `sort`: a list of items, each with its own row
of choices, answered in one go. The agent pre-selects what it can justify and says why in one
line. Wrap Today merges steps 5–7 into one sheet, so the whole closure is one question.

## 2. Goals & success criteria

- **G1: One question per wrap.** A Wrap Today run asks about finished tasks, today's reminders and
  open tasks in a single sheet, whatever the number of items.
- **G2: Most rows need no keystroke.** The agent pre-selects a choice wherever it has a reason,
  and shows the reason on the row. Accepting every suggestion takes one key (`enter`).
- **G3: Nothing moves without an answer.** A row the agent can't justify has no pre-selected
  choice, and the sheet can't be applied until every row has one. There is no silent default.
- **G4: Keyboard-complete.** Every row and every choice can be reached and set from the keyboard,
  with vim motions (CLAUDE.md "Panels").
- **G5: No mechanics leak.** V44 G7 still holds: no tool names, ids or JSON in the rendered sheet.

Non-goals: picking a specific date for a reminder in the sheet (§4.4); a sheet in the BYO
terminal panel (§6); using the sheet in Week Review or inbox triage (follow-ups, §8); a model
classifier (decision 2).

## 3. What a wrap asks today

| Step | Question | Shape | Asks |
| --- | --- | --- | --- |
| 5 | Tasks that look done | *Tick it / Not yet* | once per candidate |
| 6 | Today's open reminders | *Move to tomorrow / Let me pick / Leave them*, then 5 options per reminder | 1, or 1 + N |
| 7 | Open tasks | *All of them / None / Let me pick*, then *Soon / Someday / Leave it* per task | 1, or 1 + N |

## 4. The sheet

### 4.1 What it looks like

```
Where should today's loose ends go?
───────────────────────────────────────
TASKS
▸ Draft Q4 budget                  Done
    commit 3f1c2a "budget v1"
    [1 Done] 2 Soon  3 Someday  4 Leave
  Call the plumber                 Soon
    carried 2 days
  Read the Rust RFC             Someday
  Renew passport              needs you
REMINDERS
  Pay rent              Tomorrow 09:00
───────────────────────────────────────
Already in your backlog: Fix the bike
4 set · 1 needs you             ⏎ apply
```

- It renders **inline in the chat**, in the question bubble's place (V44 §5.5), with the
  agent's last message above it as for every question. Past about eight rows the card scrolls
  internally, and the selected row stays in view.
- **Groups** are titled sections, and each group has one fixed set of choices. Choices are the
  same for every row in a group, so the chips line up in columns and a number key means the same
  thing on every row.
- Each row shows its choice at the end, or a dim *needs you* when it has none. Only the
  **selected** row shows its chips, each with the number key that picks it, and its choice is the
  highlighted one (R3: the thread's column is 360px, too narrow for five chips beside the text).
- A row's **note** is one dim line under its text: the evidence for the suggestion, or context
  worth having ("carried 2 days", "due Fri"). It is optional.
- A **footnote** line, also optional, lists things the agent decided not to ask about, such as
  tasks already in the backlog.

### 4.2 Keys

Under the existing `ThockChatPanel` context, active while a sheet is pending:

| Key | Does |
| --- | --- |
| `up`/`down`, `j`/`k` (vim) | Move between rows |
| `g g` / `shift-g` (vim), `menu::SelectFirst`/`SelectLast` | First / last row |
| `left`/`right`, `h`/`l` (vim) | Move the selected row's choice to the previous / next chip |
| `1`–`9` | Set the selected row's choice to that chip and move to the next row |
| `enter` | Apply, if every row has a choice; otherwise move to the first row that needs one |
| `escape` | Fold, as for every V44 question; the sheet stays pending |

There is no "accept suggestion" key: a pre-selected choice is already the row's answer. Past the
sheet's first or last row, the row keys move on to the thread's rows, and `g g`/`shift-g` on a
sheet already at that edge do too.

### 4.3 Applying

`enter` with every row set sends the answers and collapses the sheet into a one-line receipt in
the thread: *Sorted 5: 1 Done, 2 Soon, 1 Someday, 1 Leave*. Choices are counted by label, across
groups, in the order the choices are listed; a choice nobody took is left out.

### 4.4 Wrap Today's sheet

Steps 5–7 of `wrap-today.md` become one step that builds the sheet, asks once and applies the answers:

| Group | Rows | Choices | Pre-selected when |
| --- | --- | --- | --- |
| **Tasks** | Unfinished `- [ ]` tasks in today's note, except `### Reminders` copies and tasks carrying a reminder due today | **Done**, **Soon**, **Someday**, **Leave** | **Done**: a commit or a note from steps 2–3 shows it finished (the evidence is the row's note). **Soon** / **Someday**: the agent can say why (a due date, how long it has lingered, the triage policy). Otherwise none. |
| **Reminders** | Open reminders due today (step 6's rule), including tasks that carry one | **Done**, **Tomorrow 09:00**, **Next week**, **Drop**, **Leave** | **Tomorrow 09:00** when the reminder is still relevant; **Done** only with evidence, as above. Otherwise none. |

- **Done** keeps step 5's rule that a task is assumed unfinished unless something shows otherwise. The
  agent never pre-selects it without a note citing the evidence.
- The hour in **Tomorrow 09:00** is `tomorrow_at`, as in step 6 today.
- **Pick a date** goes away. A reminder that needs a specific date is answered with **Leave** and
  edited in its note.
- Tasks already in the backlog get no row. They are listed in the footnote ("Already in your
  backlog: …"), so step 7's dedup moves earlier in the flow and the closure still records them as
  `→ already in backlog`.
- What each answer writes doesn't change: Done flips the box (or closes the reminder), Soon/Someday
  append to the backlog, and Tomorrow/Next week/Drop rewrite the reminder token on its home line.
  Leave keeps the task only in the note. The `# Daily Closure` section is written as before.
- With no rows at all, the step is skipped in silence, as each of steps 5–7 is today.

## 5. The `sort` tool

### 5.1 What the model calls

A second tool in `crates/thock/assets/hosted-agent/ask.ts`, next to `ask`:

```ts
sort({
  question: "Where should today's loose ends go?",
  groups: [
    {
      title: "Tasks",
      choices: ["Done", "Soon", "Someday", "Leave"],
      items: [
        { id: "t1", text: "Draft Q4 budget", note: "commit 3f1c2a \"budget v1\"", suggested: "Done" },
        { id: "t2", text: "Renew passport" },
      ],
    },
  ],
  footnote: "Already in your backlog: Fix the bike",
})
```

Validation in the tool, which throws so the model sees the error:

- 1–3 groups, 2–6 choices per group, 1–40 items in total.
- Item ids are unique across groups, and `suggested`, when present, is one of the group's choices.

It returns the answers as text and as `details`:

```
They sorted: t1 → Done, t2 → Someday
```

`sort` is a separate tool rather than an `items` form of `ask`, because it returns a map instead of
one label and is validated differently. Keeping them apart also leaves `ask` and every ritual that
uses it untouched.

### 5.2 How it reaches the chat and comes back

The pi-acp bridge (pinned at 0.0.33) only carries two kinds of request from an extension to the
app. `select` becomes a permission request, and the answer is one of its options. `confirm`
becomes a yes/no. `input` and `editor` are cancelled. Nothing it carries can return a map.

So the sheet travels on two existing paths and one small side channel:

1. **The sheet's content** is the model's own `sort` tool call. Its `rawInput` reaches the chat
   unchanged, as the `ask` call's does today (`is_ask_call`). The chat hides the call, as it hides
   `ask`'s, and builds the sheet from its `rawInput`.
2. **The pending question** is a `select` the tool raises with the title `sort:<toolCallId>` and a
   single option, `Apply`. pi-acp turns it into a `pi-ui-*` permission request as for `ask`, and
   the title ties it to the call in step 1.
3. **The answers** are written by the chat to
   `<pi_config_dir>/answers/<toolCallId>.json` (`hosted_agent::pi_config_dir`, Thock-owned and
   outside the vault), atomically, *before* it authorizes `Apply`. Both sides turn any character
   of the id outside `[A-Za-z0-9_-]` into `_`, since providers put `|` and the like in their ids,
   and that also keeps the file inside `answers/`. When the select returns, the
   tool reads the file, deletes it, and checks that every item has an answer from its group's
   choices. A missing or invalid file is an error the model reports. It is never treated as an
   empty answer.

```json
{ "t1": "Done", "t2": "Someday" }
```

If the select comes back cancelled (the session ends), the tool returns *They didn't answer.
Nothing moves.* Unlike `ask`, it never tells the model to carry on with a default (G3).

When pi-acp can forward a form elicitation, steps 2–3 collapse into one elicitation with one
property per item, and the chat's `ElicitationShape` gains a `Sheet` arm. The model-facing tool
doesn't change.

## 6. Outside the Thock Agent

The BYO terminal panel runs skills in whatever agent the person brought, and that agent has no
`sort` tool. Wrap Today's merged step starts with: *if you have no `sort` tool, skip this step*,
so the rest of the wrap (the review, Reflect) still runs and the closure lists open tasks
without moving anything. That isn't a second flow to maintain. It's the same way the skill already
treats a missing outside source.

## 7. Changes by file

| File | Change |
| --- | --- |
| `crates/thock/assets/hosted-agent/ask.ts` | Add `sort` (§5) next to `ask` |
| `crates/thock/src/hosted_agent.rs` | One line in `SYSTEM_PROMPT` on when to use `sort` (several items, each needing its own answer); `write_pi_config` empties `answers/`; tests for the extension and prompt strings |
| `crates/thock/src/chat_panel.rs` | `is_sort_call`, sheet state (selection, answers) built from the call's `rawInput`, the sheet render, the keys in §4.2, the answers file, the receipt line |
| `assets/keymaps/default-{macos,linux}.json`, `assets/keymaps/vim.json` | None needed: the existing `ThockChatPanel` bindings (`menu::Select*`, `ExpandChatActivity`/`CollapseChatActivity`, `AnswerChatChoice`, `menu::Confirm`) act on the sheet when it is selected; a sheet adds the `choice` context but never `choice_pair` |
| `crates/thock/assets/routines/timeline/skills/wrap-today.md` | Steps 5–7 become one step (§4.4); the `## 9. Reflect` and later numbers shift |
| `thock/VISION.md` §12 | Roadmap entry, updated when this ships |

## 8. Follow-ups

- **Week Review step 6** asks once per reminder in the same way, and is the next caller.
- **Inbox triage** (`triage-inbox.md`) through the hosted chat could use the same sheet with the
  inbox's destinations.
- **Desk-side pre-classification** with Jev: the desk would fill in rows the agent left blank,
  with v41's confidence chips. The tool and the skill wouldn't change (decision 2).

## 9. Tests

- **`hosted_agent.rs`:** `ASK_EXTENSION` registers `sort`, raises a `select` titled `sort:`,
  and reads `answers/`. `SYSTEM_PROMPT` mentions `sort`. The Wrap Today string test changes
  from `**Let me pick**` / `**Tick it** and **Not yet**` to the `sort` tool, the two groups and their
  choice labels.
- **`chat_panel.rs` keystroke tests** (`VisualTestContext`, `simulate_keystrokes`), on a stub
  connection replaying a `sort` call and its `pi-ui-*` request:
  - The sheet renders the groups, the pre-selected chips, the notes and the footnote. The `sort`
    call doesn't appear in the receipt.
  - `j`/`k` and `down`/`up` move rows. `l`/`h` move the choice. `2` sets the second chip and moves to
    the next row.
  - `enter` with an unset row selects that row and authorizes nothing.
  - `enter` with every row set writes exactly `{"t1":"Done",…}` to the answers path, then
    authorizes `Apply`. The sheet collapses to the receipt line.
  - Selection survives a re-render triggered by a vault file event.
  - `escape` folds, and the question is still pending afterwards.
- **Rendered text:** no `sort`, `t1` or JSON in the sheet (G5).

## 10. Decisions

| # | Question | Decision |
| --- | --- | --- |
| 1 | How much of the wrap goes in the sheet | **Steps 5–7 together.** One question per wrap. |
| 2 | Who pre-classifies | **The agent**, with a one-line reason per row. Jev on the desk is deferred: no calibrated confidence for now, but the agent can say *why*, which Jev can't. |
| 3 | Rows with no choice on `enter` | **Block**: `enter` moves to the first row that needs one. No silent default. |
| 4 | Where the sheet renders | **Inline in the chat**, and the card scrolls when long. One code path, same place as every question. |
| 5 | Fallback in other harnesses | **None.** Without `sort`, the merged step is skipped and the rest of the wrap runs (§6). |
| 6 | Tool shape | **A separate `sort` tool**, not an `items` form of `ask` (§5.1). |
| 7 | Transport | **Model call carries the sheet, `select` carries the pending question, a Thock-owned file carries the answers** (§5.2), until pi-acp forwards elicitations. |

## 11. Risks

- **R1: The tool call id at both ends. Confirmed 2026-10-28** by reading the installed
  sources of the pinned Pi 0.85.1 and pi-acp 0.0.33: Pi's agent loop calls `tool.execute(toolCall.id, …)` with the id from the assistant
  message, the extension wrappers pass it through unchanged, and pi-acp emits the ACP
  `tool_call` with that same `toolCall.id`. The id the extension puts in the `select` title is the
  id the chat sees on the `sort` call. Re-check when either pin moves. (pi-acp falls back to a
  random UUID only when an execution event has no id, which the agent loop never sends.)
- **R1b: The request can arrive before the arguments.** pi-acp queues session updates on a
  promise chain, but sends the permission request directly. In principle the `pi-ui-*` request
  could reach the chat before the final `tool_call_update` with the complete `rawInput`. The
  chat renders the sheet only when the request and a `rawInput` that parses as a full `sort`
  call are both present, and re-renders on every update to the call.
- **R2: The answers file outlives a crash.** The extension deletes it after reading. Stale files
  under `answers/` are harmless (ids are unique), and `write_pi_config` empties `answers/` when
  it prepares a session.
- **R3: Wide rows.** Five reminder choices plus the item text won't fit the chat's 360px column,
  so every row shows only its choice and the selected row shows its chips under it (§4.1).
