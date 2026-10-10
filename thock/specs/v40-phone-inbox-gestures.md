# Thock V40 - Inbox gestures on the phone: swipe to today, to the backlog, or away

**Status:** Implemented on the phone (2026-10-10); the desk's `move_file` and the shared fixtures
land with the sync-core change
**Owner:** Diego · **Date:** 2026-10-10
**Design reference:** the design notes with the gesture-to-writes diagram and the decision cards
this spec resolves: <https://claude.ai/artifact/Y81PSwkFpv7zxHtzwi9MVE>.
**Companion docs:** `v13-inbox-routine.md` §4, §9.5 (what an inbox item is, the triage log),
`v33-iphone-companion.md` §14 (the write contract this amends), `v34-vault-sync-api.md` §4.1, §7
and §9 (paths, kinds and fixtures), `v38-phone-backlog.md` §5.2 and §7 (the undo toast and the
block kinds this follows), `v39-vault-images.md` §6 (ships the other new kind on the same
sync-core change)

---

## 1. Summary

The phone's inbox screen lists what is waiting for the desk and lets the person edit an item's
words. Everything else waits for the Triage Inbox ritual. V40 adds the three decisions that need no
ritual: **this is for today**, **this is for later**, **I'm done with this**. Each is a swipe on the
row, with the long-press menu as the second path, and each is a few writes the desk applies after a
checkpoint.

The desk's ritual deletes an item once it is filed. The phone cannot delete, and should not: a
capture is often more than a title, and after V39 it can carry a picture. So the phone **moves** the
note to `archives/inbox/` and writes the task line with a wikilink back to it when there was a body
worth keeping. That is the one new write kind, `move_file`, and it lives in `thock_sync_core` with
fixtures both ends pass.

## 2. Goals & success criteria

- Swipe a waiting item right and it is a task under today's Day planner; swipe it right and choose
  Backlog and it is a task under Soon; swipe it left and it is out of the inbox. One sync round
  later the desk agrees, and the Triage Inbox ritual no longer offers the item.
- A few seconds of undo on every gesture, like removing a backlog task (V38 §5.2). Nothing is
  written until the toast expires.
- The item's words are never lost: the note is in `archives/inbox/` byte-for-byte, and the triage
  log says where the item went, in the ritual's own line format, so the desk's receipts and the
  phone's agree.
- A gesture on an item the desk triaged meanwhile is a no-op, never a resurrection and never a
  duplicate task.
- Every gesture has a path VoiceOver can take.
- No desk change beyond the sync-core kind. The ritual's own filing is untouched.

## 3. Non-goals

- **Deferring** (`deferred:` in the frontmatter) and **keeping as a note** in a chosen folder: the
  ritual's judgement calls, with a policy file behind them. The phone makes the three decisions that
  have no policy.
- **Choosing a Day planner section, a backlog category or a time.** Today lands under the planner
  heading, Backlog under Soon; the desk and the Backlog screen move it from there.
- **Un-archiving.** The note is in `archives/inbox/` in the open; the desk opens it like any file.
- **Gestures on items the ritual already handled** (filed, discarded, gone): the row shows its
  receipt and has no actions.
- **Batch gestures.** One row at a time.

## 4. The screen

`ReceiptsScreen` becomes a `List` with a plain style, hidden separators and no inset, as
`BacklogScreen` already is, so SwiftUI's own swipe actions apply. Rows keep their shape (dot, title,
detail line, chevron on an editable row).

| Gesture | Actions, in order from the edge | Full swipe |
| --- | --- | --- |
| Leading (swipe right) | **Today**, **Backlog** | Today |
| Trailing (swipe left) | **Archive** | Archive |
| Long press | Today · Backlog · Archive · Edit | — |

Only rows in the *waiting* state have actions; a row with a receipt (filed, discarded, gone) and a
row the phone cannot see the content of show none. Other devices' waiting notes
(`waitingInboxNotes`) get the same actions as this phone's captures.

