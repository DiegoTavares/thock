# Thock V45 - Reminders: a date on any line, rung on the desk and the phone

**Status:** Built (2026-10-22) on `v45-reminders`: everything in §15 steps 1–7 except the silent push wiring,
which is on `v45-push`. Desk ringing on macOS waits for V35.
**Owner:** Diego · **Date:** 2026-10-21
**Build plan:** `v45-reminders-plan.md` (modules, agent lanes and the order of work).
**Design reference:** the feature flow with the token states, the picker, the index diagram, the day
mockup, both devices' notifications, the Reminders tab and the phone screens:
<https://claude.ai/artifact/XkxotTy8NuxSNtx9Xk7e6M>.
**Companion docs:** `v4-day-planner-panel.md` (the planner grammar a reminder sits beside),
`v8-calendar-sync.md` (the reconciler `### Reminders` copies), `v10-markdown-conceal.md` and
`v24-inline-markdown-conceal.md` (the machinery the chip rides), `v6-backlog.md` and
`v17-backlog-categories.md` (backlog lines that carry reminders), `v26-planner-heading-resolution.md`
(heading matching and holds), `v33-iphone-companion.md`, `v34-vault-sync.md` and
`v34-vault-sync-api.md` (the phone and the write contract), `v38-phone-backlog.md` (the screen the
Reminders screen follows), `v39-vault-images.md` (why `![` is taken), `v43-almanac.md` (the Desk
tabs and the Next card), `v44-passing-notes.md` (the `ask` tool the weekly sweep uses)

---

## 1. Summary

A thought with a date attached has nowhere to go in Thock today. A planner line can hold a time,
but only for today and only in today's note; a backlog task has no date at all; and nothing ever
rings. V45 adds reminders:

- **A token on any line.** `(@2026-12-02 10:20)` at the end of a task or a sentence, in any note,
  the backlog included. Inside a daily note, `(@10:20)` means that day. The format is the Obsidian
  Reminder plugin's, so a vault opened in Obsidian keeps working reminders.
- **A picker for people who type.** `(@` opens a suggestion list that follows what you type
  (`tom 10`, `fri 9`, `dec 2`) without taking focus, with a keyboard-driven calendar one row down.
  Off the cursor's line the token draws as a chip.
- **One index, no crawling.** A reminder lives on the line where it was written. `reminders.md`
  holds the full line for reminders with no other home, and one link, carried by a hidden id, for
  each that lives elsewhere. Every reader opens the index and only the files it links to.
- **The day knows.** A daily note gets `### Reminders` under its planner, kept current the way
  `### Calendar` is, so the planner, the Next card, the phone's planner card and the widget show
  the day's reminders with no new UI.
- **Both devices ring.** The desk through a system notification while Thock is open, the phone
  through local notifications scheduled from its own copy of the vault, each with *Done*, *In
  1 hour* and *Tomorrow 09:00*.
- **A Reminders tab and a phone screen.** Overdue, Today and Upcoming, every action on a key at the
  desk and a gesture on the phone, and a fourth capture chip, *Reminder*.
- **Never left behind.** An open reminder leaves Today or Overdue only through a decision: close,
  reschedule or drop. Overdue reminders follow you into each new day, and Week Review sweeps
  whatever is still open.

## 2. Goals & success criteria

- Typing `(@tom 10` in `projects/house.md` on Wednesday 21 October and pressing `tab` writes
  `(@2026-10-22 10:00)`; moving the cursor away draws a chip reading *Tomorrow 10:00*; within two
  idle seconds the line ends with a `<!--rem:…-->` id and `reminders.md` gains one link line.
- In Thursday's own daily note the same keystrokes write `(@10:00)`.
- At 10:00 on Thursday the desk, if open, shows one system notification, and the phone, if it has
  synced since the reminder was made, shows one lock-screen alert. Each answer (*Done*, *In 1 hour*,
  *Tomorrow 09:00*) changes exactly one line of the vault.
- Thursday's daily note, whenever it is created and by whichever device, gains
  `- [ ] 10:00 Ask the bank for the mortgage offer · [[projects/house]]` under `### Reminders`; the
  planner draws a point at 10:00 and the Next card counts down to it.
- A reminder captured on the phone with the *Reminder* chip rings on the phone with no network, and
  on the desk once the desk has applied the capture.
- A reminder still open when its day ends is listed in every following day's `### Reminders`, in
  Overdue on the tab and the phone, and in the next Week Review, until it is closed, rescheduled or
  dropped. Nothing expires.
- A line copied anywhere (a wrap carrying a task to the backlog, a `# Daily Closure` quote, a paste)
  stays the same reminder: one ring, one row.
- Two windows on one vault ring once. A restart never rings a reminder that already rang.
- Every action in the Reminders tab has a key, vim motions included (CLAUDE.md, Panels).
- No reader ever walks the vault. The one full scan is the index's first build.
- No upstream crate changes. The completion API, the fold placeholders and gpui's system
  notifications are already public; everything else is the Thock crate, the sync core and the phone.

## 3. Non-goals

- **Repeating reminders.** The grammar leaves room (`(@every mon 09:00)` is not a token today, so
  it stays text), and the feature is the obvious next slice.
- **Location reminders, shared or assigned reminders**, and two-way sync with Apple Reminders,
  Google Tasks or a calendar. Thock reads Google read-only and stays that way.
- **A visible push from the server at the reminder's time.** The server cannot read notes, so it
  cannot know when anything is due. The phone schedules its own alerts; push only wakes it (§9.4).
- **Inbox `due:` dates becoming reminders** when an item is filed. Google Tasks already sends them
  and filing drops them today; a later change.
- **Widget timeline entries, Live Activities and an app badge.**
- **Reading natural language in other languages.** The reading is English words plus numbers; the
  file format is language-free.
- **Reminders outside a vault**, as conceal is.

## 4. The token and the id

### 4.1 Grammar

