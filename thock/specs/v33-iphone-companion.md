# Thock V33 — Thock on iPhone: a pen for the vault, not a second desk

**Status:** Design accepted (2026-10-02) — UX and feature set decided; data model and sync are a
separate spec
**Owner:** Diego · **Date:** 2026-10-02
**Design reference:** the interactive design document with the mockups, navigation models and
the option cards this spec resolves: <https://claude.ai/artifact/UqePrnerog5SmibGeLD9ZD>. Layout
questions go there first; this spec records the decisions and the vault contract.
**Companion docs:** `v13-inbox-routine.md` (the inbox note format and the triage log this app
writes to and reads from), `v6-backlog.md` (Soon / Someday, the move-to-backlog flow), `v4-day-planner-panel.md`
(the checklist line grammar), `v10-markdown-conceal.md` and `v24-inline-markdown-conceal.md` (the markup
vocabulary the phone editor adopts), `v25-thock-plus-hosted-agent.md` and `v27-agent-session-prompt.md`
(the agent the phone talks to), `v31-readwise-sync.md` (the `reference/` folder the clips sit beside)

---

## 1. Summary

Everything Thock knows arrives through a keyboard at a desk. V13 gave the vault a front door for
the moments away from it, but the transports it shipped (a Google Tasks list, a Gmail label) are
borrowed: the capture happens in someone else's app and the vault only learns about it on the next
poll. V33 is Thock's own phone app, and it is deliberately **not** a mirror of the desktop. The
desktop plans the day and the week and runs the rituals. The phone catches what happens in between.

Five jobs, and only five:

| Moment | What the phone does | Where it lands |
| --- | --- | --- |
| An idea pops to mind | A capture sheet, two seconds to a blinking cursor | `inbox/` (default), or appended to today or to the backlog |
| A quick journal entry | A timestamped paragraph, with the day's earlier entries visible | `daily/<today>.md` under `## Journal` |
| An article to keep | Share Sheet → link, your one line, and the readable text | `inbox/` link note + `reference/clips/<slug>.md` |
| Review the plan, change a thing or two | Today and this week, drawn from the note; tick, retime, add, move to Soon | One line changed in `daily/<today>.md`, `weekly/<week>.md`, `backlog.md` |
| Ask the agent | The hosted Thock Agent, reading the vault | Nothing, unless you keep the answer |

The one promise that makes the rest safe: **the phone appends, drops things in the inbox, and
changes single lines. It never files, never rewrites a note, never decides.** That is what lets
someone write there without thinking, the feeling Google Keep gets right, and it is the same split
V13 drew between capture and triage.

Editing does not feel like Markdown. The editor shows headings, checkboxes and bold the way Apple
Notes does and writes the strict subset of Markdown the desktop already conceals (§8). Anything
outside that subset is shown as an untouchable block and written back byte for byte.

Sync is not in this spec. The whole vault is assumed to be available locally on the phone; how it
gets there is the data-model spec that follows (§19).

## 2. Goals & success criteria

- From the Lock Screen to a blinking cursor in one gesture; from cursor to saved in one tap.
  A capture never requires unlocking the phone.
- A note edited on the phone opens on the desk, with conceal mode on, looking identical to one
  written there. Nobody at the desk can tell which lines came from the phone.
- A file the phone has displayed but not edited has **zero bytes changed** by the phone.
- Every write the phone makes is an append or a single-line edit, into a named small set of
  places (§14). Two phones, or a phone and the desk, appending to the same section can always
  both be kept.
- Every capture shows what became of it: *waiting for the desk*, or *filed → Backlog · Someday,
  Tue*. The phone never has to be told the system works; it shows it.
- The app opens on today, with writing on screen. If a session ends without a word written, the
  layout failed.
- No ritual runs from the phone. No rituals, no Routine authoring, no settings beyond picking the
  vault and connecting the agent.
- The word *git* does not appear. Neither do *file*, *sync*, *Markdown* or *frontmatter*. The
  app says note, today, inbox, filed, waiting for the desk — the chat panel's vocabulary (V26).

## 3. Non-goals

- **A general Markdown editor on the phone.** Tables, code blocks, HTML comments and frontmatter
  render as opaque blocks and are never editable there. Whole-note editing is not offered.
- **Running rituals.** "Wrap my day" gets a polite *that one runs at the desk*. Rituals write
  memory and dashboards and deserve the checkpoint the desk takes before them.
- **Browsing the vault.** No file tree, no folder view. The Routine collections (Books, Authors)
  are a later tier.
