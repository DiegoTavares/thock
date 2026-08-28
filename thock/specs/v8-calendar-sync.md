# Thock V8 — Calendar sync into the daily note & sectioned Day Planner

**Status:** Implemented (2026-08-18)
**Owner:** Diego · **Date:** 2026-08-18
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.3 Augmentation not replacement, §4.4 Human-in-the-loop, §4.6 Modular life, §7 Data connectors), `v4-day-planner-panel.md` (the parsing model and panel this extends), `v7-dynamic-routines.md` (the Routine this ships inside)

---

## 1. Summary

Today's meetings live in Google Calendar and today's plan lives in a Markdown file, and the user
retypes one into the other every morning. V8 closes that gap in the direction that respects the
vault: a background service pulls the day's events and **maintains a `## Calendar` subsection inside
the daily note's `# Day planner` section**, as ordinary Markdown checklist lines. Because they are
ordinary checklist lines, the existing Day Planner panel renders them as time blocks with no new
plumbing.

Two deliverables:

1. **Calendar sync (§4–§10)** — a `CalendarProvider` abstraction with a Google Calendar REST
   implementation: OAuth 2.0 loopback + PKCE, refresh token in the system keychain, a day-bracketed
   `events.list` poll every few minutes, and a **reconciler** that inserts new meetings, corrects
   times on moved ones, marks cancelled ones, and never touches a line the user has edited. Read-only
   toward Google in V8.
2. **Sectioned Day Planner (§11)** — `parse_day_plan` learns which subsection each task came from,
   and the panel colors blocks and chips by a **stable hash of the subsection name**. This is a
   prerequisite for (1): the reconciler needs to locate its section, and the panel needs to
   distinguish calendar blocks from hand-written ones at a glance.

The whole feature belongs to the **Timeline Routine** — it has no meaning in a vault without daily
notes, and every file it touches is already Timeline's.

## 2. Goals & success criteria

- **G1** — Today's accepted meetings appear in today's note within ~5 minutes of being created or
  moved in Google Calendar, without the user doing anything.
- **G2** — Ticking a meeting off, adding a sub-bullet under it, or rewriting its title survives every
  subsequent sync, forever. This is the acceptance test that matters most.
- **G3** — The note is still a plain Markdown checklist. Opened in any other editor, the Calendar
  section reads as normal tasks; the only foreign matter is a trailing HTML comment.
- **G4** — Every subsection under `# Day planner` renders in its own colour in the panel, stable
  across re-parse, restart, and days.
- **G5** — Nothing in the sync path can lose an unsaved keystroke.
- **G6** — A vault with no calendar connected behaves exactly as it does today. Zero new empty
  states, zero prompts, no `## Calendar` heading created speculatively.

**Success:** a week of use in which the author never manually types a meeting into a daily note, and
never loses a checkmark to the syncer.

## 3. Non-goals (explicitly out of V8)

- **Write-back to Google.** Editing a time in the note does not move the meeting. The note is a
  mirror (§8.4 defines what happens to divergence).
- **Any calendar other than the current day.** No week view, no weekly-note calendar, no past
  backfill. The weekly-note calendar remains a V-next Context-rail item.
- **Providers other than Google.** The trait exists so Outlook / CalDAV / EventKit can follow; V8
  ships one implementation.
- **MCP as the transport.** Deferred (§4.3) — the roadmap's MCP connector onboarding item stands on
  its own and is not blocked by this.
- **Event creation, RSVP, or attendee management from Thock.**
- **Reminders, attachments, conference links as structured data.** A meeting is a time, a title, and
  an id. A hangout link may ride in the title if it is already there; nothing is synthesised.
- **Recurring-event expansion by Thock.** `singleEvents=true` makes that Google's problem.

## 4. Core concepts

### 4.1 The mirror, not the source

Google Calendar is authoritative for *when a meeting is*. The note is authoritative for *what the
user did about it*. The reconciler only ever writes the first kind of fact and only ever reads the
second. A synced line is therefore **jointly owned**: the time token belongs to Google, the checkbox
and everything the user typed belongs to the user, and the title belongs to Google until the user
changes it — after which it is theirs (§8.4).

