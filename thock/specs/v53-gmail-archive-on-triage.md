# Thock V53 - Archive in Gmail: triage once, both inboxes agree

**Status:** Shipped (2026-11-02): merged as #179
**Owner:** Diego · **Date:** 2026-11-02
**Companion docs:** `v15-unified-gmail-sync.md` (the `thock/inbox` mapping, `.thock/gmail.toml`,
`imported.jsonl`), `v13-inbox-routine.md` §9.5 (the triage-log format), `v40-phone-inbox-gestures.md`
§5 (phone decisions and their log line), `v41-inbox-desk-view.md` §6 (desk writes and undo),
`v32-sync-status-indicator.md` (`ConnectorStatus`), `v9-gmail-backlog-capture.md` §6 (the consent)

---

## 1. Summary

An email labelled `thock/inbox` lands as a note in `inbox/` and gets triaged in Thock, but it is
still in the Gmail inbox, so the user triages it twice. V53 lets Thock archive the thread in
Gmail when the Thock decision is one the user picked.

Thock is read-only toward Google today, and some users want it to stay that way. So this feature
is **opt-in**: connecting Google stays read-only, and a separate action asks for `gmail.modify`
through incremental consent when the user turns it on. Which decisions archive in Gmail (Today,
Soon, Someday, Archive, Discard, anything else) is a list in `.thock/gmail.toml`.

The trigger is the **triage log**, not a folder. Every path that decides an inbox item (the desk
Inbox view, a phone swipe, the Triage Inbox ritual) appends one line to
`archives/inbox/triage-log.md` carrying the destination and the item's capture digest. The desk
Gmail service reads new lines, resolves digests to threads through `imported.jsonl`, and removes
the thread's `INBOX` label. Undo, which removes the log line, puts `INBOX` back.

## 2. Goals & success criteria

- **G1: Opt-in, and read-only until then.** A vault that never runs the opt-in action requests the
  same three read-only scopes as today and never sends a write to Google. No migration, no new
  prompt on upgrade.
- **G2: Every decision path, one rule.** A desk keystroke, a phone swipe and a ritual decision
  archive in Gmail under the same configuration, because all three are read from the same log.
- **G3: Gmail archive only.** Thock removes the `INBOX` label from the thread, nothing else. The
  `thock/inbox` label, read state, and every other label are untouched. Nothing is ever deleted
  or trashed in Gmail.
  _Amended by V56 §7.1 (2026-11-11):_ the same `gmail.modify` grant also sends a reply the person
  writes and confirms in Thock. Archiving is still all that happens without a keystroke.
- **G4: Not retroactive.** Turning it on acts only on decisions made afterwards. Mail already
  triaged before the opt-in stays where it is in Gmail.
