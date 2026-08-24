# Thock V13 — The Inbox Routine: capture from anywhere, triage at the desk

**Status:** Ready for implementation — every open assumption closed 2026-08-23 (§12)
**Owner:** Diego · **Date:** 2026-08-23
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.4 Human-in-the-loop, §4.6 Modular life,
§5.7 Inbox), `v9-gmail-backlog-capture.md` (the capture planner, dedup, poll loop, and status-row
grammar this generalizes), `v8-calendar-sync.md` (OAuth and the service shape), `v6-backlog.md` (the
backlog format triage writes into), `v7-dynamic-routines.md` (the `routine.toml` this ships as)

---

## 1. Summary

Everything Thock knows arrives through a keyboard at a desk. Away from it, a thought either survives
until you sit down or it doesn't. V13 gives the vault a **front door**: an `inbox/` folder that
anything can write a note into, and a **Triage Inbox** ritual that proposes a destination for each
item and files only what the user confirms.

The split is the whole idea: **capture is dumb, instant, and thumb-sized; triage is deliberate,
assisted, and at the desk.** Nothing decides anything on the phone.

Three deliverables, each severable:

1. **The landing zone (§4–§6, §10)** — `inbox/` inside the vault, a documented note format, and the
   **Inbox Routine** (`routines/inbox/routine.toml`): the folder scaffold, a **Set Up Inbox**
   interview that writes an editable triage policy into the vault, and the **Triage Inbox** ritual.
   No network, no new scopes; a drag from Finder or a note written by hand already works.
2. **Two mobile transports (§7–§9)** — a Gmail `thock/inbox` label and a Google Tasks list, both
   polled by a single new `InboxService` that reuses V9's machinery wholesale. Read-only toward
   Google, dedup in `.thock/state/inbox/`, one status row in the Backlog panel.
3. **A watched drop directory (§14, deferred)** — an absolute path *outside* the vault that a phone
   fills through iCloud Drive / Syncthing, plus a published iOS Shortcut. Speced here only so the
   landing zone doesn't have to change later.

## 2. Goals & success criteria

- **G1** — A link shared from a phone into Google Tasks, or an email labeled `thock/inbox`, becomes
  a note in `inbox/` within a poll interval, with no decision made on the phone.
- **G2** — `inbox/` is a **worklist, not a pile**: if a file is in it, it is untriaged. Triage empties
  what it files.
- **G3** — Triage never writes without confirmation, and never rewrites what the user wrote. It
  appends tasks, appends to notes, or moves whole files — nothing else. (VISION §4.2, §4.4)
- **G4** — No duplicates, ever. Re-polling, restarting, or re-labeling an already-captured item
  produces nothing new; the same idempotence property tests V9 §8.2 established.
- **G5** — Nothing in the capture path assumes any other Routine exists, and a vault with no
  `.thock/inbox.toml` behaves exactly as today. Gmail's Someday fast lane keeps working through the
  rename (§7.1) with no action from an existing user, and no existing config file is rewritten by the
  `google.toml` consolidation (§7.4).
- **G6** — Triage is **legible after the fact**: for any item that came through the inbox, the vault
  says where it went and when, without Thock running.

**Success:** a week where the phone is used only to fling things at the vault, nothing is retyped at
the desk, and `inbox/` is empty most evenings.

## 3. Non-goals

- **Deciding on the phone.** No priority, no destination, no due date crosses the wire. A Google
  Tasks due date is captured as metadata for triage to read, not as an instruction.
