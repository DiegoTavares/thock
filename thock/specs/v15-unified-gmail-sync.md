# Thock V15 — Unified Gmail sync: labels route, folders mean

**Status:** Implemented (2026-08-31)
**Owner:** Diego · **Date:** 2026-08-31
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.3 Human-in-the-loop, §4.5 Everything
is editable), `v9-gmail-backlog-capture.md` (the stack this absorbs), `v13-inbox-routine.md` (the
note format and dedup posture this generalizes), `v6-backlog.md` (the Someday section the hook
appends to)

---

## 1. Summary

Today two parallel stacks capture email. `.thock/gmail.toml` + `GmailService` + `MailProvider`
turns `thock/backlog` threads into an archive file plus a backlog line; `.thock/inbox.toml`'s
`[gmail]` section + `InboxService` + `GmailInboxSource` turns `thock/inbox` threads into inbox
notes. They duplicate polling, dedup state, label resolution, and status plumbing — and each must
know about the other's label so a both-labels thread lands exactly once. Adding a third Gmail
destination means a third copy of all of it.

V15 collapses this into **one Gmail sync pipeline configured by a map**:

```toml
# .thock/gmail.toml
schema = 2

[[sync]]
label = "thock/backlog"
path  = "archives/emails"

[[sync]]
label = "thock/inbox"
path  = "inbox"
```

The service's whole job is **landing**: for every mapping, one Markdown note per labeled thread,
written into the mapped folder in the V13 inbox-note format. Nothing else. What happens to a landed
note is the concern of whatever owns that folder — the Inbox Routine's triage ritual for `inbox/`,
and for `archives/emails` a Rust-side **landing integration** owned by the backlog machinery that
appends the familiar `- [ ] Title [[stem]]` line to Someday the moment the note lands.

Two principles fall out and are the point:

- **Labels route, folders mean.** The map is pure routing. The behavior a note gets is determined
  by where it lands, not by which label brought it in. Point any label at `archives/emails` and it
  flows into the backlog; point a new label at a new folder and it just lands there.
- **A thread lands once.** All mappings share one digest space and one claim pass, so V13 §7.1's
  fast-lane exclusion stops being a special case two services coordinate on and becomes a
  structural impossibility.

Casualties, all deliberate (§10): title-mode import and its picker, the V9 flat-`backlog` label
fallback, `inbox.toml`'s `[gmail]` section, and the old top-level `gmail.toml` keys.

## 2. Goals & success criteria

- One config file, one poll loop, one dedup space, one transport for all Gmail capture.
- Adding a new label → folder pair is a two-line config edit, zero code.
- Functionality-equivalent for the two shipped lanes: `thock/inbox` threads become inbox notes
  exactly as V13 ships them; `thock/backlog` threads are archived and appear in Someday within one
  poll, unprompted, exactly as V9 full mode ships them.