- **G5: Undo reaches Gmail.** Undoing a decision (desk `u`, the toast, the phone's undo) before the
  write is sent cancels it; after it is sent, puts `INBOX` back.
- **G6: Never silent.** A failed or refused write shows in the Gmail connector row and the status
  popover with a Retry, and is retried on the next poll. A missing permission is a visible
  holding state, never a loop of 403s.
- **G7: Gmail moves stay with the user.** Once Thock has archived a thread it never archives it
  again. If the user moves it back to the inbox in Gmail, it stays there.

## 3. Non-goals

- **Other Gmail writes.** No mark-as-read, no label changes, no reply, no trash. Each would be its
  own opt-in.
- **The `thock/backlog` mapping.** Only threads captured by the mapping whose `path` is the Inbox
  Routine's `dir` are eligible. Backlog captures never go through triage.
- **Phone-side Google access.** The phone never holds a Google token. Phone decisions reach Gmail
  when the desk next polls (§7.4).
- **Reading Gmail state back.** Thock doesn't watch for the user un-archiving in Gmail. G7 is met by
  remembering what was done, not by polling labels.
- **A per-scope revoke.** Google's revoke endpoint drops the whole grant. Turning the feature off
  stops the writes; getting rid of the permission means reconnecting (§5.3).
- **Inbox view changes.** No new keys, chips or toast copy in V53.

## 4. Core concepts

### 4.1 The log is the trigger

`archives/inbox/triage-log.md` is already the append-only receipt of every inbox decision, in one
format (V13 §9.5) that the desk, the phone and the ritual all write:

```markdown
- 2026-11-02 · Re: Q4 planning doc → Backlog · Soon <!--inbox:4d1f9a02c7b3-->
```

The marker carries the note's `capture:` digest, the same digest V15 writes to
`.thock/state/gmail/imported.jsonl` next to the thread id. So one line gives *which thread* and
*what was decided*, even when the ritual deleted the note. A line without a marker (a hand-written
item) or with a digest that isn't in `imported.jsonl` for the inbox mapping is not a Gmail item
and is ignored.

### 4.2 Decision classes

The destination text after `→` maps to one class. `Today…` matches by prefix; the Backlog
destinations match the vault's configured `[backlog.headings]`, because the desk and the phone
write `Backlog · <heading>`, not the defaults:

| Destination text | Class | Written by |
| --- | --- | --- |
| `Today…` | `today` | desk, phone, ritual |
| `Backlog · Soon` | `soon` | desk, phone, ritual |
| `Backlog · Someday` | `someday` | desk, phone, ritual |
| `Archived` | `archive` | desk, phone |
| `Discarded` | `discard` | ritual |
| anything else (kept as a note, appended to a note) | `other` | ritual |

The classifier lives in `thock_sync_core::inbox` next to `triage_log_line`, so the strings it reads
and the strings the writers produce are pinned by the same fixtures.

### 4.3 Permission and setting are separate

The setting says *what* to archive; the grant says Thock *may*. The setting is a plain file the
user can edit (product invariant: everything is editable). The grant is checked on every token
refresh: Google's refresh response carries the granted `scope` string. A setting without the
grant is a holding state with an **Allow** action (§8), never an error loop.

## 5. What the user does

### 5.1 Turning it on

`thock: enable gmail archive` (`thock::EnableGmailArchive`, also the **Allow** button of the Gmail
row while the permission is missing):

1. Opens Google's consent for the three read-only scopes plus `gmail.modify`, with
   `include_granted_scopes=true`; Google prompts only for what is new. Asking for `gmail.modify`
   alone was tried first and returned tokens without the earlier scopes, which can't be stored
   without cutting off calendar and inbox sync. A grant missing any read-only scope is refused
   and nothing is stored.
2. If the granted `scope` comes back without `gmail.modify` (the user unticked it), nothing is
   written and a toast says so: *Gmail archive stays off: Google wasn't given the permission*.
   A consent given for another Google account than the vault's is refused the same way.
3. Otherwise stores the new refresh token, writes `[archive]` into `.thock/gmail.toml` with the
   default `on = ["archive"]` (an existing non-empty `on` is kept), and rebuilds the transport
   so it reads the new grant. The next pass records the baseline (§7.2).
4. Toast: *Gmail archive is on · Edit which decisions* (opens `.thock/gmail.toml` at `[archive]`).

### 5.2 Choosing which decisions

Edit `on` in `.thock/gmail.toml` (§6). Changes apply from the next poll and, like the opt-in, are
not retroactive: newly-added classes apply only to lines written after the change.

### 5.3 Turning it off

`thock: disable gmail archive` (`thock::DisableGmailArchive`) removes the `[archive]` table (or
the user deletes it by hand; same effect) and drops anything still pending. The toast offers
**Reconnect read-only**, which revokes the grant and runs today's connect flow, for users who
want the permission itself gone.

## 6. Configuration — `.thock/gmail.toml`

```toml
schema = 2

[archive]
# Which inbox decisions also archive the email in Gmail.
# Any of: today, soon, someday, archive, discard, other.
on = ["archive"]
```

- No `[archive]` table, or an empty `on`, means off. That is the default.
- Unknown class names are dropped with a logged warning; the rest still apply.
- An unparseable `[archive]` disables only this feature, with a `Failing` row naming the file.
  Capture keeps running (V15's per-concern isolation).
- `schema` stays 2. `[archive]` is an additive table that older builds ignore as an unknown key.

## 7. Architecture

```
thock_sync_core/inbox.rs  decision_class(), parse_triage_log_line()   (shared, fixtured)
gmail.rs                  ArchiveConfig, ArchiveState, plan_archive() — pure reconciler
gmail_google.rs           MailTransport::modify_thread(), archive_permission()
google_auth.rs            api_post_json, incremental consent, granted-scope check, revoke
gmail_service.rs          runs the reconciler each poll, owns the state file, the two actions
sync_status.rs            the Gmail row's archive states
```

### 7.1 The reconciler (pure)

```rust
pub fn plan_archive(
    log: &[LoggedDecision],         // digest + class of each marked line
    imported: &ImportedIndex,       // digest → (thread id, mapping path)
    state: &ArchiveState,           // last mark per digest, last `on`
    on: &BTreeSet<DecisionClass>,
    inbox_dir: &str,
    now: DateTime<Utc>,
) -> ArchivePlan                    // records to append + Archive/Unarchive writes
```

- **Archive:** a line whose digest is not in the baseline or `applied`, whose thread was captured
  by the inbox mapping, whose class is in `on`, and that has been seen for at least the **grace
  period** (60 s, so the desk's undo toast usually cancels before anything is sent).
- **Unarchive:** a digest in `applied` whose line is gone from the log, and which was applied less
  than the **undo horizon** ago (24 h). Removing old lines when tidying the log by hand never
  touches Gmail.
- **Withdrawn:** a digest still waiting out the grace whose line is gone. If it is decided
  again, the grace starts over.
- A digest Thock unarchived may be archived again by a new decision for it.
- Everything else does nothing.

"Seen for" is measured from when the service first saw the line (recorded in `pending`), because
log lines carry a date but no time.

### 7.2 State — `.thock/state/gmail/archive.jsonl`

Append-only, one record per event: `{"digest", "thread", "event": "baseline" | "seen" | "archived"
| "unarchived" | "withdrawn" | "failed", "at", "error"?}`. The last event per digest is its state,
except `failed`, which is a receipt and leaves the mark so the next pass retries. One more record,
`{"event": "on", "classes", "at"}`, remembers which classes were on, so a class added while Thock
was closed is still baselined. Machine state,
so it lives under `.thock/state/` and is rebuildable: losing it costs at most one redundant
`INBOX` removal, which Gmail treats as a no-op.

The **baseline** is written by the first pass after opt-in and whenever a class is added to `on`:
every digest of that class already in the log is recorded as `baseline`. Turning the feature off
records an empty `on`, so turning it on again baselines what was pending. That's G4.

### 7.3 The write

`POST https://gmail.googleapis.com/gmail/v1/users/me/threads/{id}/modify` with
`{"removeLabelIds": ["INBOX"]}` (unarchive: `addLabelIds`). Thread-level, matching V15's one
note per thread. A 404 (thread deleted in Gmail) is recorded as `archived` and never retried.
A 401 takes the existing retry-behind-refresh path. A 403 `insufficient` permission moves the
feature to the holding state; it doesn't disconnect capture, which still works on the read-only
scopes.

### 7.4 When it runs

On every Gmail poll (V15's cadence, `poll_seconds`), after the apply pass, and on `SyncGmailNow`.
The desk view's decision and undo also call `GmailService::reconcile_soon()`, which schedules one
pass after the grace period instead of waiting for the next poll. Phone decisions arrive through
vault sync and are picked up on the next poll after they land. If the desk is closed, they wait.

### 7.5 Consent changes

- `SCOPES` stays the three read-only scopes. `GMAIL_MODIFY_SCOPE` is a separate constant.
- `build_auth_url` gains `include_granted_scopes=true` and a scope list parameter.
  `connect_workspace` asks for `SCOPES` plus `gmail.modify` when `[archive]` is on, so
  reconnecting after a lost sign-in doesn't quietly drop the permission.
- The token refresh keeps the response's `scope` and exposes `has_scope(GMAIL_MODIFY_SCOPE)`.
  Nothing new goes into the keychain or the vault.

## 8. Status

The Gmail connector row (V32) gains three states, shown only when `[archive]` is on:

| State | Row text | Action |
| --- | --- | --- |
| Setting on, permission missing | *Gmail archive needs permission* | **Allow** → `EnableGmailArchive` |
| Writes failing | *Couldn't archive 2 emails in Gmail* | **Retry** → `SyncGmailNow` |
| Healthy | (no row: V32 keeps rows only while the user has to act) | — |

The popover's Gmail line shows *archiving in Gmail* as a quiet suffix while the feature is on,
so the user can always see whether Thock is writing.

## 9. Product invariants

- **Human-in-the-loop where it can't be undone.** Archiving is reversible in Gmail and in Thock (G5).
  The opt-in action, with its own Google consent screen, is the standing confirmation for a write
  that leaves the vault, and it covers only the decision classes the user listed.
- **Your files, forever.** Nothing about the vault changes. The note, the log line, and the task
  are what they were in V41/V40.
- **Modular life.** No Inbox Routine, no log, nothing to reconcile. No Gmail connection, no feature.
- **No "git" in the UI** is unaffected.

## 10. Tests

### 10.1 Shared core (`thock_sync_core`)
- `decision_class` for every destination string in the V40/V41 fixtures and the ritual's §6
  examples; unknown text → `other`.
- `parse_triage_log_line` round-trips `triage_log_line`, ignores marker-less and malformed lines.

### 10.2 Reconciler (`gmail.rs`, pure, injected `now`)
- Not retroactive: digests present at baseline never produce an action.
- Grace: a line seen 59 s ago → nothing; 60 s → `Archive`.
- Class filter: `on = ["archive"]` ignores a `soon` line; adding `soon` later baselines existing
  `soon` lines.
- Backlog-mapping digests and unknown digests are ignored.
- Undo: an applied digest whose line disappears within 24 h → `Unarchive`; after 24 h → nothing.
- Undo inside grace: line appears and disappears before 60 s → no action ever.
- G7: an `archived` digest whose line is still present never produces a second `Archive`.

### 10.3 Transport and auth
- `modify_thread` sends the exact body and path; 404 is success-and-done; 403 `insufficient` →
  the holding error.
- `build_auth_url` carries `include_granted_scopes=true`; the opt-in URL asks for every
  read-only scope plus `gmail.modify`, and a grant that comes back with `gmail.modify` alone is
  refused; `connect_workspace` includes it only when `[archive]` is on.
- Granted-scope parsing from a refresh response with and without `gmail.modify`.

### 10.4 Service (`gmail_service.rs`, stub transport)
- A desk Archive decision → one `modify_thread` call after the grace period, `archived` recorded.
- Desk `u` after the write → one unarchive call.
- A failing write surfaces the *Couldn't archive* row and retries on the next poll.
- No `[archive]` → zero write calls across a full poll with log lines present.

### 10.5 Actions
- `EnableGmailArchive` / `DisableGmailArchive` write and remove `[archive]` without touching
  the rest of `.thock/gmail.toml` (insert/remove the table, never rewrite the whole file).
  Settings-touching tests use a temp vault, never the real config dir.

## 11. Changes by file

| File | Change |
| --- | --- |
| `crates/thock-sync-core/inbox.rs` | `decision_class`, `parse_triage_log_line`, fixtures |
| `crates/thock/src/gmail.rs` | `ArchiveConfig` parsing, `plan_archive`, `ArchiveState` |
| `crates/thock/src/gmail_google.rs` | `modify_thread` on `MailTransport` |
| `crates/thock/src/google_auth.rs` | `api_post_json`, scope parameter, `include_granted_scopes`, granted scope on refresh |
| `crates/thock/src/gmail_service.rs` | reconcile after each poll, `reconcile_soon`, state file |
| `crates/thock/src/inbox_view.rs` | call `reconcile_soon` after a decision and after undo |
| `crates/thock/src/sync_status.rs` | the §8 states |
| `crates/thock/src/gmail_service.rs` | `EnableGmailArchive`, `DisableGmailArchive` actions, next to `SyncGmailNow` |
| `crates/thock/src/calendar_service.rs` | the connect flow asks for `gmail.modify` when `[archive]` is on |
| `crates/thock-sync-core/fixtures/v1/triage_log.json` | parse and class vectors |
| `crates/thock/assets/routines/inbox/doc.md` | one paragraph: what turning it on does |

No inherited crate changes. No keymap changes: both actions are palette-only, like the other
connect actions.

## 12. Open questions

1. **Default `on`.** Kept at `["archive"]`, the original ask. `["archive", "discard"]` would cover
   the ritual's equivalent too.
2. **Grace period.** Resolved: the decision toast autohides after 5 s, so 60 s covers it. Desk
   `u` stays available until the next decision; an undo after the write is sent still reaches
   Gmail as an unarchive.
3. **Google verification.** `gmail.modify` is a restricted scope, the same tier as the
   `gmail.readonly` already requested. Confirm the bundled client's verification covers adding it
   before this ships.

## 13. Decisions (2026-11-02)

1. Opt-in through a separate action and incremental consent, not a checkbox at connect.
2. Which decisions archive is configurable per class.
3. "Archive in Gmail" removes `INBOX` only; `thock/inbox` stays as the record of capture.
4. The desk reconciles every path; the phone never talks to Google. The trigger is the triage log
   rather than `archives/inbox/`, because the ritual deletes filed notes instead of moving them.
