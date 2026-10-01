# Thock V31 — Readwise sync: your highlights, landed as notes

**Status:** Implemented (2026-10-01)
**Owner:** Diego · **Date:** 2026-10-01
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.2 Augmentation, §4.6 Modular life),
`v15-unified-gmail-sync.md` (the "map routes, folders mean" config shape and crash-safe apply order
this reuses), `v13-inbox-routine.md` (the "vault is the record, state is a cache" posture),
`v7-dynamic-routines.md` (the Routine format the Reading Routine ships in)

---

## 1. Summary

Readwise collects highlights from Kindle, Reader, podcasts (Snipd) and the web. The vault used to
receive them through Obsidian's Readwise plugin, which left 163 notes under `reference/readwise/`
(books, articles, podcasts) in the plugin's default template. Since the move to Thock nothing syncs
them anymore.

V31 adds a **Readwise sync service**: a background poll of Readwise's export API that lands one
Markdown note per source (book, article, podcast) and appends new highlights to it as they appear.
It reuses V15's shape: a category → folder map in `.thock/readwise.toml`, one service, one
state cache, a crash-safe apply order, and a status row. It ships inside a new, opt-in **Reading
Routine**, whose Connect Readwise skill gets the token entered and explains what the user will see.

The Routine also teaches the agent about the data. A **Reading Week** step joins the Week Review, so
the weekly review gains a `### Reading` section and Reflect folds what the user read, and the
quotes they kept, into `memory/` (§10.2). `AGENTS.md` names the highlights as a source about the
person, so every session knows they are there (§10.3).

```toml
# .thock/readwise.toml
schema = 1

[[sync]]
category = "books"
path     = "reference/readwise/books"
```

The old plugin notes are **not adopted**: they carry no Readwise ids, so matching them would rely
on titles and highlight text. They are moved out of the vault by hand, and a full resync gives
every note a stable identity from day one (§9).

## 2. Goals & success criteria

- Connecting is one paste: the user copies a token from `readwise.io/access_token` into a Thock
  prompt. The token is validated before it is stored and never touches the vault.
- A new Kindle highlight shows up in the book's note within one poll, with nothing else in the note
  changed.
- The first sync after connecting lands the user's full library for the mapped categories.
- Adding a category is a two-line config edit, with no code change.
- Sync is read-only toward Readwise and append-only toward the vault: Thock never edits or deletes a
  line it already wrote, and never recreates something the user deleted.
- A note stays readable in any Markdown editor: the body keeps the plugin's familiar template, and
  identity lives in frontmatter and invisible trailing comments.

## 3. Non-goals

- **Reader's queue.** Documents in Reader's Later / Shortlist / Archive (API v3) are out of scope.
  Reader is no longer in use. A later spec can add it as another source behind the same service.
- **Writing back to Readwise.** No creating highlights, tagging, or marking anything. Read-only.
- **Mirroring edits and deletions.** A highlight edited or deleted in Readwise after landing stays
  as it was in the vault. A note added to a highlight *after* it landed doesn't appear either (§12 #4).
- **Adopting the plugin's notes in place.** Decided against; see §9 and §12 #1.
- **Live querying through MCP.** The official Readwise MCP server (`mcp2.readwise.io/mcp`) lets the
  agent search highlights live. It complements this spec, since the agent can already read the
  landed notes, and is deferred (§13).
- **More reading rituals.** Resurfacing a highlight into the daily note, book-review skills, and
  so on. The Week Review step (§10.2) is the only ritual in scope (§13).
- **True reading progress.** Readwise's export carries no percent-read or "finished" flag for
  Kindle books. Progress here means *activity*: which books got highlights in a week, and how far
  into them (§10.2).

## 4. Core concepts

### 4.1 The map is the config

`[[sync]]` entries map a Readwise **category** (`books`, `articles`, `tweets`, `podcasts`,
`supplementals`) to a vault folder. Unmapped categories are skipped. The shipped default maps
`books` only, which is the current need. A category can map to just one folder; a second
entry for the same category is a config error, reported in the status row.

### 4.2 One note per source, highlights appended

