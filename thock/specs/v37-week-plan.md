# Thock V37 — The week, from the date: a calendar and the week canvas on the phone

**Status:** Design accepted (2026-10-05); implementation not started
**Owner:** Diego · **Date:** 2026-10-05
**Design reference:** the interactive design document with the mockups and the option cards this
spec resolves: <https://claude.ai/artifact/JCHoKASWt7gcYe49YGkPHn>. Layout questions go there
first; this spec records the decisions and the vault contract.
**Companion docs:** `v33-iphone-companion.md` (the Today canvas, the editor, the write contract this
extends), `v34-vault-sync.md` (the template seed the desk hands the phone when a note is created),
`v4-day-planner-panel.md` (the checklist line grammar `## Goals` shares), `v6-backlog.md` (Move to
Soon), `v28-agent-memory.md` (the `## Things Thock could forget` list Reflect appends to the week)

---

## 1. Summary

V33 opens the phone on today and swipes to the days around it. That covers most of life, but not
*what did I write last Tuesday* or *what did I say this week was for*. V37 adds one door, the date
at the top of the screen, and the two places it leads: **any day**, and **the week's own note**.

Tapping the header opens a calendar sheet. A day opens that day's canvas, which already exists.
A week number opens the **week canvas**: the weekly note drawn as cards by the same rules as today,
with the same small edits. Goals tick like planner lines; a paragraph in a prose section can be
changed or added in place. Headings, rules and anything the agent wrote are drawn, never edited.

The week also settles a question V33 left half-open: a paragraph in the week's `## Notes` can be
edited in place, so a paragraph in today's `## Personal` can be too. One rule, learnt once, on both
canvases (§8).

Looking at a day or a week writes nothing. The weekly note is created from its template only on the
first edit, exactly as today's note is. The phone still never files, never rewrites a note, never
decides.

## 2. Goals & success criteria

- From any day to any other day in two taps: the header, then a date. From any day to its week in
  two taps: the header, then the week number.
- Opening the calendar, a day or a week changes zero bytes in the vault.
- Every write the week canvas makes is one of the V33 §14 kinds: an append, or a replacement of
  one line or one paragraph the user tapped on purpose, guarded by the section's hash.
- A weekly note edited on the phone opens on the desk looking like one written there. Nothing
  the phone does can add, rename, reorder or remove a section.
- The calendar's week numbers and the day strip agree with the vault's week naming on every
  ISO boundary, including 53-week years.
- No new desk-side code. The template seed and the configured note paths the phone already uses
  cover the weekly note.

## 3. Non-goals

- **A week widget, a Lock Screen control for the week, Siri for the week.** Entry points stay as
  V33 §12 lists them.
- **The weekly dashboard** (`weekly/site/`). It is a web page the desk builds, not a note.
- **Running Week Review from the phone.** The phone shows its result; the ritual runs at the desk.
- **Events on the calendar.** The grid shows which days have notes, not what is on the calendar.
  Calendar lines appear inside a day, as they do now.
- **Edit one section with a before/after (V33 option E2).** Paragraph-in-place covers the cases
  that matter and is already built for the journal; E2 may follow later and nothing here blocks it.
- **Creating sections, editing templates, editing the agent's text.**

## 4. The calendar

Decided: **K1**, a half-height sheet over the canvas, and **W1**, the week number is the week's
button.

### 4.1 Entry

The header of every canvas (the eyebrow and the date on a day, the eyebrow and *Week NN* on a
week) is a button, drawn with a small chevron after the title and the same press highlight a tapped
row gets. It opens the calendar sheet over the page; the page stays visible and dimmed behind it.
Swipe down or tap outside dismisses the sheet without moving. While the phone is locked (V33 §4.6)
the header does nothing, because the calendar reads the vault.

### 4.2 The grid

- One month at a time, **Monday first**, because the vault's weeks are ISO weeks. Month chevrons
  page the grid; the month and year sit in Petrona at the top.
- A **gutter on the left carries the ISO week number** of each row. The current week's number is
  amber; a week whose note exists is ink; one whose note is missing is dim.