### 4.2 The line is the record

There is no shadow database of what is in the note. The note *is* the state, and the event id
travels in the line:

```
- [x] 10:00 - 10:30 API Leads meeting <!--gcal:9f2c1ab4e7d0-->
```

`.thock/state/calendar/` holds only what cannot live in the note — OAuth account, ETags, and last
sync outcome. Deleting that directory costs a full re-fetch and nothing else; the note still
reconciles correctly because every synced line carries its own identity.

### 4.3 Transport is behind a trait, and Google is the first one

```rust
pub trait CalendarProvider: Send + Sync {
    /// Events overlapping the local day, already normalized and filtered.
    /// `Fetched::Unchanged` means the provider proved nothing moved.
    fn fetch_day(&self, date: NaiveDate, cx: &AsyncApp) -> Task<Result<Fetched>>;
}
```

Native REST won over the alternatives on freshness and testability; the discussion is recorded in
§14. The trait exists because macOS EventKit (no credentials, covers Exchange/iCloud for free) and
an MCP connector (the direction VISION §7 commits to) are both plausible second implementations, and
neither should require touching the reconciler.

### 4.4 Derived, never authoritative — extended

V4 §4.3 said the Day Planner panel derives everything from the note and never writes to it. That
still holds: **the panel is not the syncer**. The service writes to the note; the panel re-parses it
like it would any other edit. The only new panel responsibility is displaying sync status (§10.3).

## 5. The Calendar section

### 5.1 Placement

The section is a **child heading inside the Day Planner section**, one level deeper than the
configured planner heading:

```markdown
# Day planner

- [ ] Workout
- [x] Planning

## Calendar

- [x] 10:00 - 10:30 API Leads meeting <!--gcal:9f2c1ab4e7d0-->
- [ ] 14:30 - 15:30 1:1 Ramon <!--gcal:3b81ff09c2ae-->

## Misc

- [x] Send the sandbox instructions
```

Rules:

1. The heading text is `section` from config (default `Calendar`), matched **case-insensitively**
   against child headings of the planner section, same matcher as V4 §5.2.
2. If it does not exist, it is created **only on the first sync that yields at least one event** —
   never speculatively (G6). It is inserted as the **last** child subsection of the planner section,
   preceded by a blank line.
3. If the user moves the section, renames it in config, or re-points config at an existing section
   (`section = "Meetings"`), reconciliation follows it. The section is located by heading text, never
   by remembered line number.
4. If the planner heading itself is absent from the note, sync **holds** and reports
   `NoPlannerSection`. It does not invent a `# Day planner` heading in a note that has none.
5. Content inside the section that is not a synced line — free text, a stray task, a nested list —
   is preserved in place. The reconciler only edits lines it owns and only inserts between them.

### 5.2 The synced line grammar

```
- [<state>] <HH:MM> - <HH:MM> <title> <!--gcal:<id>[:<kind>]-->
```

- Deliberately a strict subset of V4 §5.1/§5.3, so every synced line is already a valid timed task.
- Times are **local**, zero-padded, 24-hour, with ` - ` as the separator (V4 accepts several; the
  writer emits exactly one).
- All-day events are written **without** a time token, making them unscheduled chips. They are off by
  default (§7.2).
- `<id>` is the first 12 hex characters of `sha256(calendar_id + "\0" + event_id)`. Google event ids
  are long and recurring instances carry a timestamp suffix; a short digest keeps the line readable
  while staying stable across renames, moves, and reschedules. `sha2` is already a crate dependency.
- `<kind>` records Google's `eventType` when it is one the planner lays out differently: `focus`
  (`focusTime`) or `ooo` (`outOfOffice`). Ordinary events carry no suffix, so their markers are byte
  for byte what V8 wrote. A line written before this existed has its marker upgraded in place on the
  next sync — the time token, title, checkbox, and any user trailing text are untouched. A suffix
  this build doesn't recognize still identifies the line (no duplicate insert) and is left alone
  rather than reset, so an older build can't fight a newer one.