```
token   = "(@" when ")"
when    = date | date " " time | time
date    = YYYY "-" MM "-" DD          a real calendar date
time    = H ":" MM | HH ":" MM        00:00 to 23:59; Thock always writes HH:MM
closed  = "~~" token "~~"             a reminder closed in prose
id      = "<!--rem:" 12 hex "-->"     lowercase hex
```

| Written | Means | Rings |
| --- | --- | --- |
| `(@14:30)` | That day at 14:30. Only in a daily note, whose file name gives the day. | 14:30 |
| `(@2026-12-02)` | That day, no time. | at `date_only_at`, 09:00 by default |
| `(@2026-12-02 14:30)` | That day at 14:30, anywhere in the vault. | 14:30 |

Times are local wall-clock times. A reminder set for 10:00 rings at 10:00 wherever the device is.
A time that does not exist on its day (clocks going forward) rings at the first minute after the
gap; a time that happens twice rings the first time.

### 4.2 Where a token counts

- **Files:** Markdown files in the vault, except under `.thock/`, `templates/` and `routines/`, and
  except the `[daily]` and `[weekly]` template files wherever they live. Only the template file is
  left out, so `template = "daily/_template.md"` keeps every daily note in scope. A template's token
  is not a reminder; the note made from it is.
- **Not code or comments.** A token inside inline code, a fenced block, front matter or an HTML
  comment is text, and so is one after a backslash (`\(@…)`). `<!-->` and `<!--->` are whole, empty
  comments, as in CommonMark. `AGENTS.md` and the Routine docs
  write their examples in code.
- **One reminder per line:** the last token on the line. Earlier tokens on the same line are text.
- **Anywhere on the line** on read; Thock writes it at the end, before any trailing comment. A
  planner time stays first: `- [ ] 16:00 - 17:00 Dentist (@15:30)` is a 16:00 block that rings at
  15:30. A token in front of a planner time takes the line off the planner (V4 §5), which the
  picker never does.
- **Time-only outside a daily note** has no day. It is drawn dashed (*17:00 · which day?*), never
  indexed, never rung. A daily note is a file whose path matches the vault's `[daily] dir` and
  `filename`; its day is recovered by reading the stem back through the moment-style format
  (`YYYY-MM-DD` by default; `YYYYMMDD_MMM-ddd` works too). A format with no year, month or day
  token never resolves.
- **Not on calendar lines.** A line carrying `<!--gcal:` belongs to calendar sync, which would
  freeze or duplicate it (V8). A token there is text, and the picker does not open on it.

### 4.3 The id

`<!--rem:3f9a1c0d4e21-->`, twelve random lowercase hex digits, like calendar's `<!--gcal:…-->`. It
ties a line to its index entry and to every copy of the line.

- **Who writes it.** The desk adds it to any line with a token and no id (§6.1); the phone writes it
  with the token when it creates a reminder (§10). Nothing else ever does; an agent writes the token
  and lets the desk add the id. Before writing one, the writer checks the index for a collision and
  draws again.
- **Where it sits.** Last on the line, after the token and after any other trailing comment the
  line already had (a `<!--gmail:…-->` backlog capture keeps its marker before it). A line that opens
  a comment and never closes it (`… <!-- details` continuing on the next lines) gets the id, and the
  token, before that `<!--`: written after it they would sit inside the comment, unread, and their
  `-->` would close it early.
- **Never drawn.** Conceal hides it like every comment. Panel and phone labels strip every trailing
  comment, not just one (today the backlog row and the planner label strip one).
- **Sync identity is unchanged.** `line_hash` already drops trailing comments (V34 API §7.4), so
  adding an id never changes a line's identity and never trips a phone write. The one exception is
  the line that leaves a comment open, whose id goes inside the text `line_hash` reads. The token itself is
  part of the identity: a reminder rescheduled on both devices before they sync keeps both lines
  under V34's keep-both rule, and because both carry the id it is still one reminder (§5.3).

### 4.4 Line shapes

```markdown
- [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->            a task
The boiler ticks when it starts. Ask about it before the cold comes (@2026-11-02) <!--rem:7c11e0b4d2a9-->
- [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->                   a planner line
- [x] Pay the deposit (@2026-10-19 09:00) <!--rem:51aa02c9d811-->            closed task
Got the quote back, it's fine. ~~(@2026-10-18 10:00)~~ <!--rem:0d93be7a1c44-->  closed in prose
- [ ] Send the leak photos to the landlord <!--rem:9e4a21f0b388-->           dropped: id, no token
> - [ ] Ring the insurer (@2026-10-23) <!--rem:4a7c2e19b0d5-->                 a task in a quote
```

A task inside a blockquote, at any depth and with or without the space after each `>`
(`> > - [ ]`, `>>- [ ]`), is a task: closing ticks its box and leaves the quote markers byte for byte,
and its label drops them. Quoted prose closes by striking the token, like any prose. This is the
reminder grammar only; the planner and the backlog still read a quoted line as text.

## 5. Where reminders live

### 5.1 The home line and the index

A reminder lives on the line where it was written: its **home**. `reminders.md` at the vault root is
the **index**. It holds the full line for reminders with no other home (phone captures, the tab's
`o`) and a link for each reminder whose home is another note:

```markdown
# Reminders

<!-- Thock keeps this index. A task here is a reminder; a link points at one that lives in another note. -->

## Open
- [[backlog]] <!--rem:2f6b08e19a5c-->
- [ ] Send the October invoice (@2026-10-21) <!--rem:a90c1377d02e-->
- [[projects/house]] <!--rem:7d3e55a1c0f2-->
- [ ] Call the bank (@2026-10-22 10:00) <!--rem:3f9a1c0d4e21-->
- [[daily/2026-10-20]] <!--rem:c41f77a8e590-->

## Done
- [x] Pay the nursery (@2026-10-19 09:00) ✅ 2026-10-19 <!--rem:6e1f42b07a93-->
- [[projects/house]] ✅ 2026-10-19 <!--rem:51aa02c9d811-->
- [[projects/house]] dropped 2026-10-25 <!--rem:9e4a21f0b388-->
- [[daily/2026-10-14]] removed 2026-10-16 <!--rem:0b5e6d1f2a37-->
```

