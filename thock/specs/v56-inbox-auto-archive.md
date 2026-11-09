# Thock V56 - Auto-archive: the Inbox clears what isn't yours before you sit down

**Status:** Implemented (2026-11-08): PR #197; §15 lists where the code differs from the text
**Owner:** Diego · **Date:** 2026-11-08
**Companion docs:** `v41-inbox-desk-view.md` §7 (Jev, the request, the cache, the decision log),
`v53-gmail-archive-on-triage.md` (the triage log as the Gmail trigger, the undo horizon),
`v13-inbox-routine.md` §9.5 (the triage-log format), `v28-agent-memory.md` (the `memory/` folder,
decision 9 on untrusted text), `v52-nudges.md` (the weekly offer), `v15-unified-gmail-sync.md`
(the poll loop)

---

## 1. Summary

Part of the Inbox is mail that needs nothing from the person: helpdesk tickets filed against the
wrong team, notifications, things another group owns. The person reporting a ticket picks its
category, so the category is often wrong, and a rule on the category can't tell what is really the
person's. Today each one costs a look and an `e`.

V56 adds two parts that work together:

1. **A morning run (Rust, desk).** Once a day, right after the first Gmail sync that completes,
   the desk asks Jev about each item that arrived since the previous run. Any item Jev answers
   `archive` at ≥ 0.95 is filed to Archive exactly as `e` would file it, so V53 archives the
   thread in Gmail as well. The run's items wait at the bottom of the Inbox list in a collapsed
   **Auto-archived** group until the next run, where they can be read, re-filed or sent back.