- The marker is the **last** thing on the line. Exactly one space precedes it.
- The title has any occurrence of `<!--` stripped and internal newlines collapsed, so a hostile
  calendar entry cannot forge a marker or break the line.

### 5.3 What the marker costs

An HTML comment is visible in the raw Markdown and invisible in every renderer, including
`markdown_preview`. It is stripped from the Day Planner label (§11.4) so the panel never shows it.
The alternative — matching on `(start time, title)` — was rejected: a renamed or rescheduled meeting
then reads as a delete plus an insert, which duplicates the line and loses the checkbox precisely on
the days when the calendar is most volatile. The comment is the price of G2.

## 6. Authentication

### 6.1 Flow

Standard OAuth 2.0 for installed apps, reusing what is already in the tree:

1. `thock::ConnectCalendar` generates a PKCE verifier/challenge and starts the loopback listener from
   `oauth_callback_server`.
2. The system browser opens `https://accounts.google.com/o/oauth2/v2/auth` with
   `redirect_uri=http://127.0.0.1:<port>`, `access_type=offline`, `prompt=consent`, and scope
   `https://www.googleapis.com/auth/calendar.readonly`.
3. The callback yields a code, exchanged at `https://oauth2.googleapis.com/token` for an access token
   and refresh token. The browser gets the shared success page from `oauth_callback_server`.
4. `calendarList.list` populates the calendar picker (§6.3). The chosen ids are written to
   `.thock/calendar.toml`.

`state` is a random nonce checked on callback. The listener binds `127.0.0.1` only, and shuts down
after the first matching request or a 3-minute timeout.

### 6.2 Where credentials live

| Secret | Location |
| --- | --- |
| Refresh token | System keychain via `credentials_provider`, url `https://thock.local/calendar/google`, username = the account email |
| Access token | Memory only, re-minted from the refresh token on expiry |
| Client id / secret | Compiled in, overridable (§7.1) |

**No token ever touches the vault.** `.thock/calendar.toml` holds the account email and calendar ids
— identifiers, not credentials.

The client id is a Google *Desktop app* credential. Google's own guidance is that the accompanying
secret is not confidential for installed apps, which is why PKCE is mandatory here rather than
optional. `[google] client_id` / `client_secret` in `.thock/calendar.toml` override the bundled pair
for anyone who would rather use their own Cloud project.

### 6.3 The calendar picker

After a successful exchange, a `picker`-based modal lists every entry from `calendarList.list`
(summary, primary badge, current selection state) with space to toggle and enter to confirm. The
primary calendar starts selected. Confirming writes `calendars = [...]`. The picker is reachable
again later via `thock::ChooseCalendars` without re-authenticating.

### 6.4 Losing authorization

A `401` or an `invalid_grant` on refresh moves the service to `Disconnected`, stops the poll loop,
and surfaces a reconnect affordance in the panel header (§10.3). Nothing is deleted from the note —
the existing lines stay exactly as they are. `thock::DisconnectCalendar` deletes the keychain entry
and stops syncing; it does **not** touch the note (deleting the user's meetings is a human's call).

## 7. Configuration

### 7.1 `.thock/calendar.toml`

A separate file, not a new table in `config.toml`. `VaultConfigContent` is `deny_unknown_fields`
(V7 §9 trap 4), so a `[calendar]` table there would make every older build declare the whole vault
invalid. A sibling file parses independently and is ignored by builds that don't know it.

```toml
schema  = 1
account = "diego@example.com"

# Empty or absent = nothing syncs. Written by the connect flow's picker.
calendars = ["primary", "c_1a2b3c@group.calendar.google.com"]

# Child heading of [day_planner].heading that the syncer maintains.
section = "Calendar"

poll_seconds = 300      # clamped to 60..=3600

[filters]
accepted_only  = true   # skip declined and unanswered invites
include_solo   = true   # keep events with no other attendees
all_day        = false  # all-day events become unscheduled chips when true
private_busy   = false  # untitled "busy" blocks from calendars you can't read

[google]                # optional — overrides the bundled desktop client
# client_id     = "..."
# client_secret = "..."
```