- **A link holds no copy of the reminder's text or time**, so the two can't drift apart. A reader
  opens the index, then only the files it links to, and finds each line by its id.
- **The file is appended to and lines are moved between its sections; it is never sorted or
  rewritten.** Views sort; the file keeps arrival order.
- **The desk creates it** with the skeleton above the first time it needs it. A phone capture that
  finds no file gets `## Open` and the line (V34 `append` on a missing file); nothing depends on the
  title or the comment being there.
- **The path and headings are configurable** (§12) and match loosely, as the planner heading does
  (V26).
- **Thock draws each link** as the line it points to, with its text, chip and home (design reference
  §3). In any other editor the links still open the right notes.

### 5.2 State comes from the home line

A reminder is **open** while its home line is an unticked task, or prose with an unstruck token.
It is **closed** when the line is ticked or the token struck, and **dropped** when the line keeps
its id but has lost its token. Its position in `## Open` or `## Done` is bookkeeping the desk keeps
in step (§6.1); no reader trusts it over the home line.

### 5.3 Copies

Any other line with the same id is a **copy**: a mirror line in a day's `### Reminders` (§7), a task
a wrap carried to the backlog, a pasted line. A copy is never a second reminder.

- Ticking or unticking a copy is applied to the home line by the desk, and the most recent change
  wins. That is how ticking a reminder in today's note closes it in `projects/house.md`.
- If the home line is gone and a copy with a token remains, that copy becomes the home (§6.1 step 4).
  A cut in one note and a paste in another is a move, not a removal.

### 5.4 Who writes what

| Writer | Writes |
| --- | --- |
| You, in any editor | tokens, anywhere |
| The desk's index service | ids; link lines; moves between `## Open` and `## Done`; re-homed links; Done stamps |
| The desk's day reconciler | `### Reminders` in daily notes, insert and replace only |
| The Reminders tab and desk notifications | the home line: tick, strike, a rewritten token, a removed token |
| The phone | loose lines in `reminders.md` with their ids; a token, an id and a link for *Remind me…*; tick, strike, rewrite or drop on a home line; ticks of mirror lines |
| Rituals and agents | tokens; tick, strike, rewrite or drop on a home line found by its id |

Everything that is not the desk edits home lines and appends; the desk owns the index's
bookkeeping. That keeps every phone gesture inside the write kinds it already has (§11).

## 6. The desk: index and notifier

### 6.1 The index service

`ReminderService` is one entity per vault, not per window: a global map from the canonical vault
root to a weak handle, filled by the first workspace on that vault. Two windows share one index and
one notifier. (Calendar's services are per workspace today; reminders are the first that must not
be.)

It runs on worktree file events for Markdown files in scope, on its own writes, at startup, and on
`thock::RebuildReminders`. Changes are batched over 500 ms. A buffer being typed in is processed
only after two idle seconds and never on the cursor's line, as calendar sync waits (V8). For each
batch:

1. **Parse** the changed files into appearances: id, token, state, whether the line could be a home.
2. **Assign ids** to lines with a token and no id, including loose lines in `reminders.md`.
3. **Index new reminders.** An id not in the index whose line is outside `reminders.md` gets a link
   line appended under `## Open`, or under `## Done` if it is already closed.
4. **Follow moves.** An id that left its home file and now has a token elsewhere gets its link
   retargeted with one line replace. An id gone from everywhere waits ten seconds, so that a cut and
   paste can land, then moves to `## Done` as `removed <date>`.
5. **Follow state.** A home line that was closed moves its entry to `## Done` with `✅ <date>`; one
   that reopened moves back without the stamp; one that lost its token moves to Done as
   `dropped <date>`.
6. **Apply copies.** A tick or untick on a copy is written to the home line (§5.3), then step 5.
7. **Repair the index.** A link deleted from `reminders.md` while its home still has an open token
   is appended again: a reminder is dropped by removing its token, not its index entry.

Writes go through the open buffer as one undoable transaction when the file is open, and through
`Fs` otherwise, never as a whole-file rewrite (CLAUDE.md, Vault I/O). A failed write surfaces in the
sync status popover (V32) as a Reminders row with Retry; it never fails silently. A home-line edit
that failed (a tick from the tab or a notification) stays on that row until Retry applies it.

**Startup.** The service keeps `.thock/cache/reminders/scan.json`, each in-scope path with its
mtime and ids. At startup it re-parses only the files whose mtime changed while Thock was closed;
the worktree's first scan reports every file, and those reports are checked against the cache too.
Ids read from a buffer with unsaved changes are cached without an mtime, so they are read again. On
the first run, or after `thock::RebuildReminders`, it scans the vault once and builds the index.
`.thock/cache/` is outside sync and rebuildable.

### 6.2 Reading the index

Every desk reader (the tab, the notifier, the day reconciler) asks the service. The service reads
`reminders.md`, opens each linked file through its buffer or `Fs`, finds the line by id (a line with
a token first), and caches each file by mtime. A link whose line is missing reads as a reminder with
no text (*line not found*) until step 4 settles it.

### 6.3 The notifier

- **One timer** to the next due instant among open reminders (a date-only reminder is due at
  `date_only_at` on its day), plus a 60-second check that catches sleep and clock changes. It is the
  desk's first wall-clock scheduler; every existing loop is a relative timer.
- **A ring** is `cx.show_system_notification` with tag `rem:<id>:<due>`, the reminder's text
  (trimmed to 80 characters) as the title, `14:30 · projects/house` or `today · reminders` as the
  body, and three actions: *Done*, *In 1 hour* and *Tomorrow 09:00*.