- **A dot under a day means its note exists.** Computed from the phone's copy of the vault, no
  network. A day without a dot still opens and shows *No note for this day*, as the canvas
  already does.
- **Today is amber**, the only amber cell. When the sheet was opened from a day other than today,
  that day gets a thin amber ring; when it was opened from a week canvas, that week's row is
  highlighted and its number carries the ring.
- **Footer:** *Today* and *This week*, the same two jumps the desk's rail puts first. Each closes
  the sheet and lands there; if already there, it only closes the sheet.
- **Targets:** tapping a day cell opens that day's canvas; tapping a gutter cell opens that week's
  canvas. The whole gutter cell is the target, not the digits.

### 4.3 Reach

The day pager today holds thirty days back and seven forward. Picking a date outside that range
re-centres the pager on the chosen day so swiping continues from there. The same holds for weeks.
A day older than the phone's copy of the vault reads *Not on this phone yet*, not *No note for this
day* (see §11).

## 5. The week canvas

Decided: **N1**, the week canvas takes the day canvas's slot; the header switches between them.

Structure comes from the note (V33 §5). The shipped weekly template has `## Goals`, `## Notes` and
`## Week review`; Week Review appends `# AI Week Review` after them and Reflect may append
`## Things Thock could forget` below that. Each section becomes a card; a customised template
changes the screen.

```
┌──────────────────────────────┐
│ THIS WEEK · OCT 5 – 11       │   eyebrow: This week / Last week / Next week / 3 weeks ago
│ Week 41 ⌄                    │   Petrona, tappable, opens the calendar on this week
│ M  T  W  T  F  S  S          │   day strip: tap jumps to that day's canvas
│ 5  6  7  8  9  10 11         │   today amber, a day without a note dim
│ ─ GOALS · 0 of 3 ──────────  │   checklist lines, tick, long-press for the nudge menu
│ ☐ Finish the budget …        │
│ + Add a goal                 │
│ ─ NOTES ───────────────────  │   paragraphs, tap to edit, "Add a note…" below the last
│ ─ WEEK REVIEW ─────────────  │   the user's prose, same rules
│ ─ AI WEEK REVIEW · SUNDAY ── │   the agent's amber block, read-only
│ [ Write something…      ]    │   compose dock, unchanged
└──────────────────────────────┘
```

Same as a day:

- **Swipe sideways** moves a week.
- **The header** opens the calendar with this week's row highlighted.
- **The compose dock** stays and a capture still defaults to Today; *Add a goal* is the way into
  the Goals section.