- A thread carrying several mapped labels lands exactly once, claimed by mapping priority.
- No email already captured by V9/V13 is ever captured again (§9's digest bridge).
- The sync service contains no knowledge of `backlog.md`; the append lives with the backlog code.

## 3. Non-goals

- **No new sources.** Google Tasks stays in `InboxService` behind `InboxSource`, untouched, along
  with the landing-zone queue-depth scan and the triage machinery.
- **No write-back to Gmail.** Read-only posture unchanged; labels are never modified.
- **No user-facing integration registry.** Integrations are a Rust-internal table with one entry
  (§7). Users extend Thock through the config map; a `routine.toml`-declared integration hook is a
  future spec if a second integration ever wants to exist.
- **No change to triage, the Backlog document format, or the marker conceal** (`<!--gmail:…-->`
  stays the marker the editor hides).

## 4. Core concepts

### 4.1 The map is the config

`[[sync]]` entries are ordered; **order is priority**. A thread carrying labels of several mappings
is claimed by the earliest one. The shipped default — used when the file has no `[[sync]]` entries
at all — is `thock/backlog → archives/emails` first, then `thock/inbox → inbox`, preserving V13's
"both labels take the fast lane" behavior. When any explicit `[[sync]]` entry exists, the explicit
list *is* the whole map; defaults don't merge in underneath.

### 4.2 Landing is the whole job

Every mapping lands the same artifact: one note per thread in the V13 §6 inbox-note format —
`source`/`capture`/`captured`/`title`/`link` frontmatter, `# Title`, body. One addition to that
format: an optional `from:` field, written when the source knows the sender, so the archived email
keeps what V9's archive format recorded. (V13 notes simply never had a sender; nothing breaks.)
The body is the whole conversation, not just one message — §7.1 has the per-message rendering.

Filename, collision handling, sanitization, and the "a capture is never dropped silently"
placeholder body are V13 §6/§8, reused verbatim — the planner is the same planner, run once per
mapping with the mapping's folder.

### 4.3 Integrations are hooks on landing

A landing integration is Rust code keyed by **folder path**, invoked by the service after a note
lands in that folder, inside the same sync pass. One exists:

> `archives/emails` → append `- [ ] <title> [[<stem>]] <!--gmail:<digest>-->` to `backlog.md`'s
> Someday section.

The append reuses V9 §9's apply machinery unchanged: through the open buffer as one finalized
transaction behind the typing guard when `backlog.md` is open, read-modify-write through the
project `Fs` otherwise, `append_to_section_edit` either way, scaffold-if-missing. The marker makes
it idempotent — a crash between landing and appending is repaired by the next poll's marker guard,
never duplicated.

The table lives with the backlog code (`backlog.rs` owns the folder constant and the append), not
with the sync service; `gmail_service.rs` only walks the table. The hook fires on the *folder*, so
a user who remaps `thock/backlog` elsewhere gets plain landing (their choice), and any label mapped
onto `archives/emails` gets backlog integration (also their choice).

### 4.4 The vault is the record

V13 §4.3's posture, now uniform across every mapping: the state file
(`.thock/state/gmail/imported.jsonl`) is a cache; the rebuildable record is the vault itself — the
`capture:` frontmatter of notes in every mapped folder, the `<!--gmail:…-->` markers in
`backlog.md`, and the triage log's `<!--inbox:…-->` markers. The digest is V13's
`capture_digest(account, "gmail", thread_id)` for all mappings; V9's older digest is honored
read-only during the transition (§9).

## 5. What the user does

Unchanged, which is the test of the refactor:

1. On the phone, label a thread `thock/inbox` → within a poll it is an inbox note awaiting triage.
2. Label a thread `thock/backlog` → within a poll the email is archived under `archives/emails/`
   and an unchecked task with a `[[wikilink]]` to it sits in Someday.
3. (New) Edit `.thock/gmail.toml`, add `[[sync]] label = "thock/reading" path = "reading/queue"`,
   create the label in Gmail → threads land as notes in `reading/queue/`. No skill, no code.

## 6. Configuration — `.thock/gmail.toml` schema 2

```toml
schema = 2

# Poll cadence for the whole pipeline. Clamped to [60, 3600].
# poll_seconds = 300

# Ordered; first matching mapping claims the thread. Omitting every [[sync]]
# entry ships the two defaults below.
[[sync]]
label = "thock/backlog"        # full label path, matched case-insensitively
path  = "archives/emails"      # vault-relative folder, created on demand

[[sync]]
label = "thock/inbox"
path  = "inbox"
```

Parsing rules, in the V9/V13 tradition:

- Every field optional; unknown fields ignored (future keys must not break this build). An
  unparseable file disables sync with a `Failing` status row, never a panic.
- `label` and `path` are trimmed; `path` is trimmed of slashes. An entry missing either is dropped
  with a logged warning; two entries with the same label keep the first.
- No `[[sync]]` entries → the defaults of §4.1. An empty-but-present file therefore behaves like
  today's default vault.
- The file's presence is still what gates the Gmail status row (V9 §10.3 G5 unchanged).
- `account` and client override come from `.thock/google.toml` (V13 §7.4). Legacy top-level keys
  (`account`, `label`, `import`, `archive_dir`) are simply unknown fields now: read never, ignored
  silently, and — per the vault rules — never rewritten or deleted by Thock.

`.thock/inbox.toml` keeps only what is genuinely the Inbox Routine's: `dir`, `poll_seconds` (for
the Tasks poll), and `[tasks]`. Its `[gmail]` section becomes an ignored unknown table.