A gesture removes the row at once and shows the toast V38 §5.2 introduced, worded for the move
(*Added to today · Undo*, *Added to the backlog · Undo*, *Archived · Undo*). The writes are
recorded when the toast expires, when another gesture replaces it, when the sheet is dismissed, or
when the app goes to the background, so a toast is never lost. Undo puts the row back and writes
nothing.

## 5. The writes

Every gesture ends with the note out of `inbox/` and one line in the triage log. Today and Backlog
add a task line first. All of a gesture's writes go in one flush, in this order, so the desk drains
them together:

| Gesture | 1 | 2 | 3 |
| --- | --- | --- | --- |
| Today | `append` `- [ ] <title>[ [[stem]]]` under the Day planner heading of today's note, `create_from_template`, `placement: before_children` | `move_file` `inbox/<name>` → `archives/inbox/<name>` | `append` the log line to `archives/inbox/triage-log.md` |
| Backlog | `append` the same task line under **Soon** in `backlog.md`, `placement: before_children` | the same `move_file` | the same log line |
| Archive | — | the same `move_file` | the same log line |

- **The task line is the title**, as the ritual writes it (V13 §9.3). When the note has a body
  beyond its heading (any non-blank line after the `# Title` and its frontmatter, including an image
  line), the task carries an inert `[[<stem>]]` to the archived note, the way an archived email's
  task does (V15). A title-only capture carries no link. The stem is the file name without `.md`;
  the desk's wikilink resolver matches it by basename, so `[[2026-10-10-0931-call-ana]]` opens
  `archives/inbox/2026-10-10-0931-call-ana.md`.
- **The log line** is the ritual's own format (V13 §9.5), with the phone's three destinations:

  ```markdown
  - 2026-10-10 · Call Ana → Today · Day planner <!--inbox:4d1f9a02c7b3-->
  - 2026-10-10 · Call Ana → Backlog · Soon <!--inbox:4d1f9a02c7b3-->
  - 2026-10-10 · Call Ana → Archived <!--inbox:4d1f9a02c7b3-->
  ```

  The digest comes from the note's `capture:` frontmatter when it has one (every phone and Gmail
  capture does); a hand-written inbox note gets a line without a marker. The log file is created
  with no heading when missing, as the ritual creates it.
- **The archive name** is the inbox file name unchanged. `archives/inbox/` holds the log and now the
  phone's archived notes; a collision (the same name archived twice) is left to `move_file`'s rules.

## 6. `move_file`

```json
{ "kind": "move_file",
  "path": "inbox/2026-10-10-0931-call-ana.md",
  "to":   "archives/inbox/2026-10-10-0931-call-ana.md" }
```

`path` is the source, as on every write; `to` is the destination, a string. The key is the one
`move_block` uses for its destination heading; a reader picks the shape by `kind`. Both paths must
be syncable (V34 API §4.1). The desk also checks that `to` is under a folder the phone may move into, which in
V40 is exactly `archives/inbox/`; any other destination is refused and logged, never applied.

### 6.1 Rules

| Case | Outcome |
| --- | --- |
| A file at `path`, nothing at `to` | renamed through the project `Fs`, parent folders created; `applied` |
| Nothing at `path`, a file at `to` | nothing changes; `noop` (effect present: a retried write is idempotent) |
| Nothing at `path`, nothing at `to` | nothing changes; `noop`. The desk triaged the item meanwhile: it was filed and deleted, or kept as a note elsewhere. A move never resurrects |
| A file at both | nothing changes; `noop`, logged. A `move_file` never overwrites; the person still sees the item on the desk and files it there |
| `path` or `to` fails the path rule, or `to` is outside `archives/inbox/` | refused: skipped and acked (V34 API §10.4) |

- **`effect_present`**: no file at `path` and a file at `to`.
- **Snapshots**: the desk's next scan publishes a tombstone at `path` and a create at `to`, which is
  how a desk rename already looks on the wire (V34 API §4.1). The phone's pull then sees both and
  the item leaves `waitingInboxNotes` on every device.
- **Locally** the phone applies the move at once (the row at `path` becomes the row at `to`), so
  the screen is right before the desk wakes.