- **Answers** come through `on_system_notification_response` and edit the home line: *Done* ticks
  it or strikes the token; *In 1 hour* rewrites the token to now plus `snooze_minutes` on the wall
  clock (so the desk and the phone agree across a clock change), rounded up to the minute; *Tomorrow 09:00* rewrites it to tomorrow at `tomorrow_at`. A rewrite writes the full
  date unless the home is the daily note of the new day. An answer whose tag's due is no longer
  the reminder's, or to a reminder no longer open (rescheduled, closed or answered on the other
  device since it rang), edits nothing and opens the Reminders tab instead. A failed edit stays on
  the sync status row with Retry.
- **Never twice.** `.thock/cache/reminders/fired.json` records `id → due` once rung. A rescheduled
  reminder has a new due and can ring again; a restart does not repeat a ring.
- **While closed.** At startup, reminders that came due while Thock was closed and never rang on the
  desk produce one summary (*2 reminders came due while Thock was closed*, with *Open Reminders*),
  not a burst. Its tag is `rem:summary:<vault root>`, one per vault. *Open Reminders* brings Thock
  to the front and shows the tab in the most recently used window on that vault, not in every one. They wait in Today or Overdue like any other.
- **Off switch.** `[ring] desk = false` stops desk notifications; the tab and the day section keep
  working.
- **Platforms.** Linux rings through notify-rust over D-Bus today; whether the action buttons show
  is up to the notification daemon, and the tab offers the same actions. On macOS notifications
  follow the app's bundle identity, so desk ringing ships with or after V35. Thock calls gpui's
  `set_app_identity` at startup on both; it never has until now.

## 7. The day: `### Reminders`

### 7.1 What a day's section holds

A daily note's `### Reminders` (the planner heading's level plus one) lists, for that day:

- every open reminder due that day whose home is another file, and
- in **today's** note only, every reminder still open from an earlier day, marked with its due.

Reminders whose home is this note are not repeated; the Dentist line already rings. Past days are
left as they were. A future day's note, created ahead, gets its own reminders and no overdue ones.

```markdown
## Day planner
- [x] 09:30 - 11:00 Deep work on the budget
- [ ] 16:00 - 17:00 Dentist (@15:30) <!--rem:5b71c08e3f6d-->

### Reminders
- [ ] 14:30 Call the plumber · [[projects/house]] <!--rem:7d3e55a1c0f2-->
- [ ] Send the October invoice · [[reminders]] <!--rem:a90c1377d02e-->
- [ ] Renew the car insurance · 2026-10-20 09:00 · [[backlog]] <!--rem:2f6b08e19a5c-->

### Calendar
- [ ] 12:00 - 12:30 Team sync <!--gcal:5d0c2a91b7e4-->
```

- **Mirror lines** are planner lines: a timed reminder carries its time as the planner time, so the
  V4 parser, the Next card, the phone's planner card and the Today widget read it with no change.
  A date-only reminder is an untimed line. A carried one shows its original due before the link,
  which the desk and the phone draw as *from Tue 20 Oct* in the warning colour.
- **The text** is the home line's text without its checkbox, planner time, token and comments, cut
  at 80 characters on a word boundary. It is a copy for the planner's sake; the id ties it to the
  original.
- **No token on a mirror line**, so it is never a reminder of its own, and the Obsidian plugin does
  not ring it twice. The text escapes every `(@` as `\(@`, so an earlier token the home line still
  shows as text never comes alive on the mirror. `mirror_text` in the sync core writes it, with a
  vector for each rule.

### 7.2 The reconciler

The day reconciler follows calendar sync's rules (V8 §§5–7), with the same heading resolution and
holds (V26):

- **When:** a daily note is created (by `ensure_note_at`, which now tells the service, or by a file
  event when the phone made it), the index changes, the day changes at midnight, and at startup. It
  never creates a note.
- **Insert and replace only, never delete.** A new line goes before the first mirror line that
  sorts later (timed by time, then untimed, then carried) or after the section's last line. The
  section is created as the last child of the planner section the first time it is needed.
- **Replace** splices the time and the text when the home line changes. A mirror whose text the
  person edited is frozen and logged to `.thock/cache/reminders/log.jsonl`, as calendar freezes a
  renamed event.
- **Moved to another day:** the line becomes `- [ ] ~~Call the plumber~~ → 2026-10-23 ·
  [[projects/house]] <!--rem:…-->` and stays. Moved back, it is restored.
- **Closed:** the mirror is ticked. **Dropped or removed:** struck, with `(dropped)` or `(removed)`.
- **No planner heading:** the reconciler holds and the Day Planner's status row offers *Add heading*,
  exactly as calendar sync does.

### 7.3 On the planner and the Next card