Every field is optional and falls back to the defaults above; an unparseable file is a logged warning
and a disabled syncer, never a panic (the vault is hand-editable, V4 §9.4 precedent).

### 7.2 Which events pass the filters

An event is written when **all** of:

- it overlaps the local day, and
- it is not `status: "cancelled"`, and
- it is timed, or `all_day` is enabled, and
- it has a title, or `private_busy` is enabled, and
- **the user is going**: the `attendees` entry with `self: true` has `responseStatus` `accepted`; or
  there is no such entry and the user is the organizer (`include_solo`).

`declined` and `needsAction` are dropped. `tentative` is dropped by default — see §13 Q2.

### 7.3 `[day_planner].sections` (part B)

```toml
[day_planner]
heading = "Day planner"

[day_planner.sections]
Meetings = 3            # pin a palette index instead of the hashed one
Calendar = 5
```

Adding a key to an existing `deny_unknown_fields` table has the same forward-compat cost noted above,
bounded here to `[day_planner]`. Accepted deliberately: the override is a nicety, and the feature
works fully without the table.

## 8. Reconciliation (this is the contract)

A pure function, in `calendar.rs`, with no I/O and no GPUI:

```rust
pub fn reconcile(note: &str, events: &[CalendarEvent], config: &CalendarConfig) -> Vec<LineEdit>
```

`LineEdit` is `{ row, kind: Insert(String) | Replace(String) }` — a minimal, ordered edit list. There
is no `Delete`. Every branch below is unit-testable from strings alone, which is the point.

### 8.1 Reading the existing state

Scan the Calendar section for lines matching §5.2's grammar. Each yields
`(row, id, done, time_token, title, trailing)` where `trailing` is everything between the title and
the marker. Lines that do not match are opaque and are never modified or reordered.

### 8.2 The four outcomes

| Case | Action |
| --- | --- |
| Event id not in the note | **Insert** an unchecked line at its sorted-by-start-time position among the synced lines |
| Id present, time differs, title matches what we last wrote | **Replace** the time token only. Checkbox, title, trailing text untouched |
| Id present, title differs from the event | **Leave alone entirely.** The user renamed it; the rename wins (§8.4) |
| Id present in the note, absent from the fetch | **Replace** with the cancelled form (§8.3) |

Sorting applies to inserts only. Existing lines are never reordered — a rescheduled meeting stays
where it sits in the file, because moving it would fight a user who deliberately grouped it.

### 8.3 Cancellation is a mark, not a delete

```
- [ ] 10:00 - 10:30 ~~API Leads meeting~~ (cancelled) <!--gcal:9f2c1ab4e7d0-->
```

The line stays valid and still parses as a timed task, so the panel keeps drawing it, struck through
in the section colour. The user deletes it when they feel like it; a re-created event with the same
id un-marks it. **The syncer never removes a line from the note.** That rule has no exceptions and is
what makes it safe to leave running unattended.

### 8.4 Divergence

Once the title on a line differs from the event's title, the line is the user's. The syncer stops
correcting it — including its time — and records the divergence in `.thock/state/calendar/log.jsonl`
so the reason is inspectable. The alternative (correcting the title back) is exactly the "silently
rewrites what the user wrote" failure the invariants exist to prevent.

Rationale for not diverging on the *checkbox*: ticking a meeting off is the expected use, not an
edit, so a checked line still tracks time changes.

### 8.5 Idempotence

`reconcile(apply(note, reconcile(note, events)), events)` returns an empty edit list, for every input.
This is a property test, not a nice-to-have — the function runs every five minutes forever.

## 9. Applying the edits

Two paths, chosen by whether the note is already open:

- **Open in an editor** — apply as a single buffer transaction via the `MultiBuffer`, so the write is
  undoable with one `u`, coalesces with autosave, and cannot clobber unsaved keystrokes. Selections
  are preserved; the transaction is not grouped with the user's own edit history entry.
- **Not open** — write through the project `Fs`, read-modify-write, after re-reading the file (the
  fetch may have taken seconds).

Two guards:

1. **Typing guard.** If the note's buffer has been edited within the last 2 seconds, defer the edit by
   another 2 seconds, up to 30 seconds, then apply anyway. Nobody wants lines appearing under their
   cursor mid-sentence.
2. **Existence guard.** If today's note does not exist, sync holds. Calendar sync does not create a
   daily note — the user creating their note is the moment the day starts, and a syncer that
   materializes tomorrow's note at 00:01 is a surprise.

Invisible history (V2) covers the write like any other, so every sync is checkpointed and reversible.

## 10. The service

### 10.1 Shape

`CalendarService`, one entity per local project, registered from `thock::calendar::init(cx)` in
`crates/zed/src/main.rs` — the same `observe_new` + `GlobalCalendarServices` map that `history.rs`
uses, and the same single upstream touch-point (one line, adjacent to `thock::history::init`).

It owns: the resolved config, the provider, a single poll `Task`, the ETag map, and the last
outcome. It lives independently of the Day Planner panel, so closing the panel does not stop sync.

### 10.2 The loop

```
every poll_seconds, if connected and a vault is open:
    date  = Local::now().date_naive()
    for each configured calendar:
        GET events.list?timeMin=<local 00:00>&timeMax=<local 24:00>
            &singleEvents=true&orderBy=startTime&timeZone=<local>
            with If-None-Match: <stored etag>
        304 → nothing changed for this calendar
    if every calendar returned 304 → done, zero further work
    else → filter, normalize, reconcile, apply
```

**Why not `syncToken`.** Google's incremental sync token is incompatible with `timeMin`/`timeMax`/
`orderBy` — using it means tracking an unbounded event window and filtering to the day locally, plus
handling `410 Gone` re-syncs. A day is a handful of events; a conditional request that returns `304`
is cheaper than that machinery and cannot drift.

The date is recomputed every tick, so an app left open overnight follows midnight to the new note.

**Backoff:** on transport error, double the interval from `poll_seconds` to a 60-minute ceiling; reset
on the first success. Offline is just an error. `401` → `Disconnected` (§6.4), loop stops.

### 10.3 User-visible surface

A single status row at the top of the Day Planner panel, only when the vault has a Calendar config:

| State | Row |
| --- | --- |
| Never connected | *Connect calendar* — runs `thock::ConnectCalendar` |
| Healthy | `Calendar · synced 2m ago` in muted text |
| Failing | `Calendar · sync failed` + retry, with the error in the tooltip |
| Disconnected | `Calendar · sign-in expired` + reconnect |

Actions, all named so they appear in the command palette with note-taker-facing doc comments:
`thock::ConnectCalendar`, `thock::ChooseCalendars`, `thock::SyncCalendarNow`,
`thock::DisconnectCalendar`.

The row is focusable and reachable by keyboard within the panel's existing selection model.

### 10.4 Timeline Routine wiring

`routines/timeline/routine.toml` (schema 2) gains a skill describing the ritual in plain language,
matching the existing entries:

```toml
[[skill]]
id      = "connect-calendar"
name    = "Connect Calendar"
file    = "routines/timeline/skills/connect-calendar.md"
summary = "Link a Google Calendar so today's meetings appear in today's note."
reads   = ["google:calendar (read-only)"]
writes  = [".thock/calendar.toml", "daily/<today>.md (Calendar section)"]
```

Bumps the routine `version`. The skill explains the flow, the filters, and the marker comment, and
tells the user's agent to run `thock::ConnectCalendar` rather than trying to do OAuth itself.

## 11. Part B — sectioned, coloured Day Planner

### 11.1 The parser change