- **Sync, conflict handling, vault storage.** Deferred to the data-model spec (§19). This spec
  only guarantees the write contract that makes sync solvable.
- **Mirroring the desk's theme and fonts** (option I3 in the design doc). Settings live outside
  the vault and Zed palettes are code-editor palettes.
- **Android, iPad layouts, Apple Watch.** Watch is a later tier; the rest is unplanned.
- **Agent-suggested destinations at capture time** (option B3). It breaks *nothing decides on
  the phone* and needs the agent online for a two-second job.

## 4. Core concepts

### 4.1 The pen and the desk

| iPhone — the pen | Desktop — the desk |
| --- | --- |
| Capture an idea, a task, a link | Plan the day and the week |
| Journal in a few lines | Run the rituals: Wrap Today, Week Review, Triage |
| See today and this week; tick and nudge | Long-form writing and editing |
| Ask the agent a question | Backlog grooming, Routines, setup |
| See what the desk did with your captures | History and restore |

The vault is the only shared thing. Both apps read the same files; they touch different parts.

### 4.2 Zero-fear capture

Everything written on the phone goes somewhere reversible: appended to today or the backlog, or
into the inbox. A capture can be wrong, half-finished or a duplicate and the vault is still fine.
Triage (V13 §9) is the filter, and it runs at the desk.

### 4.3 Natural text, Markdown underneath

The user never sees a `#` or a `- [ ]`. The editor's vocabulary is exactly the set of markup the
desk's conceal mode folds away (V10, V24): headings, emphasis, lists, checkboxes, quotes, links,
rules. Each button maps to one Markdown form. The two apps agree on what a note *is* because they
share the list.

### 4.4 Glance and nudge

Today and the week are readable at a glance and editable in small, structured ways: tick, set a
time, add a line, move to Soon, edit one line, remove one line. Replanning is a desk activity
with the Day Planner beside you. The one escape hatch is **edit one section** (§7.3): an explicit
"I asked for an edit" gate that rewrites only that section's lines, after showing what will change.

### 4.5 Receipts

A capture's row shows its state, read from the vault itself: *waiting for the desk* while the
inbox note exists, *filed → <destination>* once the triage log (V13 §9.5) carries its marker.
Trust comes from seeing the loop close.

### 4.6 Two levels of trust