- **Any write toward Google.** Read-only stays read-only (§13 #1). Thock never completes, deletes, or
  re-labels anything in Tasks or Gmail; the phone-side list is the user's to clear.
- **A dedicated Inbox panel.** The queue is a folder and the ritual is a skill. The only new pixels
  are one status row in the Backlog panel (§10.4).
- **Rich capture.** No attachments, no images, no voice notes. Title, optional URL, plain text body.
  (Telegram, the one transport that does voice and photos, is a later source.)
- **Automatic filing.** Triage always proposes and waits, even at high confidence — no policy rule,
  no confidence threshold, and no flag can skip the confirm (§12 #5). Accepting the whole batch is
  one word; that is as fast as it gets.
- **Two-way state.** Once an item is triaged, Thock forgets it existed except for the log line.
- **Non-Google transports.** Telegram, IMAP, Apple Reminders are all plausible and all later; §7's
  seam is where they attach.

## 4. Core concepts

### 4.1 One landing zone, many transports

V9's real product was not Gmail — it was a capture *architecture*. V13 makes that explicit by
inverting it: instead of each source knowing how to write a backlog task, every source produces a
`CapturedItem` and **one** planner turns it into **one** file in `inbox/`. A new transport is a
`fetch` implementation and a config block; it touches nothing about how items land, are deduped, or
are triaged.

### 4.2 The file is the queue

There is no index, no database, no "unread" flag. **A file in `inbox/` is an untriaged item**, and
that is the entire data model. It survives Thock being uninstalled, syncs through any file transport,
is greppable, and is impossible to get out of sync with itself. Triage's job is to make the folder
empty; anything left is either new or deliberately deferred (§9.4).

### 4.3 The vault is the record, state is a cache

Dedup follows V9 §4.3 exactly, one layer at a time:

- `.thock/state/inbox/imported.jsonl` — the working set, one JSON line per captured item.
- `capture:` in every inbox note's frontmatter (§6) and an `<!--inbox:…-->` marker on every line
  of the triage log (§9.5) — the durable, rebuildable record, in the vault.

If the state file is deleted it is **rebuilt** by scanning `inbox/*.md` frontmatter and the triage
log's markers. Deleting state costs a scan, not a duplicate flood. An item whose note *and* log line
are both gone re-captures on the next poll — at that point the user erased every trace, and
re-capture is the correct reading of intent.

Because the sources are read-only (§13 #1), a still-open Google Task and a still-labeled email are
permanent capture candidates. That is the price of not writing to Google, and it is paid entirely by
the state file.

### 4.4 The watermark is for the human, not the machine

Dedup is per item and exact (§4.3); the watermark answers a different question — *what is new since I
last triaged?* `.thock/state/inbox/last-triage` holds one ISO timestamp, written by the ritual when
it finishes. Triage uses it only to sort its proposal into **New** and **Carried over**, so items the
user has already looked at and left alone don't get re-argued every session. Losing the watermark
costs one slightly noisier triage.

### 4.5 Triage is a skill, not a feature

Every destination triage can choose — append a task, append to today's note, move a file — is
something the user's agent can already do with plain file operations. So triage ships as Markdown
instructions (`/triage-inbox`), not Rust. The consequences are deliberate: the user can read exactly
what it will do, edit its judgment, and re-run it after changing its mind. The *policy* it follows is
a vault file too (§9.2), written by an interview at setup and editable forever after.

## 5. What the user does

| On the phone | Lands as |
| --- | --- |
| Share an article to Google Tasks | An inbox note titled with the page title, `url:` set |
| Type an idea into Google Tasks | An inbox note, title-only, body from the task's notes |
| Mail something to yourself, label it `thock/inbox` | An inbox note with the email's text and a link back to the thread |
| Drag a file into `inbox/`, or write one by hand | An inbox note (frontmatter optional) |

At the desk: run **Triage Inbox** (`/triage-inbox`, or `enter` on the Backlog panel's Inbox row).
It shows one line per item with a proposed destination; the user confirms, re-assigns, or defers;
it applies the batch and empties what it filed.

## 6. The inbox note format

`inbox/<YYYY-MM-DD>-<HHmm>-<slug>.md`, timestamped from the item's own moment (the email's date, the
task's creation time) in local time, falling back to capture time. The slug follows V9 §5.3:
lowercase, alphanumerics and dashes, ≤60 chars, `item` when empty. A path collision with a
*different* item appends `-<first 4 hex of the digest>`; a collision with the *same* item (state was
rebuilt mid-flight) leaves the existing file untouched — create-if-missing, like every vault write.

```markdown
---
source:   google-tasks
capture:  4d1f9a02c7b3
captured: 2026-08-23T18:22:07-07:00
title:    Ship it — a practical guide to shipping
url:      https://example.com/ship-it
due:      2026-08-27
---

# Ship it — a practical guide to shipping

https://example.com/ship-it
```

| Field | Meaning |
| --- | --- |
| `source` | `google-tasks` \| `gmail` \| `drop` \| absent (hand-written) |
| `capture` | The dedup digest — first 12 hex of `sha256(account + "\0" + source + "\0" + external_id)`, V9 §5.2's construction |
| `captured` | When Thock wrote the file |
| `title` | Sanitized per V9 §5.1 rule 4: `<!--` stripped, whitespace collapsed, `[[`/`]]` broken apart, `(untitled)` when empty |
| `url` | The thing the item is *about*, when there is one |
| `link` | A back-link into the source (a Gmail thread URL); never a `url:` |
| `due` | Only when the source carried one. Metadata for triage, not a schedule |
| `deferred` | Stamped by triage when the user leaves an item (§9.4) |

The body is the item's text, unaltered and never carrying a marker, so nothing in it can confuse the
machinery (V9 §5.4). A hand-written file with no frontmatter at all is a valid inbox item — triage
reads the first heading or the first line as its title.

## 7. Sources

```rust
pub struct CapturedItem {
    pub source: &'static str,
    pub external_id: String,      // Gmail thread id, Google Tasks task id
    pub title: String,
    pub url: Option<String>,
    pub link: Option<String>,
    pub body: Option<String>,
    pub occurred_at: Option<DateTime<FixedOffset>>,
    pub due: Option<NaiveDate>,
}

pub trait InboxSource: Send + Sync {
    fn id(&self) -> &'static str;
    fn fetch(&self, cx: &AsyncApp) -> Task<Result<Vec<CapturedItem>>>;
}
```

The trait exists now rather than after a third source because the third source (§14) is already
sketched and because two implementations is exactly when V9 §12's "one implementation isn't enough
evidence" stops applying. `MailProvider` stays as-is for the Someday fast lane; Gmail implements both.

### 7.1 Gmail — nested `thock/` labels

Gmail nests labels on `/`, so both Thock gestures collapse under one parent chip instead of scattering
through the label list. V13 moves the defaults there (§12 #1):

| Label | Destination | Rationale |
| --- | --- | --- |
| `thock/backlog` (V9's `backlog`, renamed) | `backlog.md` → Someday, directly | You already know it's a task. Triage would be ceremony. |
| `thock/inbox` (new) | An inbox note | You know it matters; you don't yet know what it is. |

Resolution is V9's, unchanged: `labels.list`, case-insensitive match against the label's **full path
name**, `labelIds` in the query and never an interpolated `q=` string. Nesting costs nothing in the
API — `thock/inbox` is simply a label whose name contains a slash.

**Migration.** `gmail.toml`'s `label` default changes from `backlog` to `thock/backlog`, which would
silently break any vault relying on the default. So label resolution gains one fallback step: if the
configured-or-default label is absent **and** a label named `backlog` exists, use it, and say so in
the status row's tooltip (*using the old `backlog` label — rename it to `thock/backlog` in Gmail*).
The fallback applies only to the V9 label, only when the configured one is missing, and never
silently: it is a visible transitional state, not a permanent alias. A vault with an explicit `label`
in config is unaffected either way.

A thread carrying **both** labels takes the backlog fast lane and is captured once — the digest space
is shared, so it can never land twice. `ImportRecord` in `.thock/state/gmail/imported.jsonl` gains a
`dest` field defaulting to `someday`, so pre-V13 state files load unchanged.

Body extraction, RFC 2047 subject decoding, `Re:`/`Fwd:` stripping, and HTML→text reduction are V9's,
reused verbatim. `link:` is the `https://mail.google.com/mail/u/<account>/#all/<thread_id>` form.

### 7.2 Google Tasks

`https://tasks.googleapis.com/tasks/v1/`, scope `https://www.googleapis.com/auth/tasks.readonly`.

```
GET /users/@me/lists                                   # resolve the configured list by title
GET /lists/{id}/tasks?showCompleted=false&showHidden=false&maxResults=100
```

Only tasks with `status = "needsAction"` are captured. Completing a task on the phone *before* Thock
polls therefore means "never mind" — a genuinely useful gesture that falls out of the read-only
stance. Completing it *after* changes nothing, because the id is already in state.

| Tasks field | Becomes |
| --- | --- |
| `id` | `external_id` (stable per list) |
| `title` | `title` |
| `notes` | `body` |
| `links[0].link`, else the first URL in `notes`, else in `title` | `url` |
| `due` | `due` (date-only; Google ignores the time component) |
| `updated` | `occurred_at` |

No `updatedMin`, no incremental machinery: an uncompleted task list is small, one `list` call per
poll is near-free, and dedup is by id — the same reasoning that rejected `syncToken` in V8 §10.2 and
`historyId` in V9 §10.2. Paginate on `nextPageToken` and stop.

**One configured list, defaulting to the account's default list** (`My Tasks`), because that is where
a mobile share sheet drops things with zero decisions. A list named in config that doesn't exist is
`Holding { ListNotFound }` (§10.4), not an error.

### 7.3 Authentication

`tasks.readonly` joins the existing unified consent (V9 §6.1): one screen, three read-only scopes,
one refresh token at `https://thock.local/google`. Consequences, all with V9 precedent:

- An already-connected user's token lacks the Tasks scope. The first Tasks call returns
  `403 insufficient scope`, which is treated as `Disconnected` with a *Connect Google Workspace*
  affordance — V9 §6.2's exact degradation path. One reconnect upgrades everything.
- The Google Cloud consent screen must list the new scope before any of this works (§11).

### 7.4 `.thock/google.toml` — one account, one override

V9 §12 Q5 deferred this at two config files; a third is where it stops being free, so V13 pays it
(§12 #4). One file holds what belongs to the *connection* rather than to any feature:

```toml
schema  = 1
account = "diego@example.com"   # written by the connect flow

[google]                        # optional client override, formerly duplicated per feature
# client_id     = "..."
# client_secret = "..."
```

`calendar.toml`, `gmail.toml`, and `inbox.toml` keep only their own feature keys — labels, lists,
import mode, poll intervals.

Resolution order, applied identically by all three services: `google.toml`, then the service's own
file, then the other Google files in a fixed order. That last step is the migration — an existing
vault with `account` in `calendar.toml` keeps working untouched, and the next connect writes
`google.toml` and stops writing the duplicates. **Legacy keys are read, never rewritten and never
deleted**: a stale `account` in `calendar.toml` is inert once `google.toml` exists, and stripping
keys out of a user's config file is exactly the kind of unrequested edit the vault rules forbid.

This is a mechanical, no-behavior-change refactor and lands as the first commit of the work, before
anything Tasks-shaped — so if it goes wrong it does so on its own.

## 8. The planner (this is the contract)

Pure, in `inbox.rs`. No I/O, no GPUI, no network:

```rust
pub fn plan_inbox_capture(
    items: &[CapturedItem],
    imported: &HashSet<String>,   // digests from state + rebuild scan
    existing: &[String],          // file stems already in the inbox dir
    config: &InboxConfig,
    captured_at: &str,
) -> InboxPlan

pub struct InboxPlan {
    pub files: Vec<InboxFile>,             // (vault-relative path, full content)
    pub newly_imported: Vec<ImportRecord>,
}
```

| Case | Action |
| --- | --- |
| Digest in `imported` | Skip |
| Digest not in state but present in an inbox note's frontmatter or the triage log | Skip, emit an `ImportRecord` to repair state |
| Fresh item | One file (§6) |
| Fresh item, no body and no url | One file, body `_(no content)_` — never drop a capture silently |
| Path collides with a different item | Suffix with the digest's first 4 hex |

**Idempotence (G4):** `plan_inbox_capture(items, imported ∪ plan.newly_imported, existing ∪ written, …)`
is empty for every input, and so is the weaker form with `imported` unchanged, because the
frontmatter scan catches what the state doesn't. Both are property tests — this is what makes a crash
between "file written" and "state written" safe.

## 9. Triage

### 9.1 The ritual

`/triage-inbox` — `routines/inbox/skills/triage-inbox.md`. Its shape:

1. Read `routines/inbox/triage-policy.md` (§9.2) and `.thock/state/inbox/last-triage` (§4.4).
2. List `inbox/*.md`, newest first, split into **New** and **Carried over**.
3. For each item, propose exactly one destination (§9.3) with a one-line reason. Present the whole
   batch as a compact numbered list — **one screen, not a conversation per item**.
4. Wait. Accept `all`, `1,3,5`, `2 → soon`, `leave 4`, `drop 6`, or prose.
5. Apply the confirmed batch: write destinations first, then remove the source files.
6. Append one line per handled item to the triage log (§9.5), then write the watermark.
7. Report: counts by destination, what was left, what failed.

Nothing is applied before step 4, and a failed write in step 5 leaves that item's file in place — an
item is only ever removed after its destination write succeeded.

### 9.2 The policy file, and the interview that writes it

**Set Up Inbox** (`kind = "setup"`) is a short, direct interview — the user answers six questions
once, and the answers become an editable file rather than hidden behavior (VISION §4.8):

1. Which folders or Routines may triage file notes into? (offer what's actually installed)
2. Where do links-to-read go — a reading-list note, Someday, or a folder?
3. Where do raw ideas go?
4. What does "act on this soon" mean here — Backlog · Soon, or today's note? Which section?
5. Anything that should always be discarded on sight?
6. When unsure, ask — or default to Someday?

The result is `routines/inbox/triage-policy.md`: a plain table of *if it looks like → propose*, plus
the destinations paragraph. The triage skill reads it every run, so editing the file changes the
ritual with no re-setup. A vault with no policy file gets sane defaults and a nudge to run setup.

### 9.3 Destinations

| Destination | What happens to the item file |
| --- | --- |
| Backlog · Soon / Someday | Task line appended under the heading; file **deleted** |
| Today's note, a named section | Line appended (note created if missing); file **deleted** |
| Appended to an existing note (a reading list, a project note) | Appended; file **deleted** |
| Kept as a note | File **moved** and renamed into the destination folder — content preserved verbatim |
| Left | Untouched, stamped `deferred:` (§9.4) |
| Discarded | File **deleted** |

Deletion is the deliberate choice (§13 #4): the content has been absorbed into the vault or
explicitly rejected, `inbox/` is a worklist that must empty to work, invisible history (V2)
checkpoints every triage batch so nothing is actually unrecoverable, and the log line (§9.5) says
where it went. A task line the triage writes carries the item's title, not its whole body — the body
either moved with the file or was the URL.

### 9.4 Leaving something

`deferred: 2026-08-23` in frontmatter, optionally with a one-line reason on the next field. The item
stays in `inbox/` and sorts under **Carried over** next time, without argument. Three deferrals is
information, not a failure — the ritual says so once, quietly, and moves on.

### 9.5 The triage log

`archives/inbox/triage-log.md`, append-only, one line per handled item, newest at the bottom:

```markdown
- 2026-08-23 · Ship it — a practical guide to shipping → Backlog · Someday <!--inbox:4d1f9a02c7b3-->
```

Two jobs. For the human, it answers "what happened to that thing I sent myself." For the machine, its
markers are half the state-rebuild scan (§4.3). It is written by the skill, so a malformed line is
possible; the cost is bounded at one potential re-capture of that item, and the format is stated
exactly in the skill body for that reason.

## 10. Plumbing

### 10.1 The Inbox Routine

`crates/thock/assets/routines/inbox/routine.toml`, schema 2, shipped in the catalog and
**default-installed for new vaults** alongside Timeline (`vault.rs`'s `install_default_routines`):

```toml
schema  = 2
id      = "inbox"
name    = "Inbox"
version = 1
summary = "A front door for the vault — capture anywhere, decide at the desk."
icon    = "envelope"
doc     = "routines/inbox/Inbox.md"

[[link]]
name  = "Triage log"
open  = "archives/inbox/triage-log.md"
group = "History"

[[scaffold]]
kind = "dir"
path = "inbox"

[[skill]]
id      = "triage-inbox"
name    = "Triage Inbox"
file    = "routines/inbox/skills/triage-inbox.md"
summary = "Go through what you captured and file it — you confirm every move."
reads   = ["inbox/**", "routines/inbox/triage-policy.md", "backlog.md", "daily/<today>.md"]
writes  = ["backlog.md (append)", "daily/<today>.md (append)", "archives/inbox/triage-log.md (append)", "inbox/** (move, delete on confirm)"]

[[skill]]
id      = "setup-inbox"
name    = "Set Up Inbox"
kind    = "setup"
file    = "routines/inbox/skills/setup-inbox.md"
summary = "Six questions that teach triage where things go in this vault."
reads   = ["routines/**", ".thock/config.toml", "the vault's folder layout"]
writes  = ["routines/inbox/triage-policy.md", ".thock/state/onboarded/inbox"]

[onboarding]
skill = "routines/inbox/skills/setup-inbox.md"
```

The section is **verbs, not places** (V11's grammar): the rail row runs triage, setup hides behind the
collapsed Setup row, and the triage log sits in a collapsed History group. There is deliberately no
row that opens `inbox/` (§12 #2) — a `[[link]]` opens a file, not a folder, and inventing a
`kind = "folder"` for one row is panel surface the queue doesn't need. The queue is reached by
running triage; browsing raw captures is what the project panel is for.

The Routine is **default-installed in new vaults** alongside Timeline (§12 #3), so the front door
exists from first run and the Backlog panel's status row always has a ritual to point at.

The Rust plumbing does **not** depend on the Routine (VISION §4.6): capture writes to the configured
`dir` whether or not it is installed. What the Routine provides is the ritual, the policy, the docs,
and the rail rows.

### 10.2 Configuration — `.thock/inbox.toml`

A sibling file, not a `config.toml` table, for V8 §7.1's forward-compat reason.

```toml
schema = 1

dir          = "inbox"          # vault-relative landing zone
poll_seconds = 300              # clamped to 60..=3600

[gmail]
enabled = true
label   = "thock/inbox"

[tasks]
enabled = true
# list  = "My Tasks"            # default: the account's default list
```

The account and any client override come from `.thock/google.toml` (§7.4), not from here.

Every field optional with the defaults above. Missing file → the network sources are invisible and
`inbox/` is just a folder (G5). An unparseable file is a logged warning and a disabled service, never
a panic.

### 10.3 The service

`InboxService` in `inbox_service.rs`, one entity per local project, mirroring `GmailService` exactly:
registered from `thock::init`, a `GlobalInboxServices` map, `reload` on `.thock/inbox.toml` /
`.thock/config.toml` worktree events, `SyncState` reused as-is.

```
every poll_seconds, if connected and a vault is open:
    for each enabled source:
        items = source.fetch()                    # errors isolate per source
    plan_inbox_capture → apply
```

Apply order encodes the crash story, V9 §9's:

1. **Files first**, through the project `Fs`, create-if-missing. A crash here leaves inbox notes with
   no state entry — the frontmatter scan repairs it on the next poll.
2. **State last**, appended to `imported.jsonl`.

There is no buffer path and no typing guard: inbox notes are new files nobody has open. Backoff
doubles from `poll_seconds` to the 60-minute ceiling and resets on success; `401` / `invalid_grant` /
`403 insufficient scope` → `Disconnected`, loop stops. Fetching runs on the background executor; only
the apply touches the foreground.

The service also tracks the **queue depth** — a count of `*.md` in `dir`, refreshed on poll and on
worktree events under `dir` — so §10.4 can show it.

### 10.4 The status row

One row in the **Backlog panel**, below V9's Gmail row and only when `.thock/inbox.toml` exists,
in V8 §10.3's grammar:

| State | Row |
| --- | --- |
| Never connected | *Connect Google Workspace* |
| Healthy, empty | `Inbox · empty` |
| Healthy, items waiting | `Inbox · 3 waiting` — `enter` runs the triage ritual |
| List or label missing | `Inbox · list "My Tasks" not found`, hint in the tooltip |
| Running on the legacy label | `Backlog · using the old "backlog" label`, rename hint in the tooltip (§7.1) |
| Failing | `Inbox · sync failed` + retry |
| Disconnected | `Inbox · sign-in expired` + reconnect |

It joins the panel's existing keyboard selection model — reachable, actionable, escapable. `enter`
dispatches the generic `thock::RunSkill { skill: "triage-inbox" }` rather than a bespoke action, per
the repo's rule about dynamic content. The Routine is default-installed (§10.1), but a user who
removed it must not get a dead row: with no `triage-inbox` skill registered, `enter` falls back to
`thock::OpenInbox`, which reveals `dir` in the project panel. New named actions:
`thock::SyncInboxNow` and `thock::OpenInbox`, doc comments written for a note-taker.

The Backlog panel is the right home for the same reason V9 §13 #8 gave: it is the bottom dock the
user already watches, and most of what triage produces lands in it.

## 11. Implementation notes

New files, all inside `crates/thock/`:

| File | Contents |
| --- | --- |
| `src/inbox.rs` | `InboxConfig` parsing, `CapturedItem`, `InboxSource`, `plan_inbox_capture`, digest/slug/sanitize helpers (shared with `gmail.rs`), note rendering, frontmatter and triage-log rebuild scanners. Pure. |
| `src/tasks_google.rs` | Google Tasks REST: list resolution, task paging, URL extraction. |
| `src/inbox_service.rs` | The GPUI entity, poll loop, plan application, state file, queue depth, status, `SyncInboxNow`. |
| `assets/routines/inbox/**` | `routine.toml`, `Inbox.md`, `triage-policy.md` (default), and the two skills. |

Changed: `google_auth.rs` (`.thock/google.toml` §7.4, the third scope, `inbox.toml` written by
connect), `calendar.rs` + `gmail.rs` (account/override resolution moves to §7.4's order),
`gmail.rs` / `gmail_google.rs` (nested-label defaults with the legacy fallback, an inbox-label fetch
yielding `CapturedItem`, `dest` on `ImportRecord`), `backlog_panel.rs` (the status row),
`thock.rs` (modules + init), `vault.rs` (default-install the Routine),
`assets/routines/timeline/routine.toml` (version bump; the connect skill now mentions Tasks and the
renamed label).

**Outside `crates/thock/`:** nothing.

Sequencing: `google.toml` consolidation first, on its own, as a no-behavior-change commit (§7.4).
Then the landing zone and Routine (phase 1), then the two sources (phase 2).

Traps worth naming up front:

- All of V8/V9's: no entity updates inside workspace updates (`cx.defer`), the poll task is stored
  but its inner apply futures are awaited not stored, worktree-event reload matching, and GPUI
  executor timers in tests.
- **The consent screen must list `tasks.readonly` before any of this works.** A scope the Cloud
  project doesn't declare fails at consent, not at call time, which reads as a broken connect flow.
- Google Tasks `due` is RFC 3339 but the time component is meaningless — parse the date, discard the
  rest, or every due date drifts by a timezone.
- `links` on a task is documented read-only and is frequently absent; the URL usually arrives inside
  `notes`. Extract in the §7.2 order and never assume.
- The rebuild scan (§4.3) runs once per reload, on the background executor, before the first poll —
  the first poll after a state wipe must not race it.
- The queue-depth watcher must not count non-`.md` files or dotfiles, and must survive the folder not
  existing (VISION §4.6: missing content is an empty state, not an error).
- Two poll loops now hit Gmail (`thock/backlog`, `thock/inbox`). That is deliberate — one extra
  `messages.list` per interval buys complete separation between a shipped, tested path and a new one.
- A nested label's `labels.list` entry carries the **full path** as its name (`thock/inbox`), while
  its `id` is opaque. Match on the full path; a match on the last segment would collide with any
  other `*/inbox` label the user keeps.
- The legacy-label fallback (§7.1) must not fire when the configured label merely *failed to fetch* —
  only when `labels.list` succeeded and the label genuinely isn't there. Otherwise a transient error
  silently reroutes capture.
- `google.toml` resolution runs in three services; put it in one function in `google_auth.rs` and
  call it, or the fallback order will drift between them.

**Testing:** `plan_inbox_capture` and every helper are string-in/string-out with thorough unit
coverage, including both idempotence properties (§8), collision suffixes, and the sanitization corners
V9 established (marker forgery, wikilink forgery, RFC 2047, empty titles). `tasks_google.rs` runs
against a fake `HttpClient` with recorded fixtures, including a task whose URL is only in `notes` and
a paginated list. The service gets a GPUI test with a stub `InboxSource` driving a full capture into
a temp vault, including the crash-between-writes replay and the state-rebuild-from-frontmatter path.
Account resolution (§7.4) gets its own table-driven test over the migration matrix: `google.toml`
only, legacy only, both, neither, and conflicting values. Label resolution gets one for the legacy
fallback, including the transient-error case that must *not* fall back. The two skills are prose and
are verified by running them against the dogfood vault.

## 12. Spec-close decisions (2026-08-23)

The six assumptions this spec opened with, resolved. Nothing here blocks implementation.

| # | Decision | Rejected, and why |
| --- | --- | --- |
| 1 | **Nested labels: `thock/inbox` and `thock/backlog`**, with V9's flat `backlog` honored as a visible transitional fallback (§7.1) | *Keep `thock-inbox` flat*: no migration, but two unrelated-looking Thock labels loose in the label list forever. *Just `inbox`*: shortest to find on a phone, but Gmail reserves the system name and a user label called `inbox` reads as a bug. The migration is one fallback branch and one tooltip. |
| 2 | **The rail row runs triage; no row opens the folder** (§10.1) | *An `inbox/README.md` explainer to link at*: a row that opens prose instead of the queue, plus a scaffolded file whose only job is to be a link target. *Teach `[[link]]` a `kind = "folder"`*: reusable and honest, but new link-kind plumbing in `routines.rs` and the panel for a single row. The project panel already browses folders. |
| 3 | **Default-installed in new vaults**, alongside Timeline (§10.1) | *Offer it in Add Routine*: purer "modular life", but capture could land files with no ritual to process them, and the status row would have to degrade on a fresh install. *Install on first Google connect*: a Routine that materializes on its own contradicts V7's explicit-activation rule. |
| 4 | **`.thock/google.toml` now, inside V13** (§7.4) | *Defer again*: zero migration risk in a release that already adds a scope — but the bill grows with every source, and the connect flow would keep writing a third duplicate. *A separate pre-V13 refactor*: the same work in a separate PR; done here as the first, isolated commit instead. |
| 5 | **Triage never auto-files** (§3) | *Policy-authorized auto-filing for some classes*: faster, opt-in, and it turns the confirm gate from a guarantee into a setting. *A `--yes` batch escape*: same objection, wearing a keyboard shortcut. Accepting the batch is already one word. |
| 6 | **The skill writes the triage log** (§9.5) | *A `thock::LogTriage` action*: guaranteed format and a trustworthy rebuild scan, but it plants a Thock-only step inside a ritual that is otherwise portable file work (§4.5). *No log at all*: loses the rebuild path and the "where did that go" record — the log's two reasons for existing. |

## 13. Decision log (from design discussion, 2026-08-23)

| # | Decision | Rejected alternatives and why |
| --- | --- | --- |
| 1 | **Read-only toward Google Tasks; dedup in `.thock/state/`** (Diego's call) | *Mark captured tasks completed* (`tasks` write scope): self-emptying list, near-free dedup, and a clean phone — but it puts the first write scope on a token that has only ever read, and V8/V9 set the read-only precedent twice. The cost, accepted knowingly, is that the Tasks list is the user's to clear by hand. *Delete the task*: destroys data in a service Thock doesn't own; one capture bug loses the item forever. |
| 2 | **Two Gmail labels, side by side** — since closed as nested siblings, §12 #1 (Diego's call) | *One label, everything through the inbox*: conceptually cleaner front door, but re-plumbs shipped V9 behavior and adds a triage step to mail the user already classified. *A label → destination map in config*: maximum flexibility, maximum config surface, for a table that would have two rows. |
| 3 | **One configured Tasks list, defaulting to the account's default** (Diego's call) | *A dedicated "Thock" list*: clean separation, but the mobile share sheet drops into the default list, so every capture would need a list pick — friction exactly where friction is fatal. *All lists*: hijacks lists the user keeps for other purposes. |
| 4 | **Triage deletes the file once it files the item** (Diego's call) | *Move to `archives/inbox/`*: never destructive, but leaves a folder of duplicated content whose only job is to be ignored. *Leave in place, stamp `triaged:`*: turns the worklist into a pile the user must visually filter — the failure mode the inbox exists to prevent. Invisible history (V2) plus the triage log (§9.5) make deletion recoverable and legible, which is what makes it safe. |
| 5 | **One `InboxService` over N sources, not one service per source** | *A service per transport* (V9's shape repeated): two status rows, two state files, two rebuild scans, for one landing zone. The seam that matters is the destination, not the transport. |
| 6 | **`InboxSource` trait extracted now** | V9 §12's "one implementation isn't enough evidence" held at one source; V13 ships two and sketches a third (§14). Waiting longer means writing the second implementation twice. |
| 7 | **Triage ships as a skill, policy ships as a vault file** | *A triage panel with keyboard filing*: faster for twenty items and genuinely tempting, but it hard-codes judgment that varies per vault, and every destination it offers is something the agent can already do. If the ritual proves too slow in daily use, the panel is a follow-up with the file format already settled. |
| 8 | **The phone decides nothing** | *Map a Tasks list named `Soon` to Backlog · Soon*: tempting and cheap, and the first crack in the split that makes this design work. A due date rides along as metadata precisely so triage, not the phone, can use it. |

## 14. Deferred: the watched drop directory

The transport that needs no account at all: an absolute path **outside** the vault — inside iCloud
Drive, Syncthing, or Dropbox — that Thock watches and *moves* files out of into `inbox/`.

It is deferred, not rejected, and the reason it must stay outside the vault is worth recording now:
putting the vault itself in iCloud Drive collides with invisible history. A `.git` directory in
iCloud is a known corruption risk (thousands of small objects, no atomicity across the sync
boundary), and iCloud's file eviction turns real files into placeholders, which breaks the fs
watching every Thock panel depends on. Syncing only a drop folder means git never sees synced files
and two devices never write the same file — the conflict class disappears rather than getting
handled.

When it lands: `drop_dir` in `inbox.toml` (absolute, outside the vault, refused otherwise), a
`drop`-sourced `CapturedItem` per file, a move rather than a copy, and a published iOS Shortcut that
appends to a file there from the share sheet. Nothing in §6 or §8 changes.