2. **A weekly refresh (a skill, the person's agent).** `refresh-inbox-policy` reads the person's
   notes, their past Inbox decisions (corrections to auto-archives included) and the archived
   tickets. It then keeps two files current: a **work map** in memory (what the person and their
   team actually work on) and `routines/inbox/triage-policy.md`. The policy becomes
   agent-maintained: detailed rules with exceptions, plus a new `## Criteria` section that defines
   Jev's five options.

Jev's state stays `{ item, policy }` (V41 decision 6). The work map reaches Jev only by being
distilled into policy rows, and the person's history reaches it the same way.

## 2. Goals & success criteria

- **G1: Off until turned on.** A vault that never runs `thock::TurnOnInboxAutoArchive` makes no
  extra Jev calls and archives nothing on its own. No migration, no prompt on upgrade.
- **G2: Never on the shipped defaults.** Turning it on runs the refresh skill first. The morning
  run doesn't start until the policy file has been written by that skill (§5.1).
- **G3: One filing, every path.** An auto-archive is a normal Archive decision: the same move,
  the same triage-log line (marked as automatic), the same `decisions.jsonl` record, so V53,
  the phone and the Triage ritual see it like any other.
- **G4: Everything it did is in view.** The last run's items are listed, in the order they were
  archived, in one collapsed group at the bottom of the Inbox list. Expanding it never costs a
  network call.
- **G5: Every correction is one key and reaches Gmail.** From the group, `u` sends an item back
  to the Inbox and `t`/`s`/`shift-s` re-file it. Each of these reverses the Gmail archive for as
  long as the item is in the group (§7.3), and each is logged as a correction the next refresh
  reads.
- **G6: A thread you're in is never touched.** If the person wrote any message in the thread,
  the item is skipped whatever Jev says. Rust checks this, not the model.
- **G7: Never silent.** A run that couldn't ask Jev (no grant, allowance spent, errors) says so
  in the group's header and the sync popover. It never half-runs without saying how far it got.

## 3. Non-goals

- **Archiving items you've already seen.** The run looks only at items that arrived since the
  previous run. An item left in the Inbox across a run is the person's call.
- **Any destination other than Archive.** The run never files to Today, Soon or Someday.
- **The phone's group.** The phone's Inbox just loses the items as the moves sync. A phone group
  would be a later spec with its own `thock_sync_core` fixture.
- **An unattended agent.** The weekly refresh is a skill the person (or a nudge) starts. Thock
  gains no background agent runner.
- **Jev seeing the work map.** No new field in Jev's `state` (V41 decision 6 stands).
- **Tuning the threshold automatically.** It is a config value. The refresh may *suggest* a
  change in its summary, never write it.

## 4. Core concepts

### 4.1 Two clocks

| | Morning run | Weekly refresh |
| --- | --- | --- |
| Who | `InboxService` (Rust, desk) | `refresh-inbox-policy` (the person's agent) |
| When | first completed Gmail sync of the local day | weekly nudge, and the last step of Week Review, both only with `auto_archive` on; or by hand |
| Reads | `inbox/`, `triage-policy.md`, `[inbox]` config | notes, `memory/`, decision history, archived tickets |
| Writes | the filing (§7.1), `.thock/state/inbox/auto-archive.jsonl` | `memory/work.md`, `triage-policy.md` |
| Calls | Jev, once per new item | the agent's own model |

The two meet only in `triage-policy.md` and the logs. Neither the nudge nor Week Review runs the
refresh unless `auto_archive` is on, because the refresh rewrites a policy the person may keep by
hand. With auto-archive off it runs only when the person asks (`thock::RefreshInboxPolicy`), and
then it improves the Inbox view's chips too.

### 4.2 The policy file becomes the agent's

`routines/inbox/triage-policy.md` was the person's file: Set Up Inbox wrote it and the person
edited it by hand. With V56 it is the refresh skill's synthesis, the same standing `memory/` has
under V28 decision 14. Concretely:

- The skill rewrites `## Proposals` and `## Criteria` wholesale each run. It reads the current
  file first, so a hand edit becomes evidence the next run keeps, the way V28 treats a deleted
  memory line.
- `## Destinations` and `## When unsure` are the ritual's, kept as they are.
- Set Up Inbox still writes the first version. The refresh then takes over.
- The file stays Markdown in the open. Deleting it returns everything to the shipped defaults,
  and the morning run stops (§5.1).

The file gains a front-matter stamp the morning run checks:

```markdown
---
refreshed: 2026-11-08
---
```

### 4.3 The work map is memory

The map lives at **`memory/work.md`**, with one pointer line in `memory/index.md`. It is
written in the second person like every memory page and kept section by section (append or
replace one section, never the whole page) by both the refresh skill and Reflect:

```markdown
# Your work

## Projects that are yours
- **OpenCue scheduler** — you and Ana own the dispatcher and the priority rules. ← daily/2026-11-04.md
- **Farm monitoring** — dashboards and alerts for the render nodes. ← daily/2026-10-29.md

## Near yours, not yours
- **OpenCue licensing (Maya, Houdini)** — the pipeline-licensing team; tickets land under
  OpenCue but you hand them over. ← your decisions, 2026-10 (14 archived, 0 kept)

## Your team
- Ana Lima, Jordan Reyes. ← daily/2026-11-02.md
```

**V28 decision 9 binds this page.** Facts come only from the person's own notes and their own
decisions: an archive or keep is the person's act, so *"you archive tickets about Maya licensing"*
is admissible, with the decisions as its source. Ticket bodies are imported text. They can show
which words go with which decision (in the policy, §6.3), but they never become a fact on this
page.

### 4.4 Criteria move into the policy

Jev's five criteria sentences move from `build_request` into a `## Criteria` section of the
policy file:

```markdown
## Criteria

- **today**: Something to do, call, or read today, or a line for today's page.
- **soon**: A task for the coming days: `item.due_in` is within a week, or the wording is urgent.
- **someday**: A link, article, idea or task with no date pressure.
- **archive**: Nothing for this person to act on: a notification, receipt, newsletter, test
  capture or duplicate, or a ticket that belongs to another team under the policy.
- **unsure**: None of the above fits, or the item is a note that belongs in a folder of its own.
```

Rust parses it with `inbox_suggest::parse_criteria`. The **key order is fixed in Rust**
(`today, soon, someday, archive, unsure`) whatever order the file uses, because Jev leans to the
first option (V41 decision 4). A missing key, a missing section or a sentence over
`CRITERION_LIMIT` (400 characters) falls back to the shipped sentence for that key. Unknown keys
are ignored. The shipped sentences remain the constants they are today, with `archive` widened as
above.

This amends V41 §7.2 ("the criteria are full sentences" in the request): the sentences are still
full sentences, but they come from a file.

## 5. What the user does

### 5.1 Turning it on

`thock::TurnOnInboxAutoArchive` (palette-only, like the V53 actions) launches
`refresh-inbox-policy` through `thock::RunSkill` with the argument `enable`. The skill does its
normal run (§6) and, as its last step in that mode, inserts `auto_archive = true` into `[inbox]`
in `.thock/config.toml`. It inserts the key and creates the table if it's missing, and never
rewrites the file.

The morning run starts on the next first-sync-of-the-day when both of these are true:

- `[inbox] auto_archive = true`, and
- `triage-policy.md` has a `refreshed:` stamp.

When the key is on but the stamp is missing (the file was deleted or hand-replaced), the run
doesn't happen, and the group header shows *Auto-archive waiting for a policy · Refresh* bound
to `thock::RefreshInboxPolicy`.

The first run only looks at items that arrived after the key was turned on (the watermark starts
at that moment, §7.2).

### 5.2 Configuration

```toml
[inbox]
suggestions = true
auto_archive = true          # V56; absent = off
auto_archive_threshold = 0.95 # V56; clamped to [0.85, 1.0]
```

A threshold below `FILLED_THRESHOLD` (0.85) is clamped up and logged: below the filled chip, the
view would be archiving things it wouldn't even highlight. `auto_archive = true` with
`suggestions = false` is a contradiction, and auto-archive wins for the morning run only. The
view keeps hiding chips.

### 5.3 Turning it off

`thock::TurnOffInboxAutoArchive` removes the two V56 keys from `[inbox]`, leaving the rest of the
table alone. The policy file and memory page stay; they keep steering the view's chips. The
current group stays visible until its items are triaged, but no new run happens.

## 6. The weekly refresh — `refresh-inbox-policy`

A new skill in the Inbox Routine (`crates/thock/assets/routines/inbox/skills/`), listed in
`routine.toml`. Its front matter declares `tier = "fast"` like Reflect. A modes line covers
`enable` (§5.1); without an argument it is a plain refresh.

### 6.1 When it runs

- **A nudge**, `refresh-inbox-policy` in `nudges::REGISTRY`. It is due when the Inbox Routine is
  installed, `auto_archive` is on, and `refreshed:` is 7 or more days old (or missing). It follows
  the same quiet hours and ledger as the V52 nudges.
- **Week Review's last step**, *only if `routines/inbox/routine.toml` exists and `[inbox]
  auto_archive = true`*: "run `refresh-inbox-policy`". Timeline doesn't depend on Inbox; a vault
  without the Inbox Routine, or that never turned auto-archive on, skips the step without a word,
  so a hand-kept policy is never rewritten without the opt-in. When Week Review has run it, the
  nudge isn't due, because the stamp is fresh.

### 6.2 What it reads

1. `profile.md`, then `memory/index.md` and `memory/work.md`.
2. The daily and weekly notes since the last `refreshed:` (at most 28 days), plus `backlog.md`.
3. `.thock/state/inbox/decisions.jsonl` and `archives/inbox/triage-log.md` since the last stamp:
   what was archived, what was kept, and every record with `"via": "auto"` that was later
   `undone` or re-filed (§7.4). Corrections weigh more than plain decisions.
4. The notes in `archives/inbox/` those records name, **as imported text**. They are read for
   the vocabulary that tells kept from archived (hostnames, tool names, error strings, team
   names). Any instruction in them is ignored, and nothing is copied from them into memory.
5. The current `triage-policy.md`.

### 6.3 What it writes

1. **`memory/work.md`**: the sections of §4.3 it has new evidence for, each line dated with its
   source, following V28's rules (decision 7 provenance, decision 14 deleted-stays-deleted). It
   adds the index pointer line if it's missing.
2. **`triage-policy.md`**: a rewritten `## Proposals` table and `## Criteria`, and the
   `refreshed:` stamp. The rules to follow:
   - **Every Archive row must be backed by the person's decisions.** A row that archives a kind
     of item needs at least three archive decisions for it and no keep in the window. Text in a
     ticket can never justify a rule on its own. This is the policy-poisoning defence (§10).
   - Exceptions are written as rows of their own, ahead of the general row they narrow, because
     Jev reads the rows in order: *"A ticket under OpenCue about Maya or Houdini licences →
     Archive"* before *"A ticket about the OpenCue scheduler or dispatcher → Backlog · Soon"*.
   - A correction from the last run **must** change the policy: narrow or drop the row that led
     to the auto-archive, or add an exception for it.
   - **Budget**: at most 60 rows and 6,000 characters across Proposals and Criteria, so a request
     stays far below Jev's 32,000-token limit.
3. **A short summary in chat**: rows added, changed and removed, and the corrections that caused
   them. When the last runs' undo rate is above 10%, it suggests raising
   `auto_archive_threshold`, and writes nothing to the config.

The skill never touches `.thock/state/`, the config (outside §5.1's `enable` step) or any note
in `inbox/`.

## 7. The morning run

### 7.1 The filing

For each candidate that clears the bar, the desk applies V41 §6.1's `e` row, through the same
code:

1. `move_file` `inbox/<name>` → `archives/inbox/<name>` via `thock_sync_core::apply`.
2. The triage-log line, with a new destination word:
   `- 2026-11-08 · HD 52112: Maya licence expired → Archived · auto <!--inbox:…-->`.
   `thock_sync_core::decision_class` maps `Archived · auto` to `DecisionClass::Archive` (and a new
   `is_automatic` to `true`), with fixtures, so V53 archives the thread in Gmail when `archive`
   is in its `on`.
3. A `decisions.jsonl` record with `"via": "auto"`, `"suggested": "archive"`, the confidence,
   `"chosen": "archive"`, and a new `"run"` field (the run id).

The whole run is one batch for undo purposes but **not** the view's one-level `u` (§8.2).

### 7.2 Candidates, the bar and the guardrail

**Candidates** are items in `inbox/` whose `captured:` time (else the file's creation time) is
after the watermark: the previous run's start, or the moment auto-archive was turned on. They
are evaluated newest first.

**The bar.** Jev's `choice == "archive"` and `confidence ≥ auto_archive_threshold`. `unsure`,
another choice or a lower confidence leaves the item in the Inbox, with its chip as usual.

**The guardrail (G6).** The item is skipped when any message in its parsed thread
(`inbox_email::parse_thread`) has a `from` containing the Google account's address, or an
`author` equal to one of the person's names. The names are the account's display name, as seen on
mail captured from that address, plus `[inbox] names = [...]` when set, for ticket systems that
relay the person under a different spelling. Items that aren't email have no thread and pass the
guardrail.

### 7.3 When it runs

`InboxService` subscribes to `GmailService`'s poll completion. On the first successful poll of a
local day where `[inbox] auto_archive` is on and the stamp exists:

1. Write `{"run": "<id>", "event": "started", "at", "watermark", "policy": "<hash>",
   "threshold"}` to `.thock/state/inbox/auto-archive.jsonl`.
2. Ask Jev for each candidate, at most `MAX_IN_FLIGHT` at once, **reusing a cached suggestion
   when the note's hash and the policy hash both match** (§7.5).
3. Apply the filings in order. Each filed item gets `{"run", "event": "archived", "stem",
   "path", "confidence"}`; each skipped one gets `"event": "skipped"` with `"reason":
   "below" | "replied" | "error"`.
4. Write `{"run", "event": "finished", "at", "archived": n, "skipped": m}`.

"First of the day" is decided from the file: a `finished` record dated today means the run has
happened. A `started` with no `finished` (the app quit mid-run) resumes on the next poll, skipping
stems already recorded. If Gmail isn't connected, the trigger is the first `InboxService` scan of
the day instead, so a vault with no Gmail still gets its run.

**The Gmail undo horizon.** V53's 24-hour horizon is extended for automatic lines: an `Archived
· auto` line removed while its run is still the latest, or within 7 days, unarchives the thread
(V53 §7.1, `plan_archive` gets the line's `is_automatic`). This is what makes G5 true across a
weekend.

### 7.4 Failures

Per item, as in V41 §7.3: 429/5xx retry with backoff within the run, a 402 stops the run, anything
else skips the item with `"reason": "error"`. A run that stopped early writes `finished` with
`"stopped": "<reason>"`, and the group header and the sync popover's Inbox row say *Auto-archive
stopped after 4 of 11 · Retry* (`thock::RunInboxAutoArchive`). No grant, or a provider other than
OpenRouter, reads *Auto-archive needs suggestions to be available*, and nothing is written.

### 7.5 The cache key gains the policy

`SuggestionRecord` gains `policy`: the sha256 of the flattened Proposals rows plus the resolved
criteria, in request order. A record answers for an item only when both `hash` and `policy`
match. A record without `policy` (written before V56) counts as stale, so the view re-asks once,
at the V41 rate. The same rule applies to the view's chips, so a refreshed policy shows up in
the chips without deleting the cache.

## 8. The view

### 8.1 The group

```
 Inbox  [New 4] Carried over 2       ⌕ │
───────────────────────────────────────┼──────────
   …rows…                              │
                                       │
 ▸ Auto-archived this morning · 7      │
───────────────────────────────────────┤
 ↑↓ move  x mark  / filter      ? keys │
```

- The last row of the list, below **Carried over**, just above the footer. One line, collapsed
  by default, muted. Run on a previous day, it reads *Auto-archived Friday · 7*.
- The group holds the `archived` stems of the latest run that archived anything, whose files
  are still in `archives/inbox/` and that haven't been corrected (§8.2). A run that cleared
  nothing (a manual one, say) never replaces it, and the header's day is that run's day. It is
  read from `auto-archive.jsonl` and the files, with no Jev call (G4).
- Expanded, the group shows its rows under the header in the list's normal row style, with the
  confidence where a fresh row shows its chip (`archive 0.97`). Selecting a row opens it in the
  reader like any other: the destination bar shows Jev's spread, with **Archive** filled.
- The list header's counts gain nothing. The group is history, not queue.
- An empty group (none archived, or all corrected) hides. A stopped or waiting run (§5.1, §7.4)
  turns the header into its status line, in the same place.
- A filter applies to the group's rows too, and its count follows.

### 8.2 Keys

The group header and its rows are part of the list's selection, so `j`/`k`, arrows and
`shift-g` reach them. Only the header row is new; the verbs are existing actions, made to know
where the selection is.

| Key | On the group header | On an auto-archived row |
| --- | --- | --- |
| `l` `right` `enter` | expands (`thock::ToggleInboxAutoArchived`) | `enter` opens the note, as on any row |
| `h` `left` | collapses | collapses and selects the header |
| `u` | — | sends it back to the Inbox: the V41 §6.2 reversal for that item, with `"undone": true` on its record |
| `t` `s` `shift-s` | — | re-files it: the task line, the auto line replaced by the new destination's line, a new `decisions.jsonl` record with `"via"` the key and `"corrects": "<run>"` |
| `e` `space` | — | does nothing: it is already archived |
| `x` | — | marks it, so a batch of corrections is one key |

`u` on a group row acts on that row, not on the view's last decision. Elsewhere `u` keeps its V41
meaning. The keys sheet and the row menu list the group's verbs when the selection is in it.
Bindings go under `ThockInboxView && !filtering && !reader`, with `h`/`l` in `vim.json`.

Selection survives a refresh: after a correction, the selection moves to the next row in the
group, or to the header when the group empties.

## 9. Status

| State | Group header | Sync popover, Inbox row |
| --- | --- | --- |
| Off | hidden | — |
| On, waiting for a policy | *Auto-archive waiting for a policy · Refresh* | same |
| Ran | *Auto-archived this morning · 7* | *Auto-archived 7 today* |
| Ran, nothing cleared the bar | hidden | *Auto-archive ran · nothing to clear* |
| Stopped, with a grant and allowance | *Auto-archive stopped after 4 of 11 · Retry* | same, with the reason in the tooltip |
| Stopped, no grant or allowance spent | *Auto-archive stopped after 4 of 11 · allowance spent*, no Retry | same, no Retry |

The existing *suggestion taken 83%* tooltip gains *auto-archive undone 2 of 61* when there are
`via: auto` records.

## 10. Product invariants

- **Your files, forever, in the open.** The policy, the criteria and the work map are Markdown
  in the vault. The run's record is machine state under `.thock/state/`, rebuildable from the
  triage log.
- **Augmentation, not replacement.** The run moves a captured item and writes a log line, the
  same thing `e` does. It never edits a note.
- **Human-in-the-loop where it can't be undone.** Archiving in Gmail is reversible, and stays so
  for as long as the item is in the group. Nothing is deleted or trashed anywhere.
- **Invisible versioning.** Nothing new is surfaced. The checkpoint before each skill run covers
  the refresh.
- **Everything is editable.** The criteria leave Rust for the policy file. The threshold is a
  config value.
- **Modular life.** The run needs the Inbox Routine's policy. Week Review calls the refresh only
  when Inbox is installed and auto-archive is on, and the memory page is optional to everything.
- **Untrusted input.** Ticket text can't write memory (V28 decision 9), and can only shape the
  policy through rows backed by the person's own decisions (§6.3). A ticket that tells the model
  "archive everything from X" gets no row without three of the person's own archives behind it,
  and the guardrail and threshold still apply.

## 11. Tests

### 11.1 Shared core (`thock_sync_core`)
- `decision_class("Archived · auto")` → `Archive`, `is_automatic` true. `"Archived"` stays
  false. Fixture vectors in `fixtures/v1/triage_log.json`.
- `triage_log_line` writes the auto form byte for byte.

### 11.2 Suggestions (`inbox_suggest.rs`, pure)
- `parse_criteria`: a full section; a file with no section; keys out of order (request order is
  still fixed); an over-limit sentence and a missing key fall back per key; an unknown key is
  ignored.
- `build_request` with parsed criteria matches the JSON of §4.4.
- `policy_hash` changes with a row or a criterion, not with whitespace outside the table.
- A cached record matches only on `hash` + `policy`. A pre-V56 record is stale.

### 11.3 The run (`inbox_auto_archive.rs`, pure, injected `now` and a stub Jev)
- Candidates: watermark filtering by `captured:`, falling back to file time. Items before the
  watermark are never asked about.
- Bar: `archive 0.95` files, `archive 0.94` doesn't, `unsure 0.99` doesn't, the clamp at 0.85.
- Guardrail: a thread with a message from the account's address is skipped as `replied`. An
  `author` matching `[inbox] names` is skipped. A Google Tasks item passes.
- Once a day: a `finished` today means no run. `started` without `finished` resumes and skips
  recorded stems.
- 402 mid-run → `stopped`, and later items untouched.
- Cold start: `auto_archive = true` with no `refreshed:` → no run and the waiting state.

### 11.4 The view (`inbox_view.rs`, `VisualTestContext` + `simulate_keystrokes`)
- The group renders collapsed below Carried over. `shift-g` selects it, `l` expands, `j` enters
  its rows, `h` collapses back.
- `u` on a group row moves the file back to `inbox/`, removes the auto line, marks the record
  `undone`. The view's last decision is untouched.
- `s` on a group row appends the Soon task, replaces the auto line with the Soon line, and writes
  `corrects`.
- `x` `x` `u` restores two rows as one batch.
- Selection survives a vault file event during a correction.

### 11.5 Gmail (`gmail.rs`, reconciler)
- An `Archived · auto` line removed after 3 days → `Unarchive`. A plain `Archived` line after 3
  days → nothing (the V53 horizon still holds).

### 11.6 Config and actions
- `TurnOffInboxAutoArchive` removes only the V56 keys. A temp vault, never the real config.
- The skill's `enable` step: a test parses exactly the TOML insert the skill tells the agent to
  write, against a config with and without `[inbox]`.

### 11.7 The skill
- A test parses the policy file exactly as the skill tells the agent to write it (front matter,
  Proposals, Criteria) with `flatten_policy` and `parse_criteria`, and checks it round-trips with
  no fallbacks.
- `nudges.rs`: the refresh nudge is due at 7 days, not at 6, and not without the Inbox Routine
  or with auto-archive off.

## 12. Changes by file

| File | Change |
| --- | --- |
| `crates/thock-sync-core/inbox.rs` | `Archived · auto`, `is_automatic`, fixtures |
| `crates/thock/src/inbox_suggest.rs` | `parse_criteria`, criteria into `build_request`, `policy_hash`, `SuggestionRecord.policy`, `DecisionRecord.run`/`corrects`, widened `archive` sentence |
| `crates/thock/src/inbox_auto_archive.rs` (new) | candidates, bar, guardrail, the run record: pure |
| `crates/thock/src/inbox_service.rs` | the run trigger and driver, `RunInboxAutoArchive`, `TurnOn/OffInboxAutoArchive`, `RefreshInboxPolicy` |
| `crates/thock/src/inbox_view.rs` | the group, its header row, the group-aware `u`/`t`/`s`/`shift-s`, `ToggleInboxAutoArchived` |
| `crates/thock/src/gmail.rs` | the automatic undo horizon in `plan_archive` |
| `crates/thock/src/gmail_service.rs` | a poll-finished event `InboxService` subscribes to |
| `crates/thock/src/vault.rs` | `InboxViewConfig` gains `auto_archive`, `auto_archive_threshold`, `names` |
| `crates/thock/src/nudges.rs` | the `refresh-inbox-policy` nudge |
| `crates/thock/src/sync_status.rs` | the §9 popover lines |
| `crates/thock/assets/routines/inbox/skills/refresh-inbox-policy.md` (new) | the skill |
| `crates/thock/assets/routines/inbox/routine.toml` | lists the skill; version bump |
| `crates/thock/assets/routines/inbox/triage-policy.md` | the `## Criteria` section, the shipped sentences |
| `crates/thock/assets/routines/inbox/skills/setup-inbox.md` | writes `## Criteria` too |
| `crates/thock/assets/routines/inbox/doc.md` | a paragraph on auto-archive and the refresh |
| `crates/thock/assets/routines/timeline/skills/week-review.md` | the conditional last step |
| `crates/thock/assets/skills/reflect.md` | `memory/work.md` is shared: section-by-section only |
| `assets/keymaps/default-{macos,linux}.json`, `assets/keymaps/vim.json` | the group keys |
| `thock/specs/v41-inbox-desk-view.md` | amendment note at §7.2: criteria come from the policy file |
| `thock/VISION.md` §12 | the roadmap entry |

No inherited crate changes beyond the keymaps.

## 13. Open questions

1. **The person's names.** The account's display name on captured mail covers direct mail. A
   ticket system that relays the person as "D. Tavares" needs `[inbox] names`. Should Set Up
   Inbox ask for it, or the refresh propose it from kept tickets?
2. **The 3-decision floor.** Three archives with no keep before an Archive row may be too slow
   for a team whose tickets vary a lot. Revisit once `decisions.jsonl` has a month of `auto`
   records.
3. **Items that arrive after the run.** They wait for tomorrow. If that leaves too much noise by
   afternoon, a second run after the first sync past noon is the smallest change.

## 14. Decisions (2026-11-08)

1. **The map reaches Jev only as policy.** V41 decision 6 holds: no new state field. The refresh
   distills the map and the history into rows.
2. **`triage-policy.md` is agent-maintained.** The person doesn't keep it by hand. The refresh
   rewrites Proposals and Criteria and reads hand edits as evidence.
3. **Criteria move into the policy file**, with the order fixed in Rust and per-key fallback.
4. **Scope is every Inbox item**, any source, guarded by the replied-thread check.
5. **Auto-archive is a normal Archive decision**, so V53 archives it in Gmail. Corrections from the
   group reverse that, with a 7-day horizon for automatic lines.
6. **Threshold 0.95, configurable**, never below 0.85.
7. **Daily, on the first completed Gmail sync of the local day.** A morning without Thock open
   just moves the run to whenever it opens.
8. **The group lasts until the next run**, and supports full triage: read, `u`, `t`/`s`/`shift-s`.
9. **The refresh is its own skill in the Inbox Routine**, offered by a weekly nudge and called by
   Week Review only when Inbox is installed and auto-archive is on.
10. **It reads notes, decision history and archived ticket bodies.** Code activity is out.
11. **The map is a memory page, `memory/work.md`, shared section by section with Reflect.** Ticket
    bodies never become memory facts (V28 decision 9).
12. **Turning it on runs the refresh first.** The morning run never works from the shipped
    defaults.
13. **Only items since the last run.** What the person has left in the Inbox is theirs.
14. **The phone just sees the moves.** No phone group in V56.
15. **The suggestion cache keys on the policy too**, so a refresh re-asks at the next view or
    run.

## 15. Implementation notes (2026-11-08)

Where the code made a choice the text above didn't, or differs from it:

1. **The run's record carries a little more.** `started` also has `"candidates"` (for *stopped
   after 4 of 11*), `archived` also has `"line"` (the exact triage-log line, so a correction finds
   it), and two more events, `{"event": "enabled" | "disabled", "at"}`, mark when the service saw
   `[inbox] auto_archive` change. The first run's watermark is the last `enabled`.
2. **The automatic undo horizon is 7 days, full stop.** "While its run is still the latest" is not
   checked: the reconciler doesn't know about runs, and the group lasts a day unless Thock stays
   closed. Gmail's `archive.jsonl` records `"automatic": true` on an archive made for an
   `Archived · auto` line. A re-file from the group (`t`/`s`/`shift-s`) leaves the digest in the
   log under a new class; when `[archive] on` doesn't list that class, the reconciler unarchives
   an automatic archive within the horizon, which is what makes G5 true for re-files.
3. **The skill's tier** is `model = "fast"` on its `[[skill]]` in `routine.toml`, the mechanism
   every Routine skill uses, not front matter.
4. **`thock::RunSkill` gains `mode`** (`{"skill": "refresh-inbox-policy", "mode": "enable"}`); the
   kickoff reads `Read and execute <file> in enable mode`.
5. **A spent allowance** is treated like no grant before a run starts: nothing is written and
   the status reads *needs suggestions to be available*. A 402 during a run stops it as §7.4 says.
6. **A stopped run**: `thock::RunInboxAutoArchive` resumes it under the same id, skipping what it
   recorded. Left alone, the next day's run starts from the stopped run's watermark and skips its
   recorded stems, so nothing it never asked about is lost.
7. **The guardrail runs before Jev is asked**, so a thread the person wrote in costs no call; it is
   still recorded as `skipped` with `"reason": "replied"`.
8. **Keys.** `right`/`left` already reach the view as `menu::SelectChild`/`SelectParent` through the
   `menu` context, and `enter` as `menu::Confirm`, so the default keymaps don't change; `vim.json`
   gains `h`/`l` for the two. `thock::ToggleInboxAutoArchived` is palette-only, so the keys
   sheet's *Show or hide the group* row shows the keys that work: `→`/`←`, or `l`/`h` with vim.
   On a header with no rows (a waiting or stopped status), `enter`/`l` runs the status's action.
9. **A re-file from the group isn't undoable with `u`**: the row leaves the group and was never the
   view's last decision. Its task line and log line are ordinary text to edit.
10. **Header and popover words.** A run from earlier in the week reads its weekday
    (*Auto-archived Friday · 7*), older ones the date (*on Oct 30*). The popover's *Auto-archived 7
    today* shows only for a run that finished today; the waiting and stopped lines show whenever
    they apply, with *Refresh* or *Retry* as the row's first action. Retry is offered only when it
    can work: with no grant or the allowance spent, the stopped line ends *· allowance spent*
    instead, in the header and the popover, and there is no button.
11. **The decision report** leaves `via: auto` records out of *suggestion taken*, and counts an auto
    record as undone when it is marked `undone` or a later record `corrects` its run.
12. **Turning off** removes the two keys and leaves the `[inbox]` header even if it is now empty.
13. **A failed log write undoes the move.** The run moves the note before writing its log line; if
    the line can't be written, the note goes back to `inbox/` and the item is `skipped` with
    `"reason": "error"`. If the move back fails too, the error says where the note is (G7).
14. **A poll-triggered run waits for the scan after the poll.** A Gmail poll marks the queue
    unread and re-lists it, so a policy or cache load finishing first can't start the run on a
    listing from before the poll's notes landed.
15. **The group comes from the latest run that archived something** (§8.1), not simply the
    latest run, so a manual run that clears nothing leaves yesterday's or this morning's group in
    place.