Writing a capture from the Lock Screen needs no unlock. Reading anything from the vault (today's
note, the week, receipts, the agent) needs the phone unlocked. Decided (§17 #11): the fastest path
is for writing, and nothing the vault holds is shown without Face ID.

## 5. Navigation — the Today canvas

Decided: **A1, Today canvas**, with A2's capture feed as the destination of the Inbox row.

The home screen *is* today's note, drawn as cards from the note's own sections:

```
┌──────────────────────────────┐
│ TODAY · WEEK 40              │   date in Petrona
│ Thursday, October 2          │
│ ─ JOURNAL ────────────────── │
│ 08:10  Slept badly, but …    │   paragraphs, timestamps as the phone wrote them
│ 13:02  Noticed I keep …      │
│ ─ DAY PLANNER · 2 of 6 ───── │
│ ☑ 08:00        Morning walk  │   the checklist lines, calendar lines read-only
│ ☐ 09:30–11:00  Deep work …   │
│ ☐ 12:30 ▎      Lunch with Ana│
│ ☐              Buy a card    │
│ ─ INBOX · 3 waiting ──────── │   opens the capture feed with receipts
│ ─ PERSONAL ───────────────── │   any other section, read-only prose
│                              │
│ ( Write an idea, a journal   │   compose dock, always present:
│   line, a clip, or ask.     )│   each noun opens its capture
└──────────────────────────────┘
```

- **Sections come from the note.** A `##` heading is a small-caps card label; a `___` rule is a
  hairline; prose is prose. A customised template changes the app. `## Journal` and
  `## Day planner` are recognised by the vault's configured headings (`[day_planner] heading`,
  V26's normalised match) and get their special rendering; everything else is a read-only prose
  card.
- **Swipe sideways** moves a day. **The week** is a segmented control at the top (Today / Week).
  The week screen carries a day strip that jumps to any day's canvas, the weekly note's sections
  (`## Goals` ticks like the planner), and the agent's `# AI Week Review` in the agent's voice
  once it exists.
- **The compose dock** is one sentence whose nouns are the entry points: *idea* and *journal*
  open the capture sheet (§6) with the destination preset (Idea → Inbox, Journal → today's
  Journal), *clip* the Clip sheet, *ask* Ask. Tapping the rest of the sentence opens the capture
  sheet with no preset.
- **Launch behaviour** (decided, H1): every cold launch is today. No restoring the last screen.
  Entry points from outside the app (§9) open their sheet over today.

## 6. Capture

### 6.1 The sheet

Rises over whatever is open, keyboard up, cursor blinking. First line becomes the inbox note's
title. Destination chips below the text (decided, **B2**, amended): **Today** selected by default,
**Inbox** and **Backlog** beside it. The chip last used is remembered per entry point, so the
dock can learn "Backlog" while the Lock Screen control stays on Today. A short hint under the
chips names the destination in plain words: *Lands in your inbox. Triage sorts it at the desk.*
The hint always reserves two lines so the chips never move when the selection changes.

Done saves. Swiping the sheet away also saves — a capture is never lost to a gesture. An empty
sheet is discarded silently. One haptic and a one-line toast name where it went.

### 6.2 What is written

| Chip | Write |
| --- | --- |
| Inbox | `inbox/<YYYY-MM-DD>-<HHmm>-<slug>.md` in the V13 §6 format, `source: thock-ios`, `capture:` the digest of (device id, capture instant), `captured:` the local instant. Body is the text as the editor wrote it (§8). |
| Today | Appended at the end of today's `## Day planner` as `- [ ] <first line>` when the capture is one line, otherwise appended to `## Personal` (or the last section before any agent heading) as prose. Today's note is created from the template if missing. |
| Backlog | Appended under the configured **Soon** heading of `backlog.md` as `- [ ] <first line>`, below the last task of the loose group (V17). Extra lines, if any, become the task's own indented continuation. |

A Today or Backlog capture does not go through triage and therefore gets no triage-log line, and
it is not listed in the inbox feed (§6.3); the toast names where it went.

### 6.3 Receipts

The Inbox row on the canvas opens the inbox feed: every inbox capture the phone made, newest first,
with a state read from the vault on each refresh, then any other note waiting in `inbox/`. Today
and Backlog captures never pass through the inbox, so the feed leaves them out; they show where they
landed. A note still waiting opens in the capture editor and can be edited until triage files it
(the first line is its title; the heading and body are rewritten, the front matter is left as
captured, since no write reaches it):

| State | Evidence |
| --- | --- |
| waiting for the desk | `inbox/<file>` still exists |
| filed → `<destination>`, `<day>` | a triage-log line whose `<!--inbox:<digest>-->` matches; destination is the text after `→` |
| discarded | log line with destination *Discard* |
| gone | note missing and no log line (deleted by hand at the desk) — shown muted, not as an error |

The triage log format (V13 §9.5) becomes a parsed contract on the phone side. It is stated
exactly in the triage skill body already; this spec adds a desk-side test that the skill's example
line parses (§16).

## 7. The plan

### 7.1 Today

Each planner line renders as a row: checkbox, time chip, text. Calendar lines (V8, under the
`## Calendar` subsection) carry the calendar colour bar and are read-only, as on the desk. Done
lines strike through. Subsection headings under the planner render as group labels in the same
hashed colour the Day Planner uses (V8 §11), so the two surfaces look related.

Tap ticks. Long-press opens the nudge menu. Every move changes **one line** in the note:

| On the phone | In the note |
| --- | --- |
| Tick | `- [ ]` ↔ `- [x]` on that line |
| Set a time | time prefix inserted or replaced: `- [ ] 15:00 - 15:30 Call the dentist` (the V4 grammar) |
| Add a line | appended at the end of `## Day planner`, or of the subsection the add was started from |
| Edit this line | the line's text replaced; checkbox state and time prefix preserved |
| Move to Soon | line removed from the planner; `- [ ] <text>` appended under **Soon** in `backlog.md` (decided, §17 #14 — the desk's wrap flow moves unfinished work to the backlog, never to tomorrow, so the phone does the same and never creates a note a day early) |
| Remove | line deleted, with a few seconds of undo. The only destructive move, and it is one line. |

### 7.2 The week

The weekly note, same rendering rules. `## Goals` checkboxes tick. Prose sections are read-only
cards. The `# AI Week Review` (or whatever heading the ritual wrote, recognised as the agent's by
being a level-1 heading after the user's sections) renders in the agent's amber voice. The phone
never produces it.

### 7.3 Edit one section (decided, E2)

Long-press a section's heading → that section alone opens in the editor (§8). Saving shows a
one-screen before/after of the lines that will change and asks once. The rewrite is scoped to
the lines between that heading and the next heading of the same or higher level; nothing outside
them is touched. This is the explicit *you asked for an edit* gate the invariants require for
anything beyond an append.

## 8. The editor

Decided: **G1**, a strict-subset rich-text editor with opaque blocks.

| What you see | What is written |
| --- | --- |
| Paragraph | paragraph, one blank line between |
| **Bold**, *italic* | `**bold**`, `_italic_` |
| • bulleted list | `- item` |
| ☐ checklist item | `- [ ] item` / `- [x] item` |
| time chip on a checklist item | `- [ ] 09:30 - 11:00 Deep work` |
| Heading | `##` at the level the surrounding section uses |
| Quote | `> quote` |
| link to a note (`[[` autocompletes titles) | `[[note]]` |
| link to a web page | `[text](https://…)` |
| divider | `___` |
| anything else: table, code block, HTML comment, frontmatter, raw HTML | shown as a grey block with its first line, preserved exactly, not editable |

Rules:

- **Round-tripping.** The editor keeps the source lines of every block it displays. On save it
  emits only the blocks the user touched, from the subset, and copies every other block through
  unchanged. A block the user never touched is byte-identical.
- **No whole-file rewrite.** Even edit-one-section (§7.3) writes only the section's line range.
  Everything else is an append or a single-line replacement.
- **The formatting bar** sits above the keyboard with five items: bold, italic, list, task,
  link. Heading and quote hide behind a long-press on the bar; a capture rarely needs them.
- **What the desk sees** is the test: a note written on the phone opens on the desk in conceal
  mode and looks like one written there.

## 9. Journal

Decided: **C1, amended** — new entries append; earlier entries stay editable.

- The Journal entry point (quick action, Lock Screen control, Siri) opens today's `## Journal`
  section with the day's entries visible and a new paragraph started at the bottom, prefixed by
  the time: `**21:14** · ` in the note, drawn as a small time label on the phone. Today's note
  is created from the template if missing.
- Opening the entry point within ten minutes of the last phone entry continues that paragraph
  instead of starting a new timestamp, so a thought typed in two bursts stays one thought.
- **Earlier entries are editable in place** (Diego: *I don't want to be looking at a typo all
  day waiting for the keyboard*). Tapping an earlier paragraph makes it live; saving replaces
  only that paragraph's lines, through the same scoped-rewrite machinery as §7.3, without the
  before/after screen — the scope is one paragraph the user tapped on purpose. Paragraphs under
  an agent heading (`# Daily Closure` and friends) are never editable on the phone.
- **The desk adopts the timestamp** (decided, §17 #12): the shipped daily template's Journal
  prompt mentions it (*Entries from your phone start with the time, like `**21:14** ·`; feel
  free to do the same here*), and Wrap Today / Wrap Yesterday are told to read `**HH:MM**`
  prefixes as the order of the day. The example first day (V22) gains one timestamped entry so
  the convention is seen before it is met. No parsing on the desk depends on it.

## 10. Clip

Decided: **D2** — an inbox link note plus the readable text under `reference/clips/`.

Entry: the Share Sheet, from Safari, Mail, Messages, a reader app. Thock itself never opens.

The clip sheet shows the page title and URL, any text selected in the source as a quote, a
one-line field for *why you kept it*, and two toggles: **Keep the article text** (on by default)
and **Also make a task to read it** (off). Save writes:

1. `reference/clips/<slug>.md` — the readable text, extracted on the phone (reader mode,
   images off), with frontmatter `source: clip`, `url:`, `title:`, `clipped:`. Create-if-missing;
   a second clip of the same URL updates nothing and only the inbox note is written.
2. `inbox/<YYYY-MM-DD>-<HHmm>-<slug>.md` — V13 format, `source: thock-ios`, body: the user's
   line first, then the link, then `[[clips/<slug>]]` when the text was kept, then the selected
   quote if any.
3. With the task toggle on: nothing extra on the phone. The inbox note carries `kind: read` in
   its frontmatter and the triage policy proposes *Backlog · Someday, carrying the link*.

The triage policy gains one shipped row (desk-side change): *A clip from the phone (a link note
pointing at `reference/clips/`) → Backlog · Someday as a task carrying the wikilink; the clip
itself stays where it is.* Reading Week may quote clips in a later tier; not in this spec.

Readwise Reader as a clip destination (D3) is a later tier (§18).

## 11. Ask

Decided: **F1** — the hosted Thock Agent, Plus only, with no on-phone BYO path. Amended 2026-10-03 by
`v35-phone-ask.md`: the agent's loop runs on the phone against its own copy of the vault, and the first
release is read-only apart from *Keep this* and one line to `memory/inbox.md` when the person tells it
something worth remembering. The appends by request below are V35's second pass.

- The Ask screen is the chat panel's grammar on a phone: the user's bubble, the agent's amber
  block, one quiet activity line per turn, the notes it read named as vault-relative paths under
  the answer (V26). Same session prompt as the desk (V27), same `memory/index.md` in context
  (V28).
- **Scope on the phone:** read everything; append to today, tomorrow, the backlog and the inbox
  when asked in words; never run a ritual, never rewrite. Same write scope as a capture, so the
  same safety.
- **Keep this** appends the answer to today's note under `# Asked on the go`, so the morning
  reader knows it came from a conversation and not from their hand. Wrap Today treats that
  heading as the agent's voice (desk-side one-liner in the skill).
- **One thread per day**, cleared at midnight. The desk keeps its own threads.
- **Without Plus** the tab explains itself in one paragraph and offers nothing else in this tier.
  *Ask later* (the question as an inbox item the desk's agent answers) is a second-tier feature.
- How the agent reaches the phone's copy of the vault is `v35-phone-ask.md`: it runs on the phone.

## 12. Entry points outside the app

The app people use every time is mostly the app they do not have to open.

| Entry point | Opens | Tier |
| --- | --- | --- |
| Lock Screen control (iOS 18 Controls) | capture sheet, no unlock | first |
| Action Button | capture sheet | first |
| Lock Screen and Home widgets | next three planner lines; tap a line to tick (unlock required) | first |
| App icon quick actions | Idea · Journal · Clip · Ask | first |
| Share Sheet extension | clip sheet | first |
| Siri and Shortcuts | *Add to Thock …* dictates a capture; *What's on my plan?* reads today | second |
| Spotlight | note titles and captures | second |
| Apple Watch | dictate a capture; tick today's lines | later |

## 13. Visual language

Decided: **I1** — SF Pro for everything the user writes and reads in body text, Petrona for the
date header and note titles, Schibsted Grotesk or SF for chrome. Mono appears only in receipts
and the agent's source lines, where a path is the honest thing to show.

- **Ground and ink** from the website's tokens: the warm dark ground by default, the same light
  theme the Getting Started flow offers. Surfaces are flat; only sheets and menus, which sit
  physically above the page, get a shadow.
- **Amber is the agent and the primary action,** nothing else. A plan line is ink, a calendar
  line is the desk's calendar blue, a receipt's *filed* dot is green.
- **Structure comes from the note** (§5). The phone draws what the file says.
- **Motion:** sheets rise, days slide, ticks settle. System curves, one motion each.
- **Words:** note, today, inbox, filed, waiting for the desk. Never file, sync, Markdown, git.

The design document holds the mockups for every screen in this spec; it is the layout reference
during implementation.

## 14. The write contract (the whole safety model)

The phone writes **only**:

| Where | How |
| --- | --- |
| `inbox/*.md` | create-if-missing; while it waits for triage, replace its level-1 heading line and that heading's section |
| `reference/clips/*.md` | create-if-missing |
| `daily/<today>.md` | create from template if missing; append under `## Journal`, `## Day planner`, `## Personal`, `# Asked on the go`; replace one planner line; replace one journal paragraph; replace one section's line range after confirmation |
| `daily/<other day>.md` | the same single-line planner edits, from the day strip |
| `weekly/<week>.md` | tick one `## Goals` line; replace one section after confirmation |
| `backlog.md` | append one task under **Soon** |
| `memory/inbox.md` | append one dated line, by the agent during Ask (V35 decision 5); Reflect files it at the desk |

Nothing else, ever. Not the rest of `memory/`, not `routines/`, not `.thock/`, not templates, not the triage
log (the phone only reads it). The desk owns every other write. Two appends to the same section
can always both be kept, which is what makes the deferred sync problem solvable rather than
hopeful.

The phone keeps one small record of its own (device-local, outside the vault): the captures it
made with their digest, destination chip and instant, so receipts can say *added to Today* for
writes that leave no inbox note.

## 15. Desk-side changes

Nothing on the desk is required for the phone to work; the inbox folder, the daily note and the
backlog are already its front door. The changes below make the loop legible and are small:

1. Sync popover (V32): the Inbox row says *3 waiting · 2 from your phone* when any waiting note
   has `source: thock-ios`.
2. Shipped triage policy: the clip row (§10).
3. Daily template, example first day, Wrap Today / Wrap Yesterday: the journal timestamp
   convention (§9).
4. Wrap Today: `# Asked on the go` is the agent's voice, not the user's (§11).
5. Triage skill: the triage-log line format is now parsed by the phone; a test pins the example
   line's shape (§16).

All prose and template edits in `crates/thock/assets/`; one string change in the sync popover.

## 16. Tests

This spec ships no code, but it fixes contracts the implementation must test:

- **Round-trip:** for every shipped template, the example first day, and a corpus of real-shaped
  notes (tables, code, comments, frontmatter, nested lists), open on the phone editor, save
  without edits → byte-identical. Edit one paragraph → only that paragraph's lines differ.
- **Single-line moves:** each row of §7.1 against a planner with subsections, calendar lines and
  a done line → exactly one line changed (two files for Move to Soon), everything else identical.
- **Capture format:** an Inbox capture parses with the desk's V13 inbox reader; `capture:` is
  stable across retries; a Today capture of one line lands as a planner task, of several lines
  as prose.
- **Receipts:** the triage skill's example log line parses to (date, title, destination, digest);
  a missing note with no log line reads as *gone*, not as an error.
- **Desk side:** the triage-log example in `triage-inbox.md` parses with the same parser the
  phone will use (shared fixture in `crates/thock/`).

## 17. Decision log (2026-10-02)

From the design document's option cards, with Diego's answers:

1. **Navigation: A1, Today canvas.** Home is today's note drawn as cards; the capture feed with
   receipts sits behind the Inbox row (A2's best part, kept).
2. **Capture destination: B2, amended.** Today by default; Inbox and Backlog chips; last chip remembered
   per entry point.
3. **Journal: C1, amended.** New entries append with a timestamp; earlier entries are editable in
   place (paragraph-scoped rewrite). Agent sections are never editable.
4. **Clip: D2.** Inbox link note plus readable text under `reference/clips/`.
5. **Plan editing: E2.** Single-line nudges, plus edit-one-section behind a before/after.
6. **Agent: F1.** Hosted Thock Agent, Plus only. *Ask later* is second tier.
7. **Editor: G1.** Strict subset, opaque blocks preserved byte for byte.
8. **Launch: H1.** Always open on today.
9. **Type: I1.** SF for writing, Petrona for dates and titles.
10. **First release: J1.** Capture, journal, clip, nudges, receipts, entry points. No agent, no
    week view (§18).
11. **Face ID:** two levels. Capture without unlock; anything read from the vault needs unlock.
12. **Journal timestamps:** the desk's template adopts the `**HH:MM** ·` convention so a day reads
    the same wherever it was written.
13. **Clips folder:** `reference/clips/`, with a line in the shipped triage policy.
14. **Move to tomorrow → Move to Soon.** The nudge menu sends an unfinished line to the backlog's
    Soon, matching the desk's wrap flow, and never creates tomorrow's note early.
15. **Vault choice and sync:** deferred to a separate data-model spec. This spec assumes the
    whole vault is local and fixes only the write contract (§14).

## 18. Tiers

**First release** (decided, J1): Today canvas (journal, planner, inbox count, prose cards); idea
capture with chips; journal append with editable earlier entries; Share Sheet clip with kept text;
tick, set a time, add a line, edit a line, move to Soon, remove; receipts; Lock Screen control,
Action Button, widgets, icon quick actions; the subset editor. No agent, no week screen.

**Second:** the week screen with the day strip and goal ticks; edit one section; Ask (hosted
agent, one thread per day) and *Ask later* for everyone else; Siri and Shortcuts; Spotlight.

**Later:** Apple Watch; Routine collections on the phone; clip to Readwise Reader; the memory
page, read-only; history — see a note as it was, restore a line.

## 19. Deferred and open

- **Data model and sync** — `v34-vault-sync.md` (2026-10-02): where the vault lives on the phone,
  how the hosted agent reaches it, what happens when both ends append to the same section between
  syncs.
- **Platform and language** for the app itself (Swift/SwiftUI is the obvious reading of
  *feels like an iPhone*; nothing here depends on it). Decided with the data-model spec, since
  the vault storage choice constrains it.
- **Reader-mode extraction quality** for clips; which sites fail and whether a *keep the link
  only* fallback should be automatic.
- **Journal timestamp locale** — `**21:14**` is 24-hour; whether to follow the device's clock
  format or keep the note's form stable across devices.