- **A reminder is a point, not a block.** A mirror line, and the token time on a planner line (the
  Dentist's 15:30), draw as a marker with a bell on the Day Planner grid, not as a 30-minute block.
  `day_plan.rs` items gain an optional reminder time; mirror lines are recognised by their id in the
  Reminders subsection.
- **The Next card** counts down to points as it does to blocks (*Next · in 25 min · 14:30 · Call the
  plumber*). A point is never *Now*.

## 8. The desk editor

### 8.1 The suggestion list

The completion provider in `wikilink_completion.rs`, which already serves `[[` and `![`, gains a
reminder branch:

- **Trigger:** an open `(@` before the cursor on the line, with no `)` between them, outside code
  and outside a calendar line. Markdown autocloses `(`, so typing `(@` leaves `(@)` with the cursor
  inside. The query is the text from `(@` to the cursor.
- **Items** are the readings of the query (§8.2), the soonest first, then *Pick a date…*. An empty
  query offers *Later today 17:00* (before 16:00), *Tomorrow 09:00* and *Monday 09:00*. A query with
  no reading returns no items, so the list closes and `enter` makes a new line as usual.
- **The response is incomplete** and every item's filter text is the query itself, so the menu asks
  again on each keystroke and its fuzzy filter keeps every reading. Without that, `dec 2` would hide
  *Wed 2 Dec*.
- **Accepting** (`tab` or `enter`) replaces `(@query` and the autoclosed `)` with the canonical
  token: the full date, or `(@HH:MM)` when the line is in the daily note of that day. `esc` closes
  the list and leaves the typed text as it is.
- The list never takes focus. Vim insert mode moves through it with `ctrl-n` and `ctrl-p`, as
  through any completion.

### 8.2 The reading

English words and numbers, read against the current local time:

| Typed | Reads as (on Wednesday 21 October at 14:05) |
| --- | --- |
| `today`, `tod` | today, no time |
| `tomorrow`, `tom`, `tmr`, `tmrw` | Thursday 22 |
| `fri`, `friday` … | the next Friday (a weekday never means today) |
| `next mon` | the Monday of next week |
| `next week` | next Monday |
| `dec 2`, `2 dec` | 2 December, this year or next if passed |
| `2026-12-02` | that date |
| `in 2h`, `in 30 min`, `in 3 days`, `in 2 weeks` | now plus that; days and weeks give a date with no time |
| `10:30`, `9am`, `9 pm` | that time |
| `10` | a bare hour: both readings, 10:00 and 22:00 |
| `morning`, `noon`, `afternoon`, `evening`, `tonight` | 09:00, 12:00, 15:00, 18:00, 20:00 |

A time with no date is the next time it comes round. A reading that is already past today offers
tomorrow instead, marked *already past* when the day was named. Unknown words are ignored and named
beside the first reading (*ignores "foo"*). The reading lives in `crates/thock/src/reminder_reading.rs` as a
pure function of the query and the clock; the phone's sheet uses presets instead (§10.2).

### 8.3 The calendar

*Pick a date…* opens a calendar popover anchored at the cursor, ported from gpui-kit's calendar
into `crates/thock/src/ui/calendar.rs` with the Apache-2.0 notice and the source path, as V41 ported
the split. It shares its day model with the masthead's two-week strip.

- Arrows, or `h` `j` `k` `l` in vim mode (`vim.json`), move the day or week, `[` and `]` change month, digits type the time,
  `backspace` clears it (no time means date-only), `enter` accepts, `esc` goes back to the list.
- Key context `ThockReminderCalendar`. Unlike the list it takes focus, because it was asked for.
- `thock::PickReminderDate` opens it at the cursor from the palette or a binding, and on a line that
  already has a token it starts on that date.

### 8.4 The chip

Conceal (V10, V24) folds a token off the cursor's line into a chip; the cursor's line shows its
source, as for every other markup.

| State | Chip |
| --- | --- |
| Upcoming | accent outline: *Mon 2 Nov*, *Tomorrow 10:00* |
| Today | accent fill: *Today 14:30* |
| Overdue | warning colour: *Tue 20 Oct 18:00 · overdue* |
| Closed | muted, struck, with a check: *Mon 19 Oct* (a ticked task, or a struck token in prose) |
| No day | dashed: *17:00 · which day?* |

Labels say *Today*, *Tomorrow* or *Yesterday*, otherwise `ddd D MMM` (with the year when it is not
this year), plus the time, through `notes::format_date`. They are recomputed at midnight.

Architecture: `SpanKind` is a `Copy` enum without a payload today, and fold placeholders are chosen
and diffed by kind (`collapsed_text_for`). `SpanKind::Reminder` carries the chip's label and state;
the fold diff keys on range, label and state, since a closed chip can read like an open one
(*Today*). The placeholder is a `FoldPlaceholder` render with
`constrain_width: false`. No upstream change.

### 8.5 Rows outside the editor

The planner's labels, the Next card, Backlog rows, Done rows and `markdown_text.rs` drop the token
and every trailing comment from the text and show the chip in the row's meta line. A backlog task
with a reminder reads *Renew passport* over *⏰ Wed 2 Dec · 09:00*.

## 9. The Reminders tab

### 9.1 Layout

The Desk's tabs become **Backlog · Today · Reminders · Done**. `DeskTab` gains `Reminders`; keys
`1`–`4` in `ThockDeskPanel && !editing`, so Done moves from `3` to `4`; `g t` and `g shift-t` cycle
as before; `ActivateDeskTab { tab: "reminders" }`.

- **Groups:** Overdue, Today, Upcoming, each sorted by due. A row is the text over a meta line with
  the due and the home (*Tue 20 Oct · 09:00 · backlog*), Overdue in the warning colour.
- **The tab's count** is the number overdue, in the warning colour, and nothing when none are.
- **Done** lists closed and dropped reminders from `## Done` beside closed backlog tasks, by date.
- Key context `ThockRemindersPanel` plus `menu`, a visible selection, and the selection kept by id
  across reloads and file events.

### 9.2 Keys and actions

| Keys | Action |
| --- | --- |
| `j` `k`, `up` `down`, `g g`, `shift-g` | `menu::SelectNext`, `SelectPrevious`, `SelectFirst`, `SelectLast` |
| `enter` | `menu::Confirm`: open the home note at the line |
| `x`, `space` | `thock::CloseReminder`: tick the line or strike the token |
| `s` | `thock::RescheduleReminder`: a small menu under the row, *Tomorrow 09:00*, *Monday 09:00*, *Pick a date…*; arrows and `enter` or `1`–`3`, `esc` closes it |
| `d` | `thock::DropReminder`: remove the token, keep the line and its words |
| `o` | `thock::NewReminder`: open `reminders.md` on a new `- [ ] ` line under `## Open` |
| `escape` | back to the editor |

The vim motions are bound under `ThockRemindersPanel` in `vim.json`, gated on vim mode, beside the
arrow keys every mode gets, and the macOS and Linux keymaps stay in step.

`thock::ToggleRemindersFocus` opens the Desk on this tab: `ctrl-alt-k` on Linux and `cmd-alt-k` on
macOS, in the Workspace context. No other context binds either chord, so it works from a terminal,
the project panel and the outline too (`ctrl-alt-r`/`cmd-alt-r`, the first choice, are the
terminal's `RerunTask` and the macOS panels' `RevealInFileManager`). Doc comments are written for a
note-taker (*Show your reminders*, *Stop reminding me, keep the line*), since they are the palette
text. `thock::RebuildReminders` (*Rebuild the reminders index from your notes*) is palette-only.

## 10. The phone

### 10.1 What the phone knows

ThockKit gains the token and id grammar, the daily-path day rule and a `ReminderIndex` that reads
`reminders.md` and the linked files from the local store, deriving state from home lines exactly as
the desk does (§5.2). The vectors in §11 keep the two in agreement.

### 10.2 Capture: the *Reminder* chip

- `CaptureDestination` gains `.reminder`, a fourth chip after Today, Inbox and Backlog.
- **Choosing it opens *Remind me*,** a sibling of `SetTimeSheet` at the same 360 points: *Later
  today 17:00* (before 16:00), *Tomorrow 09:00*, *Next week Mon 09:00*, and *Date and time* with a
  date picker and an `.hourAndMinute` picker in 24-hour time, plus *No time* for a date-only
  reminder. The chip then reads its time (*⏰ Thu 10:00*), and tapping it again reopens the sheet.
- **The chip is never remembered.** `chip(for:)` never returns `.reminder`, and saving after a
  reminder capture keeps the previous chip, so the next capture opens where it always did.
- **The hint** reads *Rings Thursday at 10:00 and waits in reminders.md.*
- **Save** appends `- [ ] <first line> (@…) <!--rem:<new id>-->` under `## Open` in
  `reminders.md`, with extra lines and pictures indented as children (V38, V39). It schedules the
  alert at once, so it rings even if the phone never reaches the server.
- **Permission** to notify is asked the first time the chip, *Remind me…* or the Reminders screen is
  used, never at launch: reminders made at the desk ring on the phone too (decision 5), so opening
  the screen asks as well. Denied, reminders still save and list, and the sheet says notifications
  are off.

### 10.3 *Remind me…* on an existing line

Backlog cards and planner rows gain *Remind me…* beside *Set a time…*. The same sheet; then one
`replace_line` that appends the token and a new id to the line, and one `append` of
`- [[<note>]] <!--rem:<id>-->` under `## Open`.

### 10.4 Today and the Reminders screen

- **Today:** a Reminders row joins Inbox and Backlog under the planner card:
  *REMINDERS · 2 overdue · 3 today ›*, or *REMINDERS · nothing due ›*. Once the desk has written
  `### Reminders`, today's reminders also show in the Day planner card like calendar lines, with the
  carried ones marked *from Tue*.
- **The Reminders screen** opens the way Backlog does (a `showingReminders` flag, `thock://reminders`):
  *‹ Today*, the title, and Overdue, Today, Upcoming, with Done folded (*Done · 2 this week ›*).
- **Gestures:** swipe right closes, swipe left opens *Remind me* to reschedule, a long press offers
  *Reschedule…*, *Open note* and *Drop the reminder*. Each is one write on the home line, with Undo
  as in V38.
- **Labels:** cards and rows drop tokens and comments from text and draw the chip. In the phone's
  editor a token stays literal text, as images do (V39 §7.5), and an edit preserves it and its id.

### 10.5 Notifications on the phone

- **Scheduling:** `UNUserNotificationCenter` with calendar triggers in local time. iOS keeps at most
  64 pending notifications per app, so the phone schedules the soonest 64 open reminders and
  refreshes the set after every store change and every return to the foreground. Identifiers are
  `rem:<id>:<due>`, so a refresh replaces only what changed.
- **Actions:** a `thock.reminder` category with *Done*, *In 1 hour* and *Tomorrow 09:00*, handled in
  the background. Each records one write in the store (tick or strike, or a token rewrite on the home
  line), hands iOS the new alerts at once and starts a sync under a background task. An answer to a
  ring whose due is no longer the reminder's, or to a reminder no longer open, writes nothing and
  opens the Reminders screen. Each refresh also takes delivered notifications off the lock screen
  when their reminder has closed or moved to another due.
- **Both devices ring.** The phone does not try to know whether the desk rang. A reminder closed on
  either device stops the other's ring once they have synced.

### 10.6 How fast the phone hears about desk changes

The phone schedules from what it has. It learns about a reminder made at the desk on its next sync:
within seconds while the app is open (the live feed, now held only in the foreground and for at most
five minutes), otherwise when the app is next opened, because silent push is designed (V34 API) but
not wired: the backend only logs pushes, and the app has no `aps-environment` entitlement and never
registers a token. Until push lands, a 15:00 reminder typed at the desk at 14:00 may not reach a
phone that stays in a pocket. Wiring push is §15 step 7, and it is the only fix: no periodic
background refresh in V45.

## 11. Sync

**No new write kinds.** Every phone gesture is an `append` or a `replace_line`, kinds the phone
already sends, and the desk applies them as today. What is new is shared reading and shared line
edits:

- **`crates/thock-sync-core/reminder.rs`**: the token and id grammar (§4.1–§4.3), the daily-path
  day rule (§4.2), mirror-line recognition (§7.1), state derivation (§5.2), and the home-line edits
  every writer makes: close (tick a task, strike the token in prose), rewrite the token (the full
  date unless the home is the new day's daily note), drop the token and keep the id, and attach a
  token and a new id to a line. The desk's tab, notifier and index service call the same edits.
  The desk links it; `ThockKit/SyncCore/Reminder.swift` ports all of it, as the rest of the core
  is ported today.
- **Vectors** in `fixtures/v1/reminders.json` (a line, its path and the `[daily]` config, and the
  reminder it yields or none), `fixtures/v1/daily_paths.json` and `fixtures/v1/reminder_edits.json`
  (a line, its path and the `[daily]` config, an edit, and the line it becomes), read by
  `tests/fixtures.rs` and `SyncCoreFixtureTests.swift`, like `hashes.json` and `paths.json`.
- **An index fixture** in `fixtures/v1/reminder_index/`: an index, the notes it links to and a
  pinned time, and the reminders they yield with their state and group. It is data only. Each side
  still reads the index itself, and the desk's index tests and `ReminderIndex`'s tests both read
  the fixture.
- **`line_hash` is unchanged** (§4.3), and so is `WRITE_VERSION`.
- **Settings sync:** `.thock/reminders.toml` is a `.toml` outside the excluded prefixes, so the phone
  reads the same `date_only_at`, `snooze_minutes` and `tomorrow_at`.
- **Version skew:** a phone without V45 shows tokens as text and never rings; a desk without V45
  applies the phone's appends and line edits as ordinary writes and keeps no index, so its links
  appear once a V45 desk runs.

The V34 API gains a short §7 note naming reminders as a reading over existing kinds, plus the
vector files in §9, with the sync-core change.

## 12. Settings

`.thock/reminders.toml`, its own file because `config.toml` rejects unknown tables and an older
build would refuse the vault (the reason calendar has `.thock/calendar.toml`):

```toml
# Where the index lives, from the vault root.
index = "reminders.md"

# Headings match loosely, like the planner's. The first name is written; the rest are aliases.
[headings]
open = ["Open"]
done = ["Done"]
day = ["Reminders"]       # the subsection under the day planner

[ring]
date_only_at = "09:00"    # when a reminder with no time rings
snooze_minutes = 60       # "In 1 hour"
tomorrow_at = "09:00"     # "Tomorrow 09:00", and the tab's reschedule choices
desk = true               # ring on the desk too
```

A missing file means these defaults. Set Language learns the three headings, as it learned the
backlog's (V19).

## 13. Rituals and the agent

### 13.1 Week Review: the sweep

A new step before the backlog offer, in `routines/timeline/skills/week-review.md`:

1. Read `reminders.md`, and through its links the home lines, and collect every open reminder due
   on or before the end of this ISO week, **earlier weeks included**, so a skipped review loses
   nothing.
2. Ask once with the `ask` tool: *N reminders are still open. Move them all to Monday at 09:00?*,
   with **Move all to Mon 09:00**, **Let me pick** and **Leave them**.
3. On **Let me pick**, ask per reminder, with its text and due: **Tomorrow 09:00**, **Next week**
   (Monday 09:00), **Pick a date** (a follow-up question), **Close** and **Drop the reminder**.
4. Write only the home line, found by its id: rewrite the token (full date unless the home is the
   new day's note), tick it or strike the token, or remove the token and keep the id to drop it.
   Never touch `reminders.md`; the desk keeps it.

### 13.2 Wrap Today and Wrap Yesterday

- **The same question for the day's leftovers:** the open reminders due that day, in one question
  (*Move to tomorrow 09:00*, *Let me pick*, *Leave them*).
- **`# Daily Closure` quotes** open tasks without their token and id. The closure is a record, not a
  reminder.
- **Carrying a task with a reminder to the backlog** keeps its token and id, which makes it a copy
  of the same reminder (§5.3). The comparison that removes duplicates ignores tokens and ids, as it
  ignores the ✅ stamp.

### 13.3 What the agent is told

- **`AGENTS.md`** gains a short section. A reminder is `(@YYYY-MM-DD HH:MM)` or `(@YYYY-MM-DD)`
  anywhere on a line (the last one counts; set one at the end), or `(@HH:MM)` in a daily note. Never write or change a `<!--rem:…-->` id. A
  reminder with no home is a task under `## Open` in `reminders.md`. Close it by ticking it.
- **The hosted agent's context block** (V27) names the index path beside the backlog file, so "remind
  me Friday to call the bank" works in chat and in phone Ask with the agent writing the token.

## 14. Tests

- **Grammar** (`thock-sync-core`, both runners): every row of §4.1 and §4.2 as vectors, including a
  token in code, after a backslash, two tokens on a line, a struck token, time-only in and out of a
  daily note under two `[daily] filename` formats, a token on a `<!--gcal:` line, ids with other
  trailing comments, and mirror-line recognition.
- **Line edits** (`thock-sync-core`, both runners): close, rewrite, drop and attach on every §4.4
  line shape as vectors, including the short `(@HH:MM)` form only in the new day's daily note and a
  `<!--gmail:…-->` marker kept before the id.
- **Index reading** (the desk's index tests and ThockKit): every `fixtures/v1/reminder_index/` case
  yields the same reminders, states and groups on both sides.
- **Reading** (`reminder_reading.rs`): every row of §8.2 at a pinned clock, a bare hour on both
  sides of noon, *already past*, unknown words, and the daily-note short form.
- **Completion** (gpui): `(@` starts a query; the query `dec 2` keeps *Wed 2 Dec* after the fuzzy
  filter; `tab` writes the full date in `projects/house.md` and `(@10:00)` in the daily note of that
  day; a query with no reading closes the list so `enter` breaks the line; no list inside code or on
  a calendar line.
- **Chip** (gpui, on the conceal harness): each state of §8.4 folds to its label; the cursor's line
  shows the source; the buffer is never modified; the label changes at midnight.
- **Index service** (gpui with a fake `Fs`): an id is added after idle and off the cursor's line;
  one link per reminder; a cut and paste between notes retargets the link; a deleted line becomes
  `removed` after the grace period; a dropped token becomes `dropped`; a tick on a copy ticks the home
  line; a link deleted by hand comes back; a phone-appended loose line is taken as is; startup
  re-parses only changed files; two workspaces on one vault get one service.
- **Day reconciler** (pure, like `calendar.rs`'s `reconcile`): insert order, replace on a home
  change, freeze on a person's edit, strike on a move, tick on a close, carried reminders only in
  today's note, no repeat of reminders whose home is the note, never a delete, the hold without a
  planner heading.
- **Planner** (`day_plan.rs`): token and comments stripped from labels, a mirror line is a point, a
  token time on a planner line adds a point, the Next card counts down to a point and never calls it
  *Now*.
- **Notifier** (gpui, test clock): rings at the due minute; date-only at `date_only_at`; no repeat
  after a restart; one summary for what came due while closed; each answer edits the home line as
  §6.3 says; `desk = false` stays silent. On the test platform, `set_app_identity` is called first,
  since gpui drops notifications until it is.
- **Reminders tab** (gpui, `VisualTestContext` + `simulate_keystrokes`): `j` `k` `g g` `shift-g`
  move; `x` ticks the home line through `Fs`; `s` then `1` rewrites the token; `d` removes it and
  keeps the id; `enter` opens the line; the selection survives a file event; `3` reaches the tab and
  `4` reaches Done; `escape` returns focus. Chords run under `MultiWorkspace::test_new`, which is
  where Workspace bindings exist in tests.
- **Phone** (ThockKit): the capture builder for `.reminder` emits one `append` with a token and an
  id; `chip(for:)` never returns `.reminder`; *Remind me…* emits a `replace_line` and an `append`;
  the scheduler picks the soonest 64 and replaces only what changed; each notification action
  records exactly one write; the screen's groups match the index fixture's, as the desk's do.
- **Rituals:** a test reads `week-review.md` and `wrap-today.md` and parses every line shape they
  tell the agent to write with the sync-core grammar (CLAUDE.md, Build and test).

## 15. Delivery

1. **This spec** and the roadmap entry in `VISION.md` §12.
2. **Sync core** (§4, §11): grammar, daily-path rule, line edits, vectors and the index fixture,
   the Swift port. No protocol change.
3. **Desk reading** (§8): the chip, the suggestion list, the reading, the calendar port, rows outside
   the editor. Value on its own: tokens draw and type well before anything rings.
4. **Desk index, tab and day** (§5–§7, §9): the service, the Reminders tab, the reconciler, planner
   points and the Next card.
5. **Desk ringing** (§6.3): Linux first; macOS with or after V35.
6. **Rituals and agent** (§13).
7. **Phone** (§10): chip and sheet, the screen and the row, *Remind me…*, notifications. The silent
   push wiring from V34 (APNs sender in `thock/services/plus`, the entitlement, token registration)
   travels with it as its own change; merging that deploys the service.
8. **Later:** repeating reminders, inbox `due:` dates, widget timeline entries.

## 16. Decision log (2026-10-21)

1. **`(@…)`, the Obsidian Reminder plugin's format,** over `![…]` (V39's image menu opens on `![`, and
   phone photos are named by date, so a typed date lists pictures; `!` reads as "embed") and a bare
   `@…` (no closing bracket, and Obsidian would not see it).
2. **A reminder lives where it was written; `reminders.md` indexes it.** Full lines for reminders
   with no other home, links for the rest, no copied text or time. Rejected: an index holding a
   copy of every reminder (two copies that drift and conflict across devices) and crawling the
   vault on every read.
3. **Ids are Thock markers**, `<!--rem:…-->`, over Obsidian block references (`^id`), which show as
   text in plain editors and need embed support Thock does not have. Twelve hex digits like
   calendar's, not six, so a large vault never collides.
4. **The day section is live**, under the planner and kept like `### Calendar`, over a snapshot
   written once at creation (misses later reminders) and over drawing it without writing (the day's
   note would not record it).
5. **Both devices ring.** One reminder may ring twice; it is never missed because one device was off.
6. **Date-only reminders ring at 09:00**, configurable, as iOS Reminders rings all-day ones.
7. **Closing ticks a task and strikes the token in prose**, so a reminder in a paragraph can close
   without a checkbox.
8. **A suggestion list with a calendar one row down**, over a calendar open while typing (it would
   need its own keys for the day and fight the caret for the arrows).
9. **Copies keep the id** and stay one reminder; ticking any copy closes it.
10. **Today's section carries what is still open from earlier days**, so Overdue is in front of you
    every morning, not just on the tab.
11. **A reminder is a point on the planner**, not a 30-minute block.
12. **One notifier per vault, one summary on opening.** Two windows never ring twice, and a closed
    desk never greets you with a burst.
13. **Backlog · Today · Reminders · Done**, so Done moves from `3` to `4`.
14. **The phone's *Reminder* chip is never remembered**, and every card gets *Remind me…*.
15. **The sweep covers every open reminder up to this week**, earlier weeks included.
16. **Dropping removes the token and keeps the id** (refined while writing this spec; the design
    reference says the id goes too). A line with an id and no token is unambiguously a dropped
    reminder, so the desk never has to guess between *dropped* and *line deleted*. The id is never
    drawn, so nothing a person sees changes.
17. **`line_hash` stays as it is.** It already ignores trailing comments, so ids never disturb sync;
    making it ignore tokens too would change the hash for existing lines and need a version gate,
    for a conflict that already resolves into one reminder.
18. **The desk owns the index's bookkeeping;** the phone appends its own captures and links. Every
    phone gesture stays inside the write kinds it has.
19. **Its own settings file**, `.thock/reminders.toml`, because `config.toml` rejects unknown tables.
20. **Local wall-clock times.** A reminder rings at its time in whatever time zone the device is in.
21. **Home-line edits live in the sync core** beside the grammar (decided while planning the build).
    Close, rewrite, drop and attach are ported to Swift and checked by shared vectors, so the desk,
    its notifier and the phone write byte-identical lines. Rejected: the same edits written once on
    each side, which can drift with nothing to catch it.
22. **A shared index fixture, data only** (decided while planning the build). The desk and the
    phone each read `reminders.md` themselves, and `fixtures/v1/reminder_index/` holds the cases
    both must read the same way. Rejected: moving index parsing into the sync core, which would
    grow the step every other module waits on.
