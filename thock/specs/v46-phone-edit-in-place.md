# Thock V46 - Edit in place on the phone, and come back to where you were

**Status:** Implemented on the phone and in `thock-sync-core` (2026-10-23).
**Owner:** Diego · **Date:** 2026-10-23
**Companion docs:** `v33-iphone-companion.md` §4.6, §5, §9 (the lock, the canvas, the journal),
`v37-week-plan.md` §6, §8 (prose edits, `SectionScreen`), `v38-phone-backlog.md` (the backlog
canvas), `v34-vault-sync-api.md` §4 (write kinds)

---

## 1. Summary

Two complaints from using the phone every day:

1. **Text that looks like mine doesn't open when I tap it.** A journal entry opens the journal, but
   not the entry: the sheet starts a new one at the bottom, and the bullets under an entry can't be
   edited at all. A section the user wrote with a level-1 heading below the planner is drawn in the
   agent's amber and is read-only. Text above the first section isn't editable either.
2. **Leaving the app throws away where I was.** Switching to another app while writing in the
   journal commits and dismisses the sheet; coming back unlocks onto today's canvas. Any day,
   week or backlog the person was reading is gone too.

V46 makes every block of the user's own prose a tap away from being edited, and makes the app
resume exactly where it was left: same canvas, same sheet, same entry, same cursor. The one place
that is not restored is today's canvas with nothing open: that state is already "home", and the
app reopens on (the new) today.

## 2. Goals & success criteria

- Tapping any paragraph, bullet or quote the user wrote opens it live in the editor, on any card
  that holds the user's words: journal, prose sections (whatever their heading level), the text
  above the first section, and the notes under a planner group.
- Tapping a journal entry opens *that* entry, with the bullets and quotes that belong to it.
- Leaving the app from the journal, a section editor, Ask, a capture, the backlog, another day or a
  week, and returning (after Face ID), shows the same screen with the same text, scroll position
  and cursor. The keyboard is up if it was up.
- This holds across the app being terminated in the background, not only suspended.
- Nothing typed is lost, and nothing the person wrote is ever overwritten by a stale restore.
- The vault is never visible before the owner is present: not behind the lock, not in the app
  switcher's snapshot.

Non-goals: editing the agent's sections, headings, tables, code blocks or the note title;
numbered-list editing (§4.8); a cross-device "continue on the desk" handoff.

## 3. What the review found

Every surface that draws vault text, and whether a tap edits it today:

| Surface | Today | V46 |
| --- | --- | --- |
| Journal card, an entry | Opens the journal on a *new* entry | Opens that entry (§4.1) |
| Journal card, bullets/quotes under an entry | Not editable anywhere | Part of the entry (§4.2) |
| Journal on a past day | Not tappable | Existing entries editable; no new entry (§4.3) |
| Prose section, `##` heading | Paragraph, bullet, quote editable | Unchanged |
| Prose section, user's own `#` heading after the title | Drawn as the agent's voice, read-only | Prose (§4.4) |
| Preamble (text between the title and the first section) | Read-only | Editable like prose (§4.5) |
| Planner group notes (prose under `### Work` etc.) | Read-only | Editable (§4.5) |
| Planner's "Add a line" | Drawn below the group's notes | Below the last task; a second row adds text (§4.6) |
| Planner and checklist lines | Tap ticks, long-press edits | Unchanged (§9 D3) |
| Numbered items | Read-only (the editor writes bullets back) | Unchanged, follow-up (§4.8) |
| Agent sections (`# Daily Closure`, `# AI Week Review`, Keep this) | Read-only | Unchanged |
| Backlog task, inbox item | Tap edits | Unchanged |
| Ask answers | Read-only, *Keep this* | Unchanged |
| Headings, tables, code, comments | Read-only | Unchanged |

Two causes are worth naming because the fix is in the model, not the view:

- `NoteView` (`NoteModel.swift`) reads **any** level-1 heading after the title as the agent's voice
  (`CardKind.agent`). A person who types `# Interview prep` at the desk gets an amber, read-only
  card on the phone.
- `Journal.entries` is one entry per *paragraph*. The phone's own journal write
  (`journalAppend`) produces a stamped paragraph followed by bullets when the person types a list,
  so its own output is only partly editable.

A journal-looking paragraph drawn without a `JOURNAL` label (a `**11:31** · …` line above the
planner) is the preamble card: the line sits before the note's first section. §4.5 makes it
editable; whether the desk or an older phone build wrote it there is worth checking against the
note file before closing this out.

## 4. Edit in place

### 4.1 A tap opens the tapped thing

The rule for every card: **a tap on a block of the user's prose opens the editor with that block
live; a tap on the card's empty space, prompt or "Add a line…" starts a new one.** This is what
`ProseCard` already does; the journal card and the new editable cards adopt it.