Each Readwise source (`user_book_id`) becomes one note in its category's folder. The first time a
source is seen, its note is created with every highlight. After that, the only write Thock makes to
the note is **inserting new highlight lines at the end of its `## Highlights` section**. Everything
else in the note (the user's own sections, comments, rewording) belongs to the user.

### 4.3 The vault is the record, state is a cache

Identity is written into the note:

- frontmatter `readwise_id: <user_book_id>` identifies the source, and
- each highlight line ends with `<!--rw:<highlight_id>@<YYYY-MM-DD>-->`, the same invisible-marker
  convention as `<!--gmail:…-->` and `<!--gcal:…-->`, which the conceal machinery already hides. The
  date is `highlighted_at` (falling back to `created_at`) in the vault's local time zone. It's what
  lets a ritual ask "what did I highlight this week?" from the note alone (§10.2).

`.thock/state/readwise/` caches what has landed (`landed.jsonl`: highlight id, book id, note path,
timestamp) plus the export watermark (`cursor.json`). If the state is lost, it is rebuilt by
scanning the mapped folders for `readwise_id` frontmatter and `rw:` markers, the same recovery V13
and V15 use. The cache's job is to remember deletions: a highlight in `landed.jsonl` whose line is
gone from the note was removed by the user and is never appended again.

### 4.4 The watermark is server time

Readwise's export takes `updatedAfter`. The watermark is set to the time the *previous successful*
sync **started**, minus a five-minute margin for clock skew, and is advanced only after the whole
apply pass succeeds. Overlap between windows is harmless because landing is idempotent on
highlight ids.

## 5. What the user does

1. Install the **Reading** Routine from the Add Routine catalog.
2. Run **Connect Readwise** (skill or `thock: connect readwise`). Thock opens
   `readwise.io/access_token` in the browser and shows a prompt; the user pastes the token and
   presses `enter`. Thock validates it (`GET /api/v2/auth/` → `204`), stores it in the system
   keychain, and writes `.thock/readwise.toml` with the defaults if it doesn't exist.
3. Within a minute, `reference/readwise/books/` fills with one note per book. From then on, a
   highlight made on the Kindle shows up in its book's note within one poll.
4. (Optional) Add `[[sync]] category = "podcasts" path = "reference/readwise/podcasts"` to sync
   podcast highlights too.

## 6. The note format

```markdown
---
source: readwise
readwise_id: 28374651
category: books
---
# A Fé Na Era Do Ceticismo

![rw-book-cover](https://m.media-amazon.com/images/I/91kqjaTsjHL._SY160.jpg)

## Metadata
- Author: [[Timothy Keller]]
- Full Title: A Fé Na Era Do Ceticismo
- Category: #books
- Tags: #faith #apologetics

## Highlights
- “Qual é seu maior problema em relação ao cristianismo?” ([Location 426](https://readwise.io/to_kindle?action=open&asin=B06XTSG7LR&location=426)) <!--rw:512340987@2026-09-28-->
    - Note: use
```

- The body follows the plugin's default template on purpose: the user's reading habits and any
  agent prompts written against the old notes keep working.
- Frontmatter carries only what the machine needs (`source`, `readwise_id`, `category`). Metadata the
  user reads stays in the body.
- `Author` is a `[[wikilink]]` (as the plugin did), so authors work as hubs in the vault's graph.
  Multiple authors (`"A, B and C"`) stay a single link because Readwise sends one string; splitting it
  would guess wrong.
- `Tags` lists `book_tags`, as `#tag`, and is omitted when empty. Highlight tags are written as a
  `- Tags: #a #b` sub-bullet, after `- Note:`.
- Location links follow `location_type`: Kindle `location` → `[Location N](to_kindle…)`, `page` →
  `Page N`, `time_offset` → the source URL with a timestamp label, `order`/`offset`/`none` →
  `[View Highlight](readwise_url)`.
- Highlights are ordered by `location`, then `highlighted_at`, both when the note is created and
  within each batch appended later. A late highlight from early in the book still goes at the end,
  because inserting it in the middle would mean editing among the user's lines.
- File name: the sanitized `title` (V13's `sanitize_title`), with V13's collision suffix
  (`Title (2).md`) when two sources share a title. A source renamed in Readwise keeps its file;
  identity is the `readwise_id`, not the name.
- Discarded highlights (`is_discard`) and deleted ones (`is_deleted`) are never landed.

## 7. Configuration — `.thock/readwise.toml`

```toml
schema = 1

# Poll cadence. Clamped to [15, 1440] minutes. Highlights trickle in, so hourly is plenty.
# poll_minutes = 60

# One entry per category to sync. Omitting every [[sync]] entry ships the default below.
[[sync]]
category = "books"
path     = "reference/readwise/books"
```

The file's existence turns the feature on, the same rule as `gmail.toml` (no config → the service
is dormant and the row is hidden). The connect action writes it; deleting it disconnects in effect
but leaves the keychain entry, which `thock: disconnect readwise` removes.

## 8. Architecture

```
readwise.rs           config parse, export DTOs, planner (pure), note rendering, vault scan
readwise_service.rs   GPUI entity: poll loop, transport, apply, state, status, actions, token prompt
assets/routines/reading/**   routine.toml, doc.md, skills/connect-readwise.md
```

The transport is small enough to live in the service (one endpoint, one header), so there's no
separate `readwise_api.rs`.

### 8.1 Fetch

```
GET https://readwise.io/api/v2/export/?updatedAfter=<watermark>[&pageCursor=…]
Authorization: Token <token>
```

The fetch pages until `nextPageCursor` is null. The first sync omits `updatedAfter`, which is the
full export. Each result is a source with its highlights nested inside, and the planner drops any
source whose `category` isn't mapped. Fetching runs on the background executor.

Errors: `401` → `Disconnected`, loop stops. `429` → wait `Retry-After` and continue the same
page (the export endpoint is limited to 20 requests per minute). Network/`5xx` → backoff doubling
from `poll_minutes` to a 6-hour ceiling, reset on success. A failed page aborts the pass *before*
any apply, so the watermark never skips data.

### 8.2 Plan (pure)

`plan_readwise_sync(sources, mappings, vault_scan, landed) -> Vec<NoteChange>`, where each change is
`Create { path, contents }` or `Append { path, lines }`:

- The source's note is found by `readwise_id` in the scan. If there's none, and the source has
  never landed (no `landed.jsonl` row for its book id), the plan is `Create`. A source that landed
  before but whose note is gone was deleted by the user, so only its *new* highlights are landed,
  into a freshly created note.
- Highlights whose id is in `landed` or already marked in the note are skipped.
- A note whose `## Highlights` heading can't be found (the user renamed or removed it) gets new
  highlights appended under a recreated `## Highlights` at the end of the file. The status row
  doesn't hold for this; landing is the job.

### 8.3 Apply, crash-safe

V15 §7.2's order:

1. **Notes**, through the project `Fs`. Creates use `atomic_write` and are create-if-missing.
   Appends insert after the last line of the `## Highlights` section. When the note is open in a
   buffer, the insert goes through the buffer (the backlog hook's buffer-vs-fs path) so an unsaved
   edit isn't clobbered.
2. **State**: `landed.jsonl` rows for every landed highlight.
3. **Watermark**: `cursor.json` advanced to this pass's start minus the skew margin.

A crash before (2) re-plans into a state repair through the vault scan. A crash before (3) refetches
an overlapping window, which landing ignores.

### 8.4 Token prompt and keychain

`thock::ConnectReadwise` opens the token page in the system browser and a small `ModalView` with a
single masked input. `enter` validates and stores; `escape` cancels. The token is stored through
`zed_credentials_provider` under `https://readwise.io` (account `readwise`), the same mechanism as
`google_auth.rs`. On a validation failure the modal stays open and says why ("Readwise didn't accept
that token").

### 8.5 Status row and actions

The Readwise row joins the **Backlog panel**'s connector rows (Gmail, Inbox), in V8 §10.3's grammar
and the panel's keyboard selection model. The Backlog panel is the bottom dock where connector
health already lives, and giving one connector its own surface would scatter it.

| State | Row |
| --- | --- |
| No `readwise.toml` | hidden |
| Config, no token | *Connect Readwise* — `enter` runs `thock::ConnectReadwise` |
| First sync running | `Readwise · importing your library…` |
| Healthy | `Readwise · synced 12m ago` (`+4 highlights` for one poll after a landing) |
| Config error | `Readwise · two entries for "books"`, detail in the tooltip |
| Failing | `Readwise · sync failed` + retry |
| Disconnected | `Readwise · token rejected` + reconnect |

New named actions, with doc comments written for a note-taker: `thock::ConnectReadwise`,
`thock::DisconnectReadwise`, `thock::SyncReadwiseNow`.

## 9. Migration — out of scope, done by hand

The plugin's notes carry no source ids, and some Kindle highlights carry no highlight ids either, so
adopting them would mean fuzzy title and text matching. A vault holding old plugin notes is a
one-off, not a state the product should plan for, so V31 ships no migration: no detection, no
skill step, no code.

For the dogfood vault (`~/Thock`), `reference/readwise/` is moved **out of the vault** by hand
before the Reading Routine is installed. The first sync then lands the full library fresh, and no
old note shares a stem with a new one, so `[[Book Title]]` links stay unambiguous.

If a vault does still hold an older note at a path the sync wants, the §6 collision suffix applies
(`Title (2).md`). The old file is never touched.

## 10. The Reading Routine

A catalog Routine (not default-installed, like Lifestyle), registered in `routines.rs`:

```toml
schema  = 2
id      = "reading"
name    = "Reading"
version = 1
summary = "Your book highlights from Readwise, kept as notes you own."
icon    = "book"
doc     = "routines/reading/Reading.md"

[[scaffold]]
kind = "dir"
path = "reference/readwise/books"

[[skill]]
id      = "connect-readwise"
name    = "Connect Readwise"
kind    = "setup"
file    = "routines/reading/skills/connect-readwise.md"
summary = "Paste a Readwise token once; your highlights land as notes from then on."
reads   = [".thock/readwise.toml"]
writes  = [".thock/readwise.toml"]

[onboarding]
skill = "routines/reading/skills/connect-readwise.md"
```

The skill follows `connect-google-workspace.md`'s pattern: "do not handle the token yourself", ask
the user to run `thock: connect readwise`, explain the format and the append-only rules when asked.
`doc.md` explains the map and the deletion rules (§4.3) in plain words.

The scaffolded folder matches the shipped default mapping. A user who remaps it gets the old
folder left empty, which is harmless.

The manifest also registers the Reading Week skill (§10.2):

```toml
[[skill]]
id      = "reading-week"
name    = "Reading Week"
file    = "routines/reading/skills/reading-week.md"
model   = "fast"
summary = "What you read this week and the passages worth keeping. Week Review runs it for you."
reads   = ["<mapped folders>/**", "profile.md", "memory/index.md", "memory/reading.md"]
writes  = ["weekly/<week>.md (append)", "memory/inbox.md (append)"]
```

### 10.1 Where the data lives, for skills

A skill never hard-codes `reference/readwise/books`. It finds the synced notes by their
`source: readwise` frontmatter, starting from the folders `.thock/readwise.toml` maps (read-only;
AGENTS.md rule 3 forbids writing there).

### 10.2 Reading in the Week Review

**The Reading Week skill** (`routines/reading/skills/reading-week.md`) works only from the vault and
makes no API calls. For a Monday–Sunday window it:

1. **Collects the week's highlights.** Every synced note with an `rw:` marker dated inside the
   window.
2. **Derives progress per book.** New highlights this week, the location or page range they span,
   and *started* when the book's first-ever highlight falls inside the window. Books with no
   activity aren't mentioned; there is no "finished" (§3).
3. **Picks up to three quotes worth keeping**, preferring highlights the user annotated with a
   `- Note:` (their own words), then the longest unannotated ones. Each keeps its book, author, and
   a `[[wikilink]]` to the note.
4. **Writes a review block:**

   ```
   ### Reading
   - **A Fé Na Era Do Ceticismo** (Timothy Keller): 6 highlights, locations 1074–1787 [[A Fé Na Era Do Ceticismo]]
   - Started **The Psychology of Money** (Morgan Housel): 2 highlights [[The Psychology of Money]]

   > “Toda boa dádiva e todo dom perfeito vêm do alto…” (Keller). Your note: *graça comum*
   ```

5. **Notes durable facts for memory** by appending dated lines to `memory/inbox.md`, the only
   memory write AGENTS.md rule 6 allows outside Reflect:

   ```
   - 2026-10-05 · Reading *A Fé Na Era Do Ceticismo* (Timothy Keller), active this week
   - 2026-10-05 · Kept a quote from *A Fé…* (Keller): “Toda boa dádiva…”; their note: graça comum
   ```

   Reflect, which already runs as Week Review's last step, files those lines into a
   `memory/reading.md` page (§10.4).

**Two modes**, the same split Reflect uses:

- **From Week Review.** It's handed the window, returns the `### Reading` block for the review to
  include, and writes the inbox lines. The weekly note gets one append, from the review.
- **Standalone.** The window is last week. The skill appends its own `# Reading Week` section to
  that weekly note (create-if-missing), then writes the inbox lines and tells the user to run Reflect
  or let the next Wrap Today pick them up.

A week with no highlights produces nothing: no heading, no inbox lines, no comment.

**The change to Week Review** (Timeline Routine, `skills/week-review.md`) is a single conditional
paragraph in its §7, the same "skip in silence if the file is missing" pattern its §9 uses for
Reflect:

> When `routines/reading/skills/reading-week.md` exists, read it and run it now, from Week Review,
> for the review's week. Put the `### Reading` block it hands back after the area sections (and
> after Pull & Merge Requests, when present). Skip this in silence when the file is missing or the
> block comes back empty.

Timeline doesn't depend on Reading and Reading doesn't depend on Timeline (VISION §4.6: modular
life). The Timeline Routine's `version` bumps so V29's update reconcile delivers the new
`week-review.md` to vaults that haven't edited it. The dashboard (`data.js`) doesn't change; reading
on the dashboard is deferred (§13).

### 10.3 Telling every session: `AGENTS.md`

The shipped core `AGENTS.md` (`crates/thock/assets/AGENTS.md`) gains one entry under **The map**,
phrased like the existing `inbox/` entry, so it's harmless in vaults without the Routine:

> - Readwise notes (when the Reading Routine is installed): one note per book, with
>   `source: readwise` frontmatter, under the folders `.thock/readwise.toml` maps
>   (`reference/readwise/books/` by default). They hold what this person reads and the passages
>   they chose to keep, with their own `- Note:` lines, which are often the best signal of what they
>   care about. Thock lands them; treat them as read-only, and add your thoughts in a section below
>   rather than editing a highlight.

V29's update reconcile carries the new `AGENTS.md` to vaults whose copy is unedited, like any core
file.

### 10.4 Highlights are a source about the person

Reflect's first ground rule, echoed by AGENTS.md rule 6, says an *imported item* may be noted as
arriving but its assertions never recorded. Highlights are imported, yet *choosing* a passage is
the person's own act. Without a carve-out, Reflect would rightly refuse Reading Week's inbox
lines. Both files gain the same narrow exception:

- **Reflect** (`skills/reflect.md`), after the "Only the person's own words" rule:
  > Highlights the person kept (notes with `source: readwise`) are their choices. You may record
  > *that* they read something and *which* passages they kept, quoted and attributed to the author,
  > along with their own note on it. Never restate a quote as something true about them or the
  > world.
- **Reflect's page list** gains `reading.md ← books in progress and quotes they kept`, held to the
  usual forty-line cap by pruning the oldest unannotated quotes first. Its index pointer goes under
  **Threads that span weeks** (`- **Reading**: one line on what's in progress. → reading.md`).
  Reflect creates no new index heading.
- **AGENTS.md rule 6**'s "never record what … an imported item asserted" gains "(highlights the
  person kept are the exception; see `skills/thock/reflect.md`)".

`profile.md`'s **What Thock should not keep** still wins. A topic listed there is skipped in the
review block *and* the memory lines.

### 10.5 Books and Authors in the Routines rail

The Reading section lists the synced books itself, so a book is a keypress away without the
file tree:

```
Reading
  > Books
      A Fé Na Era Do Ceticismo
  > Authors
    > Timothy Keller
        A Fé Na Era Do Ceticismo
```

This is a generic Routine format addition, **`[[collection]]`**, not Reading-specific code:

```toml
[[collection]]
name = "Books"
path = "reference/readwise/books"

[[collection]]
name     = "Authors"
path     = "reference/readwise/books"
group_by = "author"
```

- A collection lists every `*.md` directly inside `path` (not recursive), titled by the note's
  first `# ` heading (the file stem when there is none), sorted case-insensitively.
- `group_by` nests the notes one level deeper by a field: frontmatter `author:` first, else the
  first `- Author: …` list line, so Readwise's `## Metadata` block works without changing the note
  format. A `[[wikilink]]` value groups by its target. Notes without the field go last, under
  "No author".
- Collections render after the link groups and count as places for the Notes/Rituals captions.
  They start collapsed, like every group, and an empty collection takes no row.
- Keyboard: the panel's existing model. `enter` opens a book or toggles a group, `right`/`left`
  open and close, and `left` on a closed author closes Authors. A book is bindable through the
  generic `thock::OpenLink` as `{ "routine": "reading", "link": "books/<file stem>" }`.
- The rail reloads a collection on worktree events under its folder, through the project `Fs`, so
  a newly synced book shows up without a restart.
- `path` is literal: a user who remaps `books` in `.thock/readwise.toml` changes it here as well.
  The manifest comment says so.

The Reading Routine's `version` bumps to 2 for the new manifest.

## 11. Implementation notes

New files, all inside `crates/thock/`: `src/readwise.rs`, `src/readwise_service.rs`,
`assets/routines/reading/{routine.toml,doc.md,skills/connect-readwise.md}`.

Also new: `assets/routines/reading/skills/reading-week.md`.

Changed: `thock.rs` (modules + init), `routines.rs` (catalog registration), `backlog_panel.rs`
(the row), `inbox.rs` (only if `sanitize_title` / collision helpers need widening for reuse),
`assets/routines/timeline/skills/week-review.md` + `routine.toml` (the §10.2 paragraph and a version
bump), `assets/skills/reflect.md` and `assets/AGENTS.md` (§10.3, §10.4), and for §10.5
`routines.rs` (`[[collection]]` parse/render), `routines_panel.rs` (collection and nested-group
rows), a new `routine_collections.rs` (scan, titles, grouping), and `assets/routines/ROUTINES.md`.

**Outside `crates/thock/`:** nothing. No keymap entries are needed; the actions are palette-reachable
and the row uses the panel's existing `menu::Confirm`.

Tests:

- `readwise.rs` (pure): config parse and validation (duplicate category, unknown category,
  clamping); rendering per `location_type`; marker dates (`highlighted_at`, `created_at`
  fallback, local-time day boundary); tags/notes sub-bullets; ordering; filename collisions;
  planner cases: new source, new highlights, already-marked, landed-then-deleted line not re-added,
  landed-then-deleted note, missing `## Highlights`, unmapped category, discarded/deleted highlights.
- `readwise_service.rs` (GPUI, fake `Fs`, fake HTTP client): first full sync across multiple pages;
  incremental sync appends without touching other lines; `429` honors `Retry-After`; `401` →
  `Disconnected`; a mid-pagination failure leaves the watermark unmoved; state loss repaired by the
  vault scan; append into an open, dirty buffer. Use GPUI executor timers.

Traps to name up front: all of V8/V15's (no entity updates inside workspace updates — `cx.defer`;
the poll task is stored, its apply futures awaited; worktree-event reload on `readwise.toml`;
executor timers in tests), plus: the token must never be logged, including in error contexts that
format the request.

