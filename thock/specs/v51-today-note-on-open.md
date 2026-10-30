# Thock V51 - Today's note exists when the day starts

**Status:** Spec (2026-10-30), implementation to follow in the same PR.
**Owner:** Diego · **Date:** 2026-10-30
**Companion docs:** `v22-inviting-templates-and-example-day.md` (the example day, never cleaned),
`v8-calendar-sync.md` §9 guard 2 (sync never creates the note; it waits for it),
`v45-reminders.md` §7.2 (the day section, written on `note_created`), `v34-vault-sync-api.md` §8
(application rules) and §8.4 (the phone's rebase and prune), `v2-invisible-git.md` §6.2 (checkpoints)

---

## 1. Summary

Today's daily note (`daily/YYYY-MM-DD.md`) is created only when the user opens Today, by click or
`thock: open today`. Until then calendar sync holds ("waiting for today's note"), the reminders day
section has nowhere to go, and a ritual that moves things "to tomorrow" writes into a day that has no
page. V51 makes the note exist when the day starts: it is created from the daily template when a
vault opens in the app and again when the date rolls over while the app runs. Since every day now
gets a page, a page nobody wrote on must not pile up: a past day whose note is still exactly the
template is removed when it stops being today.

## 2. Goals & success criteria

- **G1: Today's note is there before anyone asks for it.** Opening a vault, or crossing midnight with
  it open, leaves `daily/<today>.md` on disk, created from `templates/daily.md`, so calendar sync,
  the reminders day section and the rituals find a page to write into.
- **G2: Nothing the user wrote is ever touched.** Creation is create-if-missing through the project
  `Fs`, exactly `notes::ensure_note`. Cleanup removes a note only when it is byte-for-byte the
  template, modulo trailing whitespace and the time line (§4.2); one character of the user's keeps it.
- **G3: A word typed on the phone outlives the desk's cleanup.** A phone edit to a note the desk is
  about to clean, or has cleaned, lands anyway (§5).
- **G4: Launch stays cheap.** One stat and at most one small read per past day in the window, which
  is 30 days or the time since this service last ran, capped at 400.
- **G6: A page this service made is always tidied.** Whatever the gap between two launches, the
  template-only page of the last launch day is cleaned at the next one.
- **G5: Nothing assumes a Routine.** A vault without a daily template gets no new files and loses
  none (§4.4). `OpenToday`, `ensure_example_day` and the skills keep working unchanged.

## 3. Non-goals

- Weekly notes: `ThisWeek` keeps creating on open. A week's page spans seven days and nobody has asked
  for it to exist on Monday morning.
- Cleaning anything but daily notes in the window, or a note that is empty for any reason other than
  being the template (a 0-byte note made while the template was missing stays; §4.4).
- A setting to turn V51 off. Everything is editable: a user who wants no automatic page deletes
  `templates/daily.md` and the feature is inert (§4.4).
- Creating tomorrow's note ahead of time. Rituals that "move to tomorrow" use `ensure_note`, as the
  skills already say (`wrap-today.md` step 2).

## 4. Behavior

### 4.1 Creation

Two moments, one function:

1. **Vault open.** When a local workspace whose first visible worktree is a vault appears (`observe_new`
   on `Workspace`, plus `WorktreeAdded` for the window-restore path, as `reminder_service::init` does),
   `ensure_note(fs, vault, Daily, today, now)` runs once. `today` and `now` come from
   `reminders::ReminderClock::global`, so tests drive them with `TestClock`.
2. **Rollover.** The service subscribes to `reminders::ReminderDay::global(cx)` and runs the same
   step on every `DayChanged`. That watch already fires at local midnight, within a minute of a wake
   from sleep and on a clock change; V51 adds no timer.

After a `Created` outcome the service calls `notes::announce_note` so the reminders day section is
written now (V45 §7.2) rather than on the next file event. `AlreadyExisted` is the common case on a
restart and does nothing. The example day written by `open_startup_vault` precedes `open_paths`, so on
a fresh vault the service finds it and leaves it alone.

A missing template is not an error: `CreatedWithoutTemplate` cannot happen because the gate in §4.4
skips creation entirely. A failed write is logged and shown as a workspace error through the same
path `open_vault_note` uses; the next trigger retries.

### 4.2 What "template-only" means

A note is template-only when it is what `ensure_note` would have written for that date, allowing for
what an editor does to a file on save and for the time it was created:

1. Take the template text as it is on disk now, split into lines; take the note text, split into
   lines. Normalize both: `\r\n` → `\n`, trailing whitespace stripped from every line, trailing empty
   lines dropped.
2. The two must have the same number of lines.
3. For each pair, if the *template* line contains `{{time}}` the pair is skipped. Otherwise the
   template line is expanded with `expand_template(line, date, _, title)` (`title` is the note's file
   stem, as `ensure_note` uses) and must equal the note line.

Consequences, all intended: an unknown token (`{{weather}}`) expands to itself on both sides and
compares equal; a `### Calendar` or `### Reminders` section written by sync or the reconciler makes
the note non-template-only, so a day that had meetings or carried reminders is kept, which is what a
reader of that day wants; a template edited since the note was made no longer matches old notes, so
those stay (safe side); a 0-byte note matches only a template that expands to nothing.

The matcher is a pure function in `notes.rs`, `is_template_only(note, template, date, title) -> bool`,
unit-tested beside `expand_template`.

### 4.3 Cleanup

Runs after each creation step (launch, rollover), once per trigger:

- **Window:** every date from `min(today − 30, last_ensured)` to yesterday, each resolved with
  `vault.note_path(Daily, date)`. By path, not by directory listing, so nested filename formats
  (`YYYY/MM/DD`) cost the same and no directory walk happens. `last_ensured` is the date this
  service last ensured a note for in this vault, kept in the app's key-value store
  (`db::kvp::KeyValueStore`, the Backlog's mechanism) under `thock-today-note:<vault root>` and
  written after every successful ensure, so a page V51 made is always inside the next launch's
  window however long the app was closed. The window is capped at 400 dates, so a years-old key
  costs at most 400 stats; the 30-day floor covers pages made by `OpenToday` or the phone on days
  the service never ran. The key is read before the day's ensure rewrites it.
- **For each date:** stat; if present and no open buffer exists for it in the project (the user is
  looking at it; it is reconsidered at the next trigger), load and test §4.2; if template-only,
  `fs.remove_file` with `ignore_if_not_exists`, so a second window on the same vault racing to the
  same conclusion is harmless. The load and the remove are back-to-back on the background executor.
- **Today is never in the window.** Today's note is left alone while the day is live.
- **Restore point.** Before a round judges any page (one or more past notes in the window exist and
  are not open) the service calls `history::checkpoint_before_ai_write(project)`. The checkpoint is
  asynchronous and may stage after a delete, so recoverability rests on three things together: the note's creation was itself
  checkpointed (its file event armed the idle checkpoint; quitting first takes the close checkpoint),
  the removed content is by definition the template, and the pre-cleanup checkpoint is one more
  restore point before the day's tidy. No new history API.
- **Through `Fs`.** Deletions are ordinary vault file events: the reminders scan forgets the path, the
  history service marks the vault dirty, and vault sync tombstones the path on the server, where the
  phone drops its snapshot (`VaultStore.applyTombstone`).
- **Sync gate:** see §5.

Errors in a round are logged with `log_err` and never stop the other dates; a read or delete that
fails is tried again at the next trigger.

### 4.4 The gate: a daily template on disk

`VaultConfig` always resolves `[daily]` to its defaults, so "daily notes configured" is true of every
vault and cannot gate anything. The signal that a vault uses daily notes is the template file at
`vault.template_path(Daily)`: scaffolding writes it, the Timeline Routine's docs point at it, and
without it `ensure_note` would make empty pages. When the file is missing, V51 does nothing in that
vault, neither creating nor cleaning (the §4.2 test needs a template to compare against, and cleaning
empty pages on a different rule would be a second definition of "empty"). `OpenToday` keeps its
existing behavior on a missing template (an empty note and a toast).

Daily notes are core, not the Timeline Routine's (VISION §5.1), so the Routine's installed state is
not consulted.

### 4.5 What stays the same

- `thock: open today` and the Timeline rows: `open_vault_note` → `ensure_note` → open. The outcome is
  now almost always `AlreadyExisted`. No code change.
- `ensure_example_day` (V22): the example is prose and planner lines, never template-only.
- Skills that say "create it from the template if missing" stay correct as the fallback they are.
- `calendar_service` keeps its existence guard; it simply stops holding on most days.

## 5. Phone sync safety

The question: the desk removes `daily/D.md` because it is template-only, but the phone edited that
page and the edit has not reached the desk yet. Which wins?

**What the rules already guarantee.** The phone never writes vault files. Every phone change is a
write document queued on the server (`POST /v1/vault/writes`, which does not consult the files table,
so a tombstoned path is accepted) and applied by the desk in `vault_sync::apply_write` with
`thock_sync_core::apply` (contract §8). On the desk, a write that arrives for a path that no longer
exists is applied to `existing = None`:

- `append` with `create_from_template` (what the phone sets on every daily-note append:
  `PhoneWrites.swift`, `BacklogModel.swift`) is seeded from the template for that date
  (`vault_sync::template_seed`) and the user's line is added. The note comes back as template plus
  the words. Fixture: `fixtures/v1/append/gen-daily-template-append-file-missing-seeded.json`.
- `replace_line` and `replace_section` (V46 edit in place) find no target and apply as `kept_both`
  (`apply.rs`): the words land, marked `<!--thock:also-->`, in a note that holds the heading and
  them. Not pretty, but nothing is lost.
- `remove_line`, `remove_block`, `move_block` on a missing file are `noop`/unchanged: there was
  nothing of the user's in a template-only note to remove or move.

On the phone, a tombstone removes the local snapshot only; pending writes are pruned on ack alone
(§8.4, `VaultStore.removeFiles(notIn:)` keeps a path with a waiting write). The user keeps seeing
their edit until the desk's version of it arrives.

So the rules make the user's words win in every ordering but one: a write applied by the drain in
the instant between the cleanup's load and its remove of the same note. The drain writes with
`atomic_write` and uploads in the same job; if the remove lands after that write, the ack has already
told the phone the desk's result is final.

**Decision: defer cleanup until the session's first sync pass has settled, and keep the rules as
they are.** No new sync-core rule, no new fixture. Concretely, when the project has a
`VaultSyncService` with a session, the cleanup round waits (`cx.observe` on the service) until its
status is no longer `Starting` or `Working`: `UpToDate` means the catch-up and the drain that
`start_session` queues have both run, so every write the phone had queued while the desk was closed
is on disk before any note is judged; `Failing`, `Paused`, `PhoneNotConnected`, `Off` or `Hidden`
mean there is no drain to wait for (an offline desk's writes are applied when it is back, to a
re-created note, as above). No cap on the wait: a sync that never settles defers cleanup to the next
trigger, and waiting loses nothing. After that first pass, polls are a minute apart, and the window
is the sub-millisecond gap between two back-to-back background `Fs` calls on a day-old page that was
template-only at the desk a moment earlier. Accepted and recorded as a known limit (§9).

## 6. Changes by file

| File | Change |
| --- | --- |
| `crates/thock/src/today_note.rs` (new) | `TodayNoteService`, one per local project on a vault, attached from `init` with the `observe_new(Workspace)` + `WorktreeAdded` pattern of `reminder_service.rs`; keyed by project id in a global with `service_for_project` for tests. Subscribes to `ReminderDay` for `DayChanged`. `ensure_today` then `clean_past_days` (§4.1, §4.3, §5 gate). |
| `crates/thock/src/notes.rs` | `is_template_only` (§4.2) and `cleanup_dates(today, last_ensured) -> Vec<NaiveDate>` (the window of §4.3), both pure, both tested. |
| `crates/thock/src/thock.rs` | `pub mod today_note;` and `today_note::init(cx)` in `init`. |
| `crates/thock/src/vault_sync.rs` | Nothing functional. `PhoneSyncState` is already public; the service already `cx.notify()`s on every status change, which is what the gate observes. |
| `crates/thock/assets/routines/timeline/doc.md` | "created from `templates/daily.md` the first time" → the note is ready when the day starts; a past day left exactly as the template is tidied away. |
| `crates/thock/assets/skills/welcome-tour.md` | Step 4: today's note is already there; the "doesn't exist" fallback stays for a vault without a template. |
| `thock/VISION.md` | §5.1 (line 59): the note is created when the day starts, not on the click; §12 roadmap entry. |
| `thock/site/public/index.html` | The vault tree caption "today, created on first keystroke" → "today, ready when the day starts". Merging deploys the site; the PR says so. |

Not touched: `calendar_service.rs`, `reminder_day.rs`, `history.rs`, `routines_panel.rs`,
`thock-sync-core`, the phone.

## 7. Tests

All GPUI tests in `today_note.rs` use `FakeFs`, `Project::test`, `TestClock::install` (the
`reminders.rs` clock) and `ReminderDay::global`, following `reminder_service.rs`'s `setup`.
Advancing the executor's clock past midnight fires `DayChanged`.

| # | Test | Asserts |
| --- | --- | --- |
| 1 | `a_vault_opens_with_todays_note` | Attach on a vault with a template and no notes: `daily/<today>.md` exists and equals `expand_template(template, today, now, stem)`. |
| 2 | `an_existing_note_is_never_touched` | Today's note holds "user edits"; attach; bytes unchanged. Also with `ensure_example_day`'s text: untouched. |
| 3 | `midnight_makes_the_new_day_and_tidies_the_old` | Clock at 23:59:30, today's note template-only; advance 60 s: tomorrow's note exists, yesterday's is gone. |
| 4 | `one_line_of_the_users_keeps_the_note` | Yesterday's note is the template plus `- Called Ana`; attach: kept. Same with a `### Calendar` line. |
| 5 | `a_time_line_does_not_count` | Template has `Started {{time}}`; yesterday's note has a different time: removed. |
| 6 | `the_window_is_thirty_days_without_a_record` | No key yet; template-only notes 30 and 31 days back: the first goes, the second stays. |
| 6b | `a_page_this_service_made_is_tidied_after_any_gap` | Attach on day D (note made, key written); detach; attach on D + 45: D's template-only page is gone, D + 45's exists, the key reads D + 45. |
| 7 | `an_open_note_waits` | Yesterday's template-only note has an open buffer: kept this round. |
| 8 | `no_template_means_nothing_happens` | No `templates/daily.md`; an empty note two days back: nothing created, nothing removed. |
| 9 | `cleanup_waits_for_the_first_sync_pass` | The real `VaultSyncService` against `vault_sync.rs`'s fake server, a phone `append` queued for yesterday's template-only note: after the session's first pass, yesterday's note holds the line and survives cleanup; the catch-up has tombstoned nothing. |
| 10 | `notes::is_template_only` unit cases | CRLF, trailing blank lines, trailing spaces, a `{{time}}` line, an unknown token, an extra character, a 0-byte note against a non-empty template. |
| 11 | `notes::cleanup_dates` | 30 dates without a record; back to the record when it is older; 400 at most; never today; oldest first. |

`routines_panel.rs`'s existing `OpenToday` tests keep passing (the outcome is `AlreadyExisted` after
attach). `cargo test -p thock -j 2 today_note notes::` while iterating; `thock/script/test` before
the PR.

## 8. Build order

1. `notes.rs`: `is_template_only`, `cleanup_dates`, tests (#10, #11).
2. `today_note.rs`: service, attach, `ensure_today`, the key-value record, tests #1–#3.
3. Cleanup with the window, the open-buffer skip and the checkpoint call; tests #4–#8 and #6b.
4. The sync gate; test #9.
5. Docs: the Routine page, the tour, VISION §5.1 and §12, the site caption.
6. `cargo fmt --all -- --check`, the scoped clippy line, `thock/script/test`, PR.

## 9. Decisions (2026-10-30)

| # | Decision | Why |
| --- | --- | --- |
| 1 | Gate on the daily template file, not on config or the Timeline Routine. | Config always resolves; daily notes are core (VISION §5.1); without a template the created page would be empty and "template-only" would need a second definition. |
| 2 | Template-only compares line by line with `{{time}}` lines skipped, trailing whitespace and blank lines ignored. | Save-time editor normalization and the creation time are the only legitimate differences. Everything else is the user's. |
| 3 | Clean when a day stops being today: at launch and on `DayChanged`, never during the day. | Diego's call. A live day is one the user may still write on. |
| 4 | Window of 30 days or back to the last ensured date (key-value store), capped at 400, by path, no directory walk. | G4 and G6: a fixed window would leave the page of a launch 45 days ago untidied, which contradicts "empty notes are always cleaned"; a template-only note older than both bounds is the pre-V51 state, harmless and visible. |
| 5 | Reuse `ReminderDay`/`ReminderClock`; no second timer. | One clock in the app; tests drive it the same way the reminders tests do. |
| 6 | Cleanup waits for the sync session's first pass; no new sync-core rule. | §5: the application rules already re-create a cleaned note around the user's words; the only gap is a race inside one round, which the wait closes for the case that matters (writes queued while the desk was closed). |
| 7 | Checkpoint before the round with the existing `checkpoint_before_ai_write`; no awaitable checkpoint API. | The removed text is the template and its creation was already checkpointed; an awaitable variant would fight the service's coalescing for a benefit no one can see. |
| 8 | Skip a past note that has an open buffer. | Deleting a file under a reader is rude; the next trigger reconsiders it. |
| 9 | Weekly notes unchanged. | Not asked for; a week's page has no "day starts" moment. |
| 10 | No setting. | Deleting the template turns it off; a setting would be hidden behavior with a file already doing the job. |

**Known limit.** A phone write applied by a poll in the sub-millisecond gap between the cleanup's read
and its remove of the same day-old note is lost on the desk (its ack tells the phone the desk's result
is final). Closing it fully would mean serializing cleanup through the sync worker; not worth the
coupling for a window that small on a page that was template-only moments before.