`AppSheet.journal` gains the day and the entry: `.journal(day: VaultDay, entry: EntryRef?)`.
`JournalCard` puts the tap on each entry instead of on the whole card. `JournalScreen` takes the
day as a parameter instead of reading `.today()`, so an entry opened before midnight still saves
to the day it belongs to.

### 4.2 A journal entry is a paragraph and what hangs under it

An entry becomes a paragraph plus the bullet, task-free list and quote blocks directly after it,
up to the next paragraph, heading, rule or opaque block. `JournalEntry` gains the span
(`lines: Range<Int>`, `blocks: [Block]`) and keeps `time` from the paragraph's stamp.

Editing opens all of the entry's blocks in `RichTextEditor`, which already writes bullets and
quotes. Saving goes through `replaceParagraph(at:source:with:…)` with the whole span as `source`;
a multi-line source already takes the `replaceSection` path guarded by the section hash, so a desk
edit in between is a conflict, never a loss. The stamp is re-attached to the first line as today.

An entry containing a checklist line or an opaque block stops before it: those keep their own
affordances and are never rewritten by a paragraph edit.

### 4.3 Past days' journals

Existing entries on any day are editable, like prose sections already are (V37 §6). *New entry*
appears only on today's journal: a stamp on a past day would claim a time that never happened.

### 4.4 Only the agent's headings are the agent's voice

`VaultConfig` gains `agentHeadings`: the level-1 headings Thock's shipped skills write after a
note's title (`Daily Closure`, `AI Week Review`, `Reading Week`, the Money ritual's `Money`) and
the one *Keep this* appends under (`Asked on the go`). The phone does not read Routine manifests,
so a Routine whose skill appends a new level-1 section adds it to this list. A level-1 heading
after the title is `.agent` only when its key (`SyncCore.headingKey`) matches the list; any other
one is `.prose`, with its subheadings inside it as today. The weekly `# Tentative` and
`# Personal` sections that Week Review reads are the person's, and now edit as such.

This is phone-side classification only. The sync rule that a *new* heading is inserted before the
first non-title level-1 heading (`Apply.addHeading`, fixture `AGENT_HEADINGS`) is unchanged, so
the desk and the phone still agree on placement. `proseHeading(in:)` keeps skipping agent cards;
the user's own `#` sections become valid targets.

### 4.5 The preamble and planner notes

- **Preamble.** Its blocks are the title heading's *own* lines. A replace is a `replaceLine` (or
  the multi-line `replaceSection`) under the title's `HeadingRef`; "Add a line…" is an `append`
  with `placement = .beforeChildren`, which lands before the first section. A note with no title
  heading has a preamble with no heading to write under: it stays read-only, and the spec's tests
  pin that.
- **Planner group notes.** The prose under a planner subsection (`PlannerGroup.notes`) opens in
  `SectionScreen` scoped to that subsection's heading. Calendar groups stay read-only: the desk's
  calendar sync owns them.

Both use the existing write kinds. The preamble's `beforeChildren` append on a level-1 title gets
a shared fixture in `thock-sync-core` so the desk applies it identically (§7).

### 4.6 Two add rows in the planner: one for tasks, one for text

A single-line idea sent to Today lands in the planner as a paragraph (`capture`, `.today`), set
apart from the tasks by a blank line. `PlannerCard` draws a group as its items, then its notes, then
`AddLineRow`, so once an idea has landed, "Add a line" sits under the text instead of under the
list it adds to.

Each planner group is drawn in this order:

1. The tasks.
2. **Add a line**: a new task, directly under the last task. Unchanged behaviour (`addLine`).
3. The group's notes, if any, each editable (§4.5).
4. **Add a note…**: opens `SectionScreen` on the group with no line live; *Done* appends a
   paragraph to the group's own lines (`appendParagraph` with `placement = .beforeChildren`), so
   it lands with the other notes and never inside a subsection.

Row 4 shows whether or not the group has notes yet, so open text always has a place to start, and
it is drawn dimmer than row 2 so the task row stays the obvious one. The calendar and the day's
Reminders section get neither: the desk writes them.

**Where the task lands in the file.** `addLine` appends at the end of the planner's own lines, so
in the file the new task follows the notes even though the phone draws it above them, and the desk
shows the list split by the paragraph. `append` gains an anchor like `move_block`'s
`place: after`: `addLine` names the group's last task, and the line is inserted after it and its
indented continuation. With no task yet, it keeps today's placement.
This is a sync-core change with shared fixtures both ends pass (§7). The anchor is a new optional
`after: {line_hash, ordinal}` on `append`, sent only when set. A desk that predates it ignores
the field (serde skips unknown fields on the operation) and applies the write with its
`placement`, which is where the task landed before V46.