`PlanItem` gains `pub section: Option<String>`. While walking the planner range, `parse_day_plan`
tracks the most recent heading **at exactly one level deeper than the planner heading**; deeper
headings (`###` under `##`) do not change it, so a subsection and its children share one identity.
Items before any subsection get `None`.

`DayPlan` gains `pub fn sections(&self) -> Vec<&str>` in first-appearance order, for the panel and
for the reconciler's section lookup.

This is the whole change to `day_plan.rs` beyond tests — the parser is otherwise untouched, and every
existing test still passes with `section: None`.

### 11.2 Colours are hashed, not random

A random pick re-rolls on every re-parse, which means blocks change colour as the user types.
Instead:

```rust
fn section_palette_index(name: &str, palette_len: usize) -> usize
```

FNV-1a over the trimmed, lowercased section name, modulo the palette length. `Meetings` is the same
colour today, tomorrow, and after a restart — which is the property that makes the colour *mean*
something. The visible behaviour ("every subsection gets a different colour") is identical.

### 11.3 The palette

`cx.theme().players()`, skipping index 0 (reserved: `players().local()` is the local-user colour and
stays associated with root-level items). Eight theme-defined hues that already ship light and dark
variants and swap with the theme, rather than a hardcoded list that fights every user theme.

- Root-level items keep today's exact appearance: `text_accent` fill and border.
- Sectioned items use `PlayerColor.cursor` at `0.4` alpha for the border and `0.15` for the fill,
  mirroring the current accent treatment so nothing about the visual weight changes.
- Done items keep the existing muted treatment regardless of section — struck through beats coloured.
- Unscheduled chips get the same border colour, so a chip and its section's blocks match.
- `[day_planner.sections]` (§7.3) pins an index; out-of-range values are a logged warning and fall
  back to the hash.

Collision (two sections hashing to the same index) is accepted. With eight colours and typically two
or three subsections it is uncommon, and the override exists for when it bites.

### 11.4 Marker stripping

`PlanItem::label` has a trailing `<!--gcal:…-->` removed during parsing, so the panel, the chips, and
the block captions never show it. Stripping is generic over `<!-- … -->` at end of line, not
gcal-specific — a user's own trailing comment gets the same treatment.

## 12. Implementation notes

New files, all inside `crates/thock/`:

| File | Contents |
| --- | --- |
| `src/calendar.rs` | `CalendarEvent`, `CalendarProvider`, the reconciler (§8), config parsing (§7.1), `sha` id derivation. Pure — no GPUI, no network. |
| `src/calendar_google.rs` | OAuth flow, token refresh, `events.list` / `calendarList.list`, response → `CalendarEvent`. |
| `src/calendar_service.rs` | The GPUI entity, poll loop, edit application (§9), status, actions, picker. |

Changes to existing files: `day_plan.rs` (`section` + `sections()`), `day_planner_panel.rs` (colours,
status row), `thock.rs` (module + `init`), `vault.rs` (`[day_planner.sections]`),
`assets/routines/timeline/routine.toml` + a new skill file.

**Outside `crates/thock/`:** one line in `crates/zed/src/main.rs` (`thock::calendar::init(cx);`) next
to the existing `thock::history::init(cx);`, and `http_client` + `serde_json` added to
`crates/thock/Cargo.toml` (both already workspace members). Nothing else.

Traps worth naming up front:

- **Do not update the entity from inside a workspace update** — the status row reaches the workspace;
  `cx.defer` per the existing panel rules.
- **The poll task must not be cancel-by-replace.** Store one task; replacing it on config reload is
  correct, but the *edit* tasks it spawns must be awaited inside it, not stored in a field.
- Timed events carry `dateTime` + `timeZone`; all-day events carry `date`. Converting the former with
  `chrono::Local` and the latter as a bare date is the entire timezone story — do not hand-roll
  offsets.
- An event spanning midnight is clamped to the day window, matching V4 §5.5's existing clamp.
- `events.list` paginates; follow `nextPageToken` even though a day rarely needs it.
- The ETag is per calendar **and** per query — changing the day window invalidates it. Key the map on
  `(calendar_id, date)` and drop yesterday's on rollover.