- **Agent headings** (a level-1 heading after the user's sections, V33 §7.2) render in the agent's
  amber voice with the notes it read underneath, read-only. Everything after the first agent
  heading is the agent's, including Reflect's checklist.

Different from a day:

- **The day strip** replaces the quick actions. Days without a note are dim; today is amber.
- **No Inbox row.** Receipts belong to today, where captures are made.
- **No time chips.** Goals are checklist lines without times; the nudge menu has no *Set a time*.
- **Cold launch is still today** (V33 H1). The app never restores to a week.

## 6. Editing the week

Decided: **L1**, paragraph in place, no confirmation screen. Sections are the user's; the
structure is not.

| On the phone | In the note |
| --- | --- |
| Tick a goal | `- [ ]` ↔ `- [x]` on that line |
| Add a goal | `- [ ] <text>` appended at the end of `## Goals` |
| Edit a goal | that line's text replaced, checkbox state kept |
| Move a goal to Soon | line removed; `- [ ] <text>` appended under **Soon** in `backlog.md` (V33 §17 #14) |
| Remove a goal | line deleted, with a few seconds of undo |
| Add a paragraph to Notes or Week review | paragraph appended at the end of that section, before the next heading |
| Edit a paragraph | that paragraph's lines replaced through the journal's scoped rewrite (V33 §9); everything around it byte-identical |
| The template's italic prompt | kept in the file; hidden once the section has the user's words, as the journal does |
| Headings, rules, the opening italic line | drawn, never editable; no adding, renaming or reordering sections |
| `# AI Week Review` and anything after it | read-only |
| Tables, code, comments, anything outside the subset | grey block, preserved exactly (V33 §8) |

A paragraph replacement is guarded by the section's hash, so a desk edit between syncs becomes a
visible conflict (V34) rather than a silent overwrite. The first edit on a week with no note creates
it from the template, through the same seed the desk hands the phone for a daily note, and then
applies the edit.

## 7. Goals and the backlog

*Move to Soon* stays in the goal menu. A goal is not a task, but an abandoned one usually becomes
one, and the desk's own wrap flow sends unfinished work to Soon. The line is appended under the
configured Soon heading like any planner line; nothing is written to the triage log.

## 8. Prose parity on daily notes (decided 2026-10-05)

Every prose section the user wrote gets the same paragraph editing, on both canvases. On a daily
note that means `## Personal` and any other read-only prose card from V33 §5: tapping a paragraph
makes it live, tapping below the last adds one, and saving replaces or appends only those lines.

The exceptions stay exactly as V33 drew them: nothing under an agent heading (`# Daily Closure`,
`# Asked on the go`, `# AI Week Review`), nothing in a grey block, no heading, no rule. `## Journal`
keeps its timestamp behaviour; `## Day planner` and `## Goals` keep their checklist moves.

## 9. The write contract (V33 §14, extended)

| Where | Added by V37 |
| --- | --- |
| `weekly/<week>.md` | create from template on first edit; tick, append, replace or remove one `## Goals` line; append or replace one paragraph in a user prose section |
| `daily/<any day>.md` | reachable from the calendar for any date; append or replace one paragraph in a user prose section, in addition to the V33 planner and journal edits |
| `backlog.md` | unchanged; a goal moved to Soon uses the existing append |
| everything else | unchanged. The phone still never touches `memory/` (beyond V35's inbox line), `routines/`, templates, `weekly/site/` or the triage log |

Nothing new on the desk. The desk already expands the weekly template when the phone asks to
create a note (`vault_sync::template_seed`), the weekly path comes from the vault's configured
folder and `GGGG-[W]WW` pattern the phone already reads, and the calendar's dots come from the
phone's local store.

## 10. Tests

- **Week naming** on ISO boundaries: 28 Dec 2026 → `2026-W53`, 1 Jan 2027 → `2026-W53`,
  4 Jan 2027 → `2027-W01`. The gutter, the day strip and `weeklyPath` agree.
- **Grid shape:** every month renders Monday-first with each ISO week on one row, and the amber
  week number is the week the vault opens for today.
- **Dots:** a day with a note has a dot, one without has none, a week number is dim when its note
  is missing, and opening the calendar changes no file.
- **Re-centre:** a date outside the pager's range lands on that day and swiping continues from it.
- **Goals moves:** each row of §6 against a weekly note with a prompt line, a done goal, prose in
  Notes and an `# AI Week Review` below: exactly one line changed (two files for Move to Soon),
  everything else byte-identical.
- **Paragraph edits:** replace one paragraph in Notes, append one to Week review, and the same on
  a daily `## Personal`: only those lines differ. A paragraph under an agent heading is not
  offered for editing.
- **First edit creates the note:** a tick on a week with no note creates it from the template and
  then applies the tick; viewing it first does not.
- **Locked:** the header does nothing while the phone is locked.

## 11. Decision log (2026-10-05)

From the design document's option cards, with Diego's answers:

1. **Calendar shape: K1.** A half-height sheet over the canvas.
2. **Choosing a week: W1.** Tap the week number in the gutter.
3. **Where the week canvas lives: N1.** The same slot as the day; the header switches between them.
4. **Prose editing depth: L1.** Paragraph in place, no confirmation screen.
5. **Prose parity:** daily notes get the same paragraph editing (§8).

Still open:

- **Future weeks.** Adding a goal on a Friday creates next week's note a few days early. Accepted
  for now, since it is the same moment V33 accepted for tomorrow's note; revisit if it surprises.
- **Days older than the phone's copy.** Whether the phone holds every day ever written is a V34
  question. Until it is answered, a day the phone does not have reads *Not on this phone yet*.