- **Rebase** (V34 API §8.4): `move_file` has no text to rebase against. A snapshot that tombstones
  `path` before the write is acked means the desk handled the item; the write stays queued and
  lands as a `noop`.
- **Order against the task line**: the task `append` goes first so that, if the desk's drain is cut
  between writes, the worst case is a task that still has its inbox note, which the ritual then
  offers again and the person sees twice. The reverse order could lose the words.

### 6.2 Where it lives

`crates/thock-sync-core` (`write.rs`, `apply.rs`) and `ThockKit/SyncCore` (`WriteDocument.swift`,
`Apply.swift`), with `fixtures/v1/move_file/` that both runners pass. Fixture cases for a kind that
touches two paths carry `before` and `after` as objects keyed by path; both runners gain that
branch, shared with V39's `put_file`. `WRITE_VERSION` stays 1: an older desk holds the kind and
everything behind it (V34 API §10.4), so a phone updated before the desk loses nothing and the toast
reads *waiting for the desk*.

The desk-side folder check and the V34 API amendments (§4.1 the phone-move note, §7.3 the kind,
§9.2 the area and the keyed fixture shape, §13 the changelog) land with the sync-core change.

## 7. The write contract (V33 §14, amended)

V33 §14 said the phone never touches the triage log and only edits an inbox note while it waits.
V40 amends the table:

| Where | How |
| --- | --- |
| `inbox/*.md` | create-if-missing; while it waits for triage, replace its level-1 heading line and that heading's section; **move it to `archives/inbox/` (V40)** |
| `archives/inbox/*.md` | **the destination of that move; never written otherwise (V40)** |
| `archives/inbox/triage-log.md` | **append one line in the ritual's format, for a move the phone made (V40)** |
| `daily/<today>.md` | … and **append one task line under the Day planner heading for an inbox item (V40)** |
| `backlog.md` | append one task under **Soon** (unchanged; V40 adds a second reason to) |

Everything else in the table and the paragraph after it stands. V13 §4.2 stands too: a file in
`inbox/` is untriaged, and the phone now has three ways to make it not be.

## 8. Receipts

`ReceiptState` gains `archived(day:)`, decided like `filed` from the log line whose destination key
is `archived`. The row's detail reads *archived · Oct 10*. Today and Backlog gestures produce the
existing `filed` state with the destination the log line names, so a capture made on this phone and
moved on this phone reads *Today · Day planner · Oct 10* like one the ritual filed.

## 9. Tests

- **Sync core**: `move_file` fixtures for every row of §6.1; the keyed before/after shape; the
  second-apply idempotence the runner already checks.
- **Desk sync** (gpui): a `move_file` renames through `Fs` and the next scan uploads a tombstone
  and a create; a destination outside `archives/inbox/` is refused and acked; both-present leaves
  both.
- **Phone builders** (ThockKit): each gesture emits its writes in §5's order with the right task
  line (with and without the `[[stem]]`), the right log line (with and without the digest), and the
  right `move_file`; the local store shows the row at `to` right away; `Receipts.state` reads
  `archived`.
- **Phone screen**: the smoke script gains `inbox-today:<title>`, `inbox-backlog:<title>` and
  `inbox-archive:<title>` steps that drive the gesture and assert the queue.

## 10. Decision log (2026-10-10)

1. **Move to `archives/inbox/`, not delete.** One kind serves all three gestures, multi-line
   captures with pictures stay findable in the open, and the desk's own ritual keeps deleting as it
   does. A `delete_file` kind was the alternative; the body would then live only in invisible
   history.
2. **The task line is the title with a wikilink when there was a body**, not the body as
   continuation lines. The archived note is the body's home; the task points at it, as an archived
   email's task does.
3. **Writes are recorded when the undo toast expires**, not optimistically with a compensating
   write. A move that was undone must leave no trace on the wire.
4. **Today lands under the Day planner heading**, not a section the person picks: the gesture is
   for the decision that needs no decision. The time and the section are one tap away on today's
   canvas.