**Testing:** the reconciler and the parser are string-in/string-out and get thorough unit coverage
including the idempotence property (§8.5). The Google client is tested against a fake `HttpClient`
with recorded response bodies — no network in CI. The service gets a GPUI test with a stub provider
driving a full sync into a temp vault, using executor timers per the repo's timer rules.

## 13. Open assumptions to confirm on review

1. **Section creation timing** — created on the first sync with events, appended as the last child
   subsection. Alternative: create it during daily-note templating so its position is the template
   author's choice. Templating is cleaner but means every vault grows an empty `## Calendar`.
2. **`tentative` invitations** — currently dropped with `needsAction`. Arguably a tentative meeting
   is exactly the thing you want to see in the planner. Cheap to flip to a filter option.
3. **Divergence on title** — the rename freezes the line's time updates too. The alternative (keep
   correcting time, never touch title) is more useful but harder to explain.
4. **`.thock/calendar.toml` vs `.thock/routines/timeline/calendar.toml`** — the latter is where V7
   says per-Routine state belongs; the former is easier to find and edit. Chose findability.
5. **Colour source** — `players()` is semantically "collaborators". It is the only multi-hue,
   theme-aware palette that ships; a dedicated `section_colors` theme extension would be more honest
   but touches `crates/theme/`.
6. **Poll floor** — 60s minimum. Google's quota is generous and 304s are cheap, but a low floor across
   many calendars is the obvious way to get rate-limited.

## 14. Decision log (from design discussion, 2026-08-18)

| # | Decision | Rejected alternatives and why |
| --- | --- | --- |
| 1 | **Native Google REST** as the V8 transport, behind `CalendarProvider` | *MCP connector*: no standardized tool schema across calendar servers, no conditional requests, and a third-party subprocess in a five-minute loop. Stays the likely second provider. *macOS EventKit*: no credentials at all and covers every account the OS knows, but macOS-only and freshness is the OS's, not ours. *Agent skill on a timer*: burns tokens forever on a deterministic pull and puts non-deterministic writes into a file being edited. *Secret ICS URL*: Google caches it for hours, failing the core requirement. |
| 2 | **`<!--gcal:id-->` markers on synced lines** | *Match on time + title*: a renamed or moved meeting reads as delete + insert, duplicating lines and losing checkmarks. *Sidecar line map*: breaks the moment a hand-editable file is reordered. |
| 3 | **Read-only toward Google in V8** | Two-way sync doubles the feature — conflict resolution, confirmation UX, and a write scope on the token — for a workflow that is mostly consumption. |
| 4 | **Lives in the Timeline Routine**, not its own Routine and not core | A standalone Routine would be independently installable but meaningless without daily notes; core would make calendar sync part of what Thock *is* rather than something you opt into. |
| 5 | **Bundled OAuth client id, overridable** | *BYO client id only*: every user walks Google Cloud Console before the feature works. *Bundled only*: no escape hatch for anyone who wants their own project. |
| 6 | **Calendars picked in the connect flow** | *Primary only with an allowlist*: predictable but means editing TOML to add the team calendar. The picker is a modest amount of UI over the `picker` crate already in use. |
| 7 | **Accepted + solo events only, by default** | Syncing everything turns the planner into an invitation inbox. |
| 8 | **Stable hash for section colours**, not random | Truly random re-rolls on every re-parse, so blocks change colour while typing. The hash looks random and gives colours memory across days. |
| 9 | **Day-window `events.list` + ETag**, not `syncToken` | Google forbids `syncToken` alongside `timeMin`/`timeMax`/`orderBy`; incremental sync would mean tracking an unbounded window plus `410 Gone` recovery to save less than a conditional request already does. |
| 10 | **Never delete a line** | A syncer that removes content from a note it does not own is the single most dangerous thing this feature could do. Cancelled events get marked; the human deletes. |