### 4.7 The inbox's Done button

`ReceiptsScreen` draws `SheetHeader` without the horizontal padding every other sheet gives it,
so *Done* sits against the sheet's right edge and is clipped by its rounded corner. The fix is
that padding on the header. Every other call site sits in a container padded by 20 points (the
capture, clip, You and share sheets) or pads the header itself (journal, section, Ask), so moving
the inset into `SheetHeader` would double it everywhere else.

### 4.8 Not in V46

Numbered items stay read-only until the editor can write `1.` back as `1.`. Tracked as a
follow-up in the editor (`EditorDocument`), not here.

## 5. Come back to where you were

### 5.1 The resume point

`AppModel` keeps a `ResumePoint`, saved on every change of screen and on the way to the
background (the name avoids sync-core's `Place`, which positions a `move_block`):

```swift
struct ResumePoint: Codable {
    enum Canvas: Codable { case day(VaultDay), week(VaultWeek), backlog }
    var canvas: Canvas
    var sheet: SheetPoint?        // journal, section, ask, capture, clip, receipts, calendar, you
    var editor: EditorPoint?      // the live block's identity, its draft, cursor and scroll anchor
    var leftAt: Date
}
```

It is a JSON file, `resume.json`, in the app group's container with complete file protection, so
it survives termination and a new build. It holds no vault text except the draft of a capture or
a clip, which is the person's own unsent words and pictures (§5.3). It is read and deleted on the
way back.

**Identity, not line numbers.** `AppSheet.section` carries a card's line index today. A desk sync
while the phone is away can move every line, so a saved resume point names things the way writes do: a
section by `HeadingRef`, a block by its line hash and ordinal under that heading, a journal entry
by its stamp and line hash. Restoring resolves those against the current note; §5.4 covers a
target that's gone.

### 5.2 What is dismissible

**Today's canvas with no sheet open** is home. Leaving from there saves no resume point, and the return
lands on today, which is the current behaviour (`goToToday` on every return) and also how a phone
left overnight shows the new day.

Everything else is restored, regardless of how long the app was away: another day, a week, the
backlog, and every sheet. A day canvas that *was* today when the person left but is yesterday on
return counts as home: they were looking at "today", not at a date.

Explicit entry points still win: a widget, the Action Button, a quick action or a `thock://` link
opens what it asks for. The journal and section editors were committed on the way out (§5.3), so
following the shortcut loses nothing. A capture shortcut over a saved capture reopens that
capture with its draft. Any other entry point over a saved capture that is no longer on screen
saves it where it was going, which is what leaving the app did to a capture before V46; a clip
in progress is dropped.

The sheets a checklist row opens (*Edit this line*, *Set a time*, *Remind me…*) stay up while the
app is only suspended, but are not part of the resume point.

### 5.3 Leaving an editor

| Editor | On the way to the background |
| --- | --- |
| Journal, section | Commit what the editor holds (as today), then **stay**: the live block becomes the block just written, so continuing to type replaces it instead of starting another one |
| Capture, clip | Save the draft into the resume point; nothing is written to the vault until *Save* |
| Ask | Nothing to save; the turn in flight already has its background grace (`askGrace`) |

The journal and section editors write through because what they write can be edited in place
afterwards, and the desk should see it without waiting for the phone to come back. A capture's
destination is chosen at the end, so a half-written capture must not become an inbox item.

`JournalScreen` and `SectionScreen` stop calling `dismiss()` on `.background`. The commit stays,
and after an append the editor switches `editing` to the new block (the journal already finds it
through `journalEntryToContinue`; sections get the same lookup).

### 5.4 Coming back

1. The scene goes `.inactive`: a **privacy cover** (the `LockedView` ground, no text) goes over the
   whole window, sheets included, so the app switcher's snapshot shows nothing of the vault. This
   applies even when the app is unlocked.
2. On `.active`: if the resume point reads the vault (anything but a capture or clip sheet), the cover
   stays and Face ID is asked for, as `becameActive` does today. A capture or clip sheet shows at
   once, as today: writing needs no unlock.
3. After the unlock: the canvas and sheet from the resume point are presented without animation, the
   live block is resolved by identity, the draft (capture) or the written text (journal, section)
   is in the editor, the cursor and scroll anchor are restored, and the keyboard comes up if it was
   up. Then the cover fades.
4. If the target block is gone or changed at the desk, the editor opens on a new block in the same
   section with a toast: *"That line changed at the desk."* Text the person typed has already been
   written (§5.3), so this is never a loss. If the whole note or section is gone, the resume point falls
   back to the canvas of that day.
5. Cancelling Face ID leaves the cover with *Show my notes*, as `LockedView` does now; the resume point is
   kept for the next try.

## 6. Changes by file

- `ThockKit/Vault/NoteModel.swift`: entry spans (§4.2); `agentHeadings` classification (§4.4);
  `isEditable` covers preamble blocks and planner notes.
- `ThockKit/Vault/VaultConfig.swift`: `agentHeadings`.
- `ThockKit/SyncCore/WriteDocument.swift`, `Apply.swift` and `crates/thock-sync-core`: `append`
  accepts `place: .after(…)` (§4.6).
- `ThockKit/Vault/PhoneWrites.swift`, `VaultSession.swift`: span-aware `journalReplace`; preamble
  replace and append; lookup of a just-written section block.
- `ThockKit/Vault/ResumePoint.swift`: `ResumePoint`, `LineKey` and the identity lookups on
  `VaultSession`, so they are testable without UIKit.
- `Thock/AppModel.swift`: `AppSheet.journal(day:entry:)`, `.section` keyed by `HeadingRef`; save
  and restore the resume point; `becameActive` resumes instead of `goToToday` unless home (§5.2);
  `isCovered` for the privacy cover.
- `Thock/Cards.swift`: per-entry taps on `JournalCard`; tappable preamble and planner notes;
  `PlannerCard` draws tasks, *Add a line*, notes, *Add a note…* (§4.6).
- `Thock/JournalScreen.swift`, `SectionScreen.swift`: take day/heading identities; commit and stay
  on background; report draft, cursor and scroll to the resume point.
- `Thock/CaptureSheet.swift`, `Shared/ClipSheet.swift`: draft in and out of the resume point.
- `Thock/RichTextEditor.swift`: expose and restore the selection through `EditorHandle`.
- `Thock/RootView.swift`, `ThockApp.swift`: the privacy cover on `.inactive`.
- `Thock/Sheets.swift`: the inbox header's inset (§4.7); the in-app clip sheet's draft.

No inherited Zed crate is touched. The desk changes only in the shared sync-core fixture (§7).

## 7. Tests

- **ThockKit unit tests** (`VaultTests`, `EditorTests`):
  - A user `# Heading` after the title is `.prose` and editable; `# Daily Closure` stays `.agent`;
    a Routine-declared agent heading is honoured.
  - Journal spans: a stamped paragraph with bullets is one entry; a checklist line or table ends
    it; an unstamped leading paragraph is its own entry.
  - Replacing a multi-block entry round-trips through `Apply` and leaves neighbours byte-identical;
    a desk edit in between produces a conflict.
  - Preamble replace and append land in the title's own lines; a note without a title has no
    editable preamble.
  - `ResumePoint` encodes and decodes; identity resolution finds a block after lines above it moved, and
    reports "gone" after the desk rewrote it.
  - The home rule: today with no sheet saves no resume point; yesterday-that-was-today is home.
- **Sync-core fixtures**, passed by both `thock_sync_core` and `SyncCoreFixtureTests`:
  - a `beforeChildren` append under a level-1 title with existing `##` sections;
  - an `append` anchored after a planner task with a continuation line and a paragraph below it:
    the task lands between them; a missing anchor falls back to the end of the own lines.
- **Planner layout:** a planner with tasks and a captured paragraph renders tasks, *Add a line*,
  the paragraph, *Add a note…*; adding a task writes it above the paragraph in the file.
- **iPhone smoke** (on palloma): open a journal entry, type, press Home, relaunch via
  `XCUIApplication.activate()`, and assert the same entry is live with the typed text; repeat
  after `terminate()` to cover termination.

## 8. Build order

1. Model: agent classification and journal spans, with tests, plus the inbox's Done button
   (§4.7). Ships on its own; fixes the amber section immediately.
2. The planner's two add rows and the anchored `append` (§4.6).
3. Tap-to-edit on the journal card, past-day journals, preamble and planner notes, plus the
   sync-core fixture.
4. The resume point: save, privacy cover, restore on return, then termination, then capture drafts.

## 9. Decisions

- **D1. Restore has no expiry.** The ask is "always keep the state", so a resume point is restored
  however long the app was away. Today's canvas is the only home. Revisit if a stale restore after
  days away feels wrong in use.
- **D2. Journal and section editors write through on background; captures keep a local draft.**
  Recommended because it keeps the desk current and a capture's destination is a final choice.
  The alternative (draft everything, write on *Done*) leaves the desk blind to journal text until
  the phone comes back.
- **D3. A checklist row's tap still ticks.** Editing a line stays on long-press, because ticking
  is the most frequent move on the planner. A tap-on-text-to-edit split would make the checkbox a
  small target on a row people hit in passing.
