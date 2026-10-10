# Thock V38 - The backlog on the phone: move, tick, edit and remove tasks

**Status:** Implemented (2026-10-06); ships with the next phone build
**Owner:** Diego · **Date:** 2026-10-06
**Design reference:** the design document with the three proposals, the mockups and the decision
cards this spec resolves: <https://claude.ai/artifact/KoKfR3FuYfJM2zjH7rw2pw>. Layout questions go
there first; this spec records the decisions, the write contract and the sync rules.
**Companion docs:** `v6-backlog.md` and `v17-backlog-categories.md` (the file and the desk pane),
`v33-iphone-companion.md` §5, §7 and §14 (the Today canvas, the nudge menu, the write contract this
extends), `v34-vault-sync.md` and `v34-vault-sync-api.md` §7 to §9 (the write kinds and the fixture
corpus this adds to), `v37-week-plan.md` §8 (the user's lines are theirs to edit)

---

## 1. Summary

The phone can put a task into the backlog but cannot see it. V38 adds a **Backlog** screen that
draws `backlog.md` the way the Today canvas draws a daily note, and lets the person do with a task
what the desk pane does: tick it, change its words, add one, send it to today, move it between Soon
and Someday. It goes past the desk in two places the phone needs and the desk never did: a task can
be **moved anywhere** (up and down inside its group, into another category, into the other section),
and a task can be **removed**, with a few seconds of undo.

Moving is the point of the screen and it is a drag: press a row and move, it lifts, carry it, let
go. The long-press menu covers the long trips (*Move to… Someday › Thock*) and the exact one-step
case (*Move up*, *Move down*), so a move never needs a drag across the whole screen and every move
has a path VoiceOver can take.

Every gesture is one write, and the two new write kinds it needs, `move_block` and `remove_block`,
are the only desk-side change. They live in `thock_sync_core` with shared fixtures, so the desk and
the phone cut and place the same block by the same rules.

## 2. Goals & success criteria

- From today to the backlog in one tap, with the open counts visible before tapping.
- A task reordered on the phone opens on the desk in that order, with every category heading the
  desk had before, and the desk pane's selection survives the re-parse.
- Every write the screen makes changes one task block (the task line and its children) and nothing
  else; ticking changes two files, as on the desk. Prose, unknown sections, HTML comments and the
  Completed list stay byte-identical except for the one block.
- A move queued offline survives the desk's own appends: it is rebased by hash, never by line
  number, and a task the desk renamed meanwhile is left where the desk put it.
- A task moved on both ends between syncs ends up present exactly once.
- Nothing on the desk beyond the two sync-core kinds. No pane change, no new config.

## 3. Non-goals

- **Batch moves** (select several, send them somewhere together): the design document's proposal C.
  The write contract supports it (N writes, one toast) and nothing here closes the door.
- **Pick-up-and-place without dragging** (proposal B). *Move up* and *Move down* in the menu are
  the part of it that ships.
- **Un-completing a task.** A completion is also written in a daily note (V6 §6.3); retracting it
  silently would break append-only. Re-opening a task stays a desk edit, as V6 §6.6 decided.
- **Editing or showing a task's children.** They travel with the task and are hinted at (§5.2),
  nothing more.
- **Creating, renaming, reordering or removing sections and categories**, with one exception the
  desk already makes: a Soon ↔ Someday move from the menu recreates the task's same-named category
  in the destination when it is missing (§6.3).
- **Copy as Markdown** and **Reveal**, the desk pane's two clipboard-and-editor gestures.
- **Ask writing to the backlog.** V35's second pass, unchanged by this spec.

## 4. Entry

Decided: a **Backlog row** on today's canvas, under the Inbox row, opening a **full-screen
canvas**.

- The row reads *BACKLOG · 5 soon · 14 someday*, counts from the phone's copy of the file, drawn
  like the Inbox row. A vault with no `backlog.md` reads *BACKLOG · empty*. The row sits on today
  only; other days and the week canvas do not carry it, as they do not carry the Inbox row.
- Tapping it pushes the Backlog over the pager with a *‹ Today* control. It is a canvas, not a
  sheet: a list whose rows are dragged cannot live inside a sheet that itself dismisses on drag,
  and the list wants the full height. The quick-action row keeps its four buttons; the backlog is
  a place you go, not a thing you capture.
- `-thock-open backlog` and `thock://backlog` open it, like the other entry points.
- The *Move to Soon* nudge on a planner or goal line gains a *Show* button on its toast, so a task
  just sent to the backlog can be placed right away.
- Cold launch is still today (V33 H1). The app never restores to the backlog.
- While the phone is locked (V33 §4.6) the row does nothing, because the screen reads the vault.

## 5. The screen

### 5.1 What it draws

The file's own structure, in the file's order, by the rules the Today canvas uses (V33 §5):

```
┌──────────────────────────────┐
│ ‹ Today                   +  │
│ Backlog                      │   Petrona
│ 5 SOON · 14 SOMEDAY          │
│ ─ SOON · 5 ──────────────  + │   the configured Soon heading, open count, a dim add
│ ○ Renew passport             │   circle ticks, text edits, press-and-move drags
│ ○ Call the dentist           │
│   HOME · 3                 + │   a category: the planner's hashed group colour
│   ○ Fix the gate  +2 lines   │   children hinted, never shown
│   ○ Buy a smoke alarm        │
│ ─ SOMEDAY · 14 ──────────  + │
│ ○ Learn woodworking          │
│   THOCK · 4                + │
│   ○ Week widget              │
│ ─ DONE · 42 ───────────── ›  │   collapsed; expands in place, newest first, read-only
└──────────────────────────────┘
```

- **Sections and categories come from the file.** Soon, Someday and Completed are found by the
  vault's configured headings (`[backlog] headings`, the V19 names) with the English defaults as
  fallback, at any heading level, first match wins, exactly as the desk parses them (V6 §5.1). A
  heading deeper than its section's inside Soon or Someday is a category (V17 §4); loose tasks
  render first, then each category in file order.
- **A task** is a top-level checkbox line in one of the three sections. Its text is drawn with
  inline links as links, the trailing `<!--gmail:…-->` and `<!--inbox:…-->` markers hidden and
  preserved (the desk's `strip_trailing_comment` rule). A task that is already ticked inside Soon or
  Someday is hidden, as on the desk.
- **Done** is one collapsed row with the count. Expanded, it lists Completed newest first, struck
  through, with the `✅` date as a small label; undated hand-written completions sort last. It is
  read-only: no circle, no menu beyond nothing.
- **Empty states.** A missing file, a missing section or an empty section renders its label and an
  *Add a task* row. Nothing is an error, and looking writes nothing: the file is created from the
  desk's default only by the first add (the capture sheet's existing behaviour).
- **Collapsing a category** is a tap on its label. Device-local state, like the desk's; nothing is
  written.

### 5.2 Rows

| Part | Tap | Long press |
| --- | --- | --- |
| The circle | tick (§6.1) | the menu |
| The text | edit this line, in the planner's edit sheet | the menu |
| `+N lines` hint | nothing; it says the task has children that travel with it | |
| A category label | collapse or expand | |
| The `+` on a section or category | add a task at the end of that group, in the planner's add row. Drawn dim, so the rail stays quiet | |

The menu is the planner row's nudge menu (V33 §7.1) with the backlog's items, in this order:
**Tick**, **Edit this line**, **Move up**, **Move down**, **Move to…** (a submenu: Soon, Someday,
and every category under its section), **Move to today**, **Remove** (destructive, red). *Set a time*
is the planner's own and is not here.

## 6. Moves

Decided: **proposal A, Lift and drop**, with *Move up*, *Move down* and *Move to…* in the menu from
the first build.

### 6.1 The table

| On the phone | In the vault |
| --- | --- |
| Tick | two writes in one queue entry: `- [x] <text>` appended to today's planner (today created from its template if missing); then the block moved to the end of Completed with its line rewritten `- [x] <text> ✅ <today>`, children following, no category. The today append goes first, as V6 §6.3 orders it |
| Edit this line | one `replace_line` on the task line; checkbox state, hidden marker and children kept. Empty text reverts |
| Add | one `append` of `- [ ] <text>` at the end of the group the add started from, below the group's last task and above the next heading; the capture sheet's existing placement |
| Drop after a drag, Move up, Move down, Move to… | one `move_block` (§7) |
| Move to today | the block appended verbatim to today's planner (today created if missing), then one `remove_block`; the planner's *Move to Soon* in reverse and the desk's `t` |
| Remove | one `remove_block`, delayed a few seconds behind a toast with Undo, exactly like the planner's Remove. Undo drops the write before it is queued |
| Collapse, expand, scroll, open | nothing |

Every move inside the backlog ends with a toast (*Moved to Someday*, *Moved to Home*) whose
**Undo** queues the reverse move; the moved task is found again by its words. *Move to today* and
a tick show a toast without Undo, since each also wrote to today's note.

### 6.2 The drag

- Press and move starts it, from anywhere on the row; there is no handle, so the row's right
  edge stays empty. The row lifts with the
  system's haptic, the list scrolls itself near the top and bottom edges, and the system's drop
  line shows where the row will land. Letting go is the write. Press and hold without moving opens
  the menu, so the two never compete.
- The landing place is the group the line sits in and the row above it: `place: after` that row, or
  `top` when the line is directly under the group's label. Dropping on a collapsed category places
  at its end. Dropping on the Done row is refused with a nudge toward Tick.
- A drag across Soon and Someday is the same write as *Move to…*: destination group, place after
  the row above. The category rule of §6.3 applies.
- Dropping where the row already is writes nothing.

### 6.3 Categories

- A move inside a group changes order only.
- A move into another group changes the task's category; the task's children follow.
- A drag, or a category item in *Move to…*, lands in the exact group chosen.
- **Decided:** the menu's **section items** (*Move to… Soon*, *Move to… Someday*) behave like the
  desk's chevron (V17 §5.3): a categorized task keeps its category, and the same-named category is
  created at the end of the destination section when it is missing (`create_heading: true`); a loose
  task stays loose. This is the one heading the phone ever creates, and only inside a backlog
  section. The planner's *Move to Soon* is unchanged and lands loose.
- An emptied category keeps its heading, as on the desk.

## 7. The write contract (V33 §14, extended)

| Where | Added by V38 |
| --- | --- |
| `backlog.md` | append one task (unchanged); replace one task line; remove one task block; move one task block; append to Completed as part of a tick |
| `daily/<today>.md` | append `- [x] <text>` on a tick and a verbatim block on *Move to today*, both at the end of the planner's own lines, above its subsections; the V33 placement |
| everything else | unchanged. The phone still never touches `memory/` beyond V35's inbox line, `routines/`, templates or the triage log |

The phone still never adds, renames, reorders or removes a section or a category.

### 7.1 Two block-aware kinds

A planner line has no children; a backlog task often does. The two new kinds operate on a
**block**: the task line plus every indented, non-blank line after it, with blank lines *between*
children included and trailing blank lines excluded. This is the desk's own span rule
(`crates/thock/src/backlog.rs`), so both ends cut the same block. On a line that has no indented
lines after it, a block is one line, and the kinds are the single-line kinds with a different name.

```json
{ "kind": "move_block",
  "heading":   { "text": "Home", "level": 3, "ordinal": 0 },
  "line_hash": "9f1c…", "ordinal": 0,
  "to":        { "text": "Home", "level": 3, "ordinal": 0 },
  "place":     { "after": { "line_hash": "4b7e…", "ordinal": 0 } },
  "new_line":  null,
  "create_under": { "text": "Someday", "level": 2, "ordinal": 0 } }
```

- `heading`, `line_hash`, `ordinal` name the source line as `replace_line` does (V34 API §7.4): the
  group the task is in, the hash of its words, its ordinal among same-hash lines of that group's
  own lines (under its heading, above its first subsection).
- `to` names the destination group. `place` is `"top"`, `"end"`, or `{ "after": { line_hash,
  ordinal } }`, an anchor task in the destination group. Default `"end"`.
- `new_line` (optional) replaces the task line as it lands; the children are copied verbatim. A tick
  uses it for the `[x]` and the date stamp.
- `create_under` (optional) names the section `to` lives in. With it, `to` is looked for inside
  that section only, so `Someday › Home` is never Soon's Home, and is created at the end of the
  section when missing. Without it, `to` is found anywhere and, when missing, created where an
  append would create it.

`remove_block` takes `heading`, `line_hash` and `ordinal` and removes the block.

### 7.2 Rules

| Case | Outcome |
| --- | --- |
| Source found, destination and anchor found | the block is cut and placed; `applied` |
| The block already sits in the destination at that place | nothing changes; `noop` (effect present, so a retried write is idempotent, V34 §8.3) |
| Source missing | `noop`. The task was renamed, moved or completed at the desk meanwhile. A move must never become a copy, so there is no kept-both here. The phone shows the desk's version after the next pull and the person moves it again |
| Anchor missing | placed at the end of the destination group; `applied`. The order is slightly off, nothing is lost |
| Destination missing, `create_under` given | heading created at the end of that section, block placed under it; `section_added` |
| Destination missing, no `create_under` | heading created as an append would create it; `section_added` |
| Cutting the block leaves two blank lines touching | one goes with the block, so neither end ever writes a double blank |
| Source and destination are the same group and `place` resolves to where the block is | `noop` |
| `remove_block`, source found | block removed; `applied` |
| `remove_block`, source missing | `noop` |

- **Addressing is by hash, not position**, so a move queued offline is rebased onto a newer
  snapshot the way the single-line kinds are (V34 §8.2) and survives the desk's own appends.
- **Two ends reorder the same group**: moves apply in arrival order on the current text. The result
  is always a valid order with every task present exactly once; it may be neither person's exact
  order, which is the honest outcome.
- **Ticking is two writes in one queue entry** so they flush together and in order: the today
  append first, the move second. Both apply locally and cannot half-apply on the phone; on the desk
  the queue is drained in order, so the today note is never behind the backlog.
- **`effect_present`** for `move_block`: the block (with `new_line` applied if given) is in the
  destination group at the named place. For `remove_block`: no line in the source group has the
  hash and ordinal.

### 7.3 Where it lives

Both kinds live in `crates/thock-sync-core` (`write.rs`, `apply.rs`) and in
`thock/ios/ThockKit/Sources/ThockKit/SyncCore/` (`WriteDocument.swift`, `Apply.swift`), with the
fixture corpus extended by `fixtures/v1/move_block/` and `fixtures/v1/remove_block/` that both
runners pass. The phone's model and builders are `ThockKit/Vault/BacklogModel.swift`; the screen is
`Thock/BacklogScreen.swift`. `WRITE_VERSION` stays 1: an older desk that meets an unknown kind holds the write and
everything queued behind it, and its status row says to update Thock (V34 API §10.4), so a phone built
before the desk is updated loses nothing and the Backlog screen reads *waiting for the desk* on its toast
until the desk catches up.

## 8. Tests

- **Shared fixtures** (`crates/thock-sync-core/fixtures/v1/{move_block,remove_block}/`, run by the
  Rust tests and `SyncCoreFixtureTests`): move within a group, to the top, after an anchor, into
  another category, Soon → Someday loose, Soon → Someday with `create_heading`, a task with children
  and a blank line between them, `new_line` on the way to Completed, source missing, anchor missing,
  destination missing with and without `create_heading`, remove with children, and the second
  application of every write being a `noop`.
- **ThockKit builders** (`VaultTests`): every row of §6.1 against the sample vault's `backlog.md`
  changes exactly the block's lines; tick changes two files, today first; a task with children
  moves with them; the hidden Gmail marker survives an edit and a move; the Backlog row's counts
  match the parse; a vault whose headings are translated (V19) parses through its configured names.
- **Desk round-trip** (`crates/thock`): a backlog moved by a fixture's `after` parses into the same
  tasks in the new order with every category the desk had before; the pane's selection survives.
- **On the simulator** (`thock/ios/script/smoke`): a scripted move to Someday, a tick and a move
  to today (`-thock-script "backlog-move:…;backlog-tick:…"`) leave the expected `backlog.md` and
  today's note on the phone and at the practice desk. A drop where the row already is writes
  nothing, by the fixture `dropping where it is changes nothing`.
- **Conflict** (`SyncTests`): the same task moved on both ends between syncs ends up present exactly
  once; a task renamed at the desk and moved on the phone leaves the desk's rename and the phone's
  move a `noop`, with the phone showing the renamed task where the desk put it.
- **Locked:** the Backlog row does nothing while the phone is locked.

## 9. Decision log (2026-10-06)

From the design document's decision cards, with Diego's answers:

1. **Proposal A, Lift and drop.** Native drag from the row; *Move up*, *Move down* and *Move to…*
   in the long-press menu from the first build.
2. **Entry: a Backlog row under the Inbox row on today's canvas.** It carries the counts; quick
   actions stay for capturing.
3. **A full-screen canvas, not a sheet.** Dragging rows inside a dismissible sheet misfires, and the
   list wants the height.
4. **Remove on the phone: yes, with the Undo toast.** One line the person tapped on purpose,
   reversible for a few seconds, within the invariants. The desk keeps no delete.
5. **Move to today: yes.** The planner's Move to Soon in reverse, one menu item.
6. **Category on Soon ↔ Someday: create the same-named category**, desk parity, through the menu's
   section items (§6.3). A drag lands exactly where it was dropped.
7. **Children: a `+N lines` hint.** Expanding can follow.
8. **The Done list: a collapsed row with a count**, expanding in place; an audit trail, not a
   workspace.
9. **Two new kinds, `move_block` and `remove_block`.** Teaching `remove_line` to take children
   would change the planner's behaviour on every vault.

## 10. Decision log (2026-10-07)

1. **No drag handle, dim add buttons.** The `≡` on every row and an amber `+` on every group made
   the right edge busier than the tasks. The drag already starts anywhere on the row, so the handle
   went; the group `+` stays but is drawn dim, leaving the nav bar's amber `+` as the one bright
   control.