## 12. Decision log (2026-10-01)

1. **Resync over adopt-in-place, migration by hand.** The plugin's notes have no ids, and
   title/text matching is fragile. Old plugin notes are a one-off, so they're moved out of the
   vault by hand rather than handled by the Routine. That also avoids stem collisions with the
   fresh notes.
2. **Highlights only, no Reader queue.** Reader isn't in use; the export endpoint covers everything
   that matters today.
3. **Books by default, map for the rest.** V15's map shape: adding articles or podcasts is
   config, not code.
4. **Append-only, never mirror.** Edits, deletions, and late notes in Readwise don't reach landed
   lines. That follows from VISION §4.2 and makes a "Thock rewrote my note" bug impossible. Revisit
   only if late notes turn out to matter in practice.
5. **Rust service over an agent skill.** Sync must be periodic, deterministic, and cost no tokens.
   Skills build *on* the landed notes; they don't do the landing.
6. **Plugin template preserved** in the body, with identity in frontmatter and trailing markers, so
   the notes read the same as the ones the user already knows.
7. **Status row in the Backlog panel**, alongside the other connectors, rather than a new surface.
8. **Reading joins the Week Review as a conditional step**, not a Timeline dependency. Week Review
   runs Reading Week when its file exists, the same pattern it uses for Reflect.
9. **Memory goes through `memory/inbox.md` and Reflect.** Reading Week never writes memory pages
   itself, so Reflect stays the one writer and its rules (dates, sources, the never-keep list,
   deleted lines stay deleted) apply to reading facts as well.
10. **A narrow carve-out for highlights** in Reflect and AGENTS.md: the choice to keep a passage is
    the person's; the passage's claims are not.
11. **Highlight dates live in the marker**, so "this week's highlights" can be read from the note
    alone, without reading Thock's state.

## 13. Deferred

- **More reading rituals** in the Reading Routine: a daily resurfaced highlight appended to the
  daily note, a "finished a book" review skill, an author index.
- **Reading on the weekly dashboard**: a `reading` field in `data.js` and a panel for it.
- **Official Readwise MCP** as an optional, guided connection for live search across sources that
  aren't synced.
- **Reader queue** (API v3 `list`) as a second source, if Reader comes back.

## 14. Open questions

1. Is the Backlog panel still the right home once a third, non-email connector joins it, or is it
   time for a shared "Connections" surface? Out of scope here, but worth tracking.