Note the coupling `inbox.toml` had is now explicit in one file: the inbox mapping's `path` must
match `inbox.toml`'s `dir` for triage to see Gmail captures. Both default to `inbox`, so the
default vault needs nothing; the setup skill (§11) is told to keep them aligned when it customizes.

## 7. Architecture

```
gmail.rs          config (SyncMapping list), unified planner glue, digest bridge
gmail_google.rs   one transport: labels.list once, claim pass, per-thread fetch
gmail_service.rs  one service: poll loop, vault scan, landing, hook dispatch, state
inbox.rs          CapturedItem, note format (+ optional from:), planner — unchanged home
inbox_service.rs  Google Tasks + queue depth + triage plumbing; Gmail source removed
backlog.rs        EMAIL_ARCHIVE_DIR constant + the Someday append integration
```

Deleted outright: `MailProvider`, `ImportMode`, `CapturedEmail` (folds into `CapturedItem` with
`from`), `GoogleMailProvider`/`GmailInboxSource` (merge into one `GmailTransport`), the
`ImportModePicker`, `ChooseEmailImportMode`, the legacy-label fallback and its status hint, and
V9's separate archive renderer and `thread:` frontmatter writer.

### 7.1 One fetch pass

Per poll: one `labels.list`, resolving every mapped label by full path name, case-insensitively
(V9's resolution, cached until an error suggests staleness). Then per mapping in priority order:
`messages.list` by `labelIds`, newest first — the listing's only job is discovering thread ids.
A thread already claimed this pass — or whose digest (either construction, §9) is in the skip
set — is skipped. Claimed threads are fetched whole (`threads.get`, `format=full`, one request
per thread) and become `CapturedItem`s tagged with their mapping: subject and sender come from
the thread's first message, the item's moment from its last, and the body carries every
non-draft message's text oldest-first — a single-message thread keeps V13 §6's bare body, a
longer one renders one `## sender — date` section per message so replies read in order.

A mapped label missing from Gmail is a per-mapping **holding** note (`label "thock/reading" not
found`), surfaced in the status row; other mappings keep capturing — the V13 "errors isolate per
source" rule applied per mapping.

### 7.2 One apply pass, crash-safe

V9 §9's order, generalized: **notes first** (create-if-missing, `atomic_write`, per mapping), then
**integrations** (the backlog append, idempotent by marker), then **state** (append to
`.thock/state/gmail/imported.jsonl`: digest, thread id, title, mapping path, timestamp). A crash
at any boundary re-plans next poll into a state repair, never a duplicate — the same story V9 and
V13 each told, now told once.

The per-poll vault scan (V13's, widened): stems and `capture:` digests of every mapped folder —
triage moves files out of `inbox/`, so this stays a per-poll scan, and it doubles as the
queue-depth count `InboxService` currently makes for the row. Plus `backlog.md` markers and the
triage-log markers.

### 7.3 Status rows

The Backlog panel keeps two rows with cleaner ownership: the **Gmail row** (new service: idle /
synced / holding with the missing labels named / failing / disconnected) and the **Inbox row**
(`InboxService`: Google Tasks state and queue depth, exactly as today). `SyncGmailNow` drives the
unified pipeline; `SyncInboxNow` now means "Tasks now". The legacy-label rename hint disappears
with the fallback.

## 8. The planner (contract deltas only)

V13 §8's planner is the planner; V15 runs it once per mapping with the mapping's folder as `dir`.
Deltas:

- `CapturedItem` gains `from: Option<String>`; the note renderer writes `from:` when present.
- The claim pass (§7.1) happens *before* planning, in the transport — the planner still dedups
  defensively, but cross-mapping uniqueness is not its job.
- `ImportRecord` gains the mapping `path`, recorded in state for the human reading the jsonl; the
  machine still keys on digest alone.

Everything else — sorting by moment, stem collision suffixes, hostile-title sanitization,
state-repair on vault-digest hits, the placeholder body — is untouched and its tests carry over.

## 9. Migration

A clean break in config, a bridge in dedup:

- **Config.** Old `gmail.toml` files parse as schema 2 with every legacy key ignored — for a vault
  that ran on defaults (label defaults, `import = "full"`) behavior is identical. The dogfood vault
  is hand-migrated to an explicit `schema = 2` in the same change. A vault still on
  `import = "title"` changes behavior: labeled threads are archived now (the strictly richer
  capture); its existing title-mode lines in `backlog.md` stay valid and their markers keep
  deduplicating.
- **Digests.** The skip set loads, without distinction: both state files
  (`state/gmail/imported.jsonl`, `state/inbox/imported.jsonl`), `capture:` *and* legacy `thread:`
  frontmatter from mapped folders, backlog markers, triage-log markers. For every fetched thread
  the transport computes the V13 digest *and* V9's `thread_marker_id`, skipping on either hit.
  New writes carry V13 digests only. The legacy computation is one function kept for the bridge,
  marked for removal once the dogfood vault's state has fully rolled over.
- **Labels.** The V9 flat `backlog` fallback is gone; a vault whose Gmail still uses the flat
  label sees an honest `Holding: label "thock/backlog" not found` row and renames the label —
  the transitional hint served its purpose.
- **State layout.** `.thock/state/gmail/` remains the pipeline's state home. Gmail records stop
  being written to `state/inbox/imported.jsonl`; existing lines there stay valid skip input.

## 10. Removed behavior (deliberate)

| Removed | Why it can go |
| --- | --- |
| Title import mode + picker | Contradicts "the service lands the email"; full mode is a superset and the dogfood vault already runs it. Old title lines remain valid. |
| V9 flat-label fallback + hint | Transitional by design (V13 §7.1); the holding row now names the missing label honestly. |
| `inbox.toml [gmail]` | The map owns label routing; one config for one concern. |
| Cross-service fast-lane plumbing | One claim pass makes exclusion structural. |

## 11. Implementation notes

- Update the vault-visible docs that teach the old shape: `routines/inbox/doc.md` and
  `skills/setup-inbox.md` (the `[gmail]` section and label story), the timeline routine's
  `connect-google-workspace.md` where it names `gmail.toml` keys.
- `markdown_syntax.rs` conceal needs nothing: the marker prefix `<!--gmail:` is unchanged.
- Worktree-event reload set for the service: `gmail.toml`, `google.toml`, `config.toml` — plus
  mapped-folder events no longer matter to *this* service (the scan is per-poll), while
  `InboxService` keeps its landing-zone watch for queue depth.
- Tests to carry/port: V9 service tests (title-mode ones die with the mode; full-mode capture,
  state-loss repair, buffer-vs-fs append port to the hook), V13 planner and service tests
  (unchanged), new tests for mapping parse/priority claim, multi-mapping landing, digest bridge
  (a V9-format archive + old state line prevents recapture), holding-per-mapping, and the hook
  firing for any mapping routed at `archives/emails`.
- Ship order mirrors V13 §7.4's advice: land the config/planner/transport unification behind
  behavior-identical tests first; delete the old stack in the same PR only once the unified tests
  cover both lanes.

## 12. Decision log (2026-08-31)

1. **Backlog append = Rust hook on landing**, keyed by folder, owned by backlog code — not inline
   in the sync service, not deferred to a skill (the instant, unprompted append is shipped
   behavior worth keeping).
2. **Title mode dropped.** Every mapping archives the full email; the picker goes.
3. **Clean break on config and fallbacks**, with a read-only digest bridge so nothing is ever
   captured twice; dogfood vault hand-migrated.
4. **Labels route, folders mean** — integration attaches to the destination folder, so the map
   stays a pure `label → path` table and remapping is always safe.
5. **`from:` joins the note format** (optional) so archived email keeps its sender; the V13 note
   format is otherwise the single landing format.
