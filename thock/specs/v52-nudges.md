# Thock V52 - Nudges: a desk agent that remembers the routine

**Status:** Approved (2026-10-30); implementing
**Owner:** Diego · **Date:** 2026-10-30
**Companion docs:** `v44-passing-notes.md` §5.8 (conversations Thock starts: the bubble, the pause
rule, the `openers` setting) and §6.4 (the sketch this spec makes real), `v28-agent-memory.md`
(the Reflect opener, the first one), `v7-dynamic-routines.md` §7.5 (`thock::RunSkill { skill }`),
`v45-reminders.md` §9 (the notifier's clock), `v17-backlog-categories.md` §5.4 (view state in the
key-value store)

---

## 1. Summary

Thock knows the rituals, and it already knows how to start a conversation (V44 §5.8), but today
the only conversation it starts is Reflect's. The rituals that keep a vault honest, wrapping a
day and reviewing a week, still depend on the person remembering them.

V52 adds **nudges**: canned, time-and-day-triggered offers from the Thock Agent, shown as V44
openers, each with two chips: **Go for it**, which runs the ritual through the existing
`thock::RunSkill` path, and **Not now**, which snoozes it for the day. There is no model call
until the person accepts. A nudge is evaluated at coarse moments only (vault open, the top of
each hour, which includes midnight) and never on a file event, so it reads like a colleague who
remembers the routine, never like something watching the notes.

Two nudges ship: **Wrap**, when the last day isn't closed, and **Week Review**, from Friday
afternoon until the week is reviewed. They are entries in a small registry, so the next
time-based intervention (a month's start, a long gap since Reflect) is one entry, not a subsystem.

## 2. Goals & success criteria

- **G1: Right moment, fixed words.** A nudge is deterministic: the same vault at the same time
  always yields the same offer, with copy about the ritual, never about what was read in a note.
- **G2: Never surveillance.** Evaluation happens at coarse moments only (§4.3), does one cheap
  structural check (does a note exist, does it carry a heading), and is never triggered by a
  vault file event. A nudge can't appear in reaction to something you just typed.
- **G3: One at a time, bounded per day.** At most one nudge is pending, at most two are offered
  in a day, and **Not now** keeps a nudge away until tomorrow, across restarts.
- **G4: Modular life.** A nudge exists only when the skill it offers is installed and enabled in
  this vault. No Timeline Routine, no nudges.
- **G5: Keyboard-complete.** Accepting, dismissing and reaching a nudge work without the mouse,
  through named `thock::` actions in the palette and the keymaps (CLAUDE.md "Panels").
- **G6: Accepting is the ritual.** **Go for it** is exactly `thock::RunSkill { skill }`: the same
  launch, name, model tier and hosted/BYO routing as the Routines rail.

## 3. Non-goals

- **Live observation.** No reaction to edits, saves, file events or the caret. No "you've been
  writing for an hour" prompts.
- **A model call before acceptance.** Copy is fixed text; the agent is never asked whether to
  nudge or what to say.
- **Notifications outside the app.** No system notifications, no phone push, no watch. A nudge
  is a bubble in the desk's chat, and only while the app is open.
- **A BYO surface.** Nudges render in the hosted chat (V44 decision 15). A person who chose their
  own terminal agent (`mode = Byo`) has no chat and gets no nudge in V52; a banner in the Agent
  panel is a follow-up (§10).
- **Per-nudge settings.** `"openers": "off"` silences every opener, nudges included (V44 decision
  17). Finer control waits until someone asks for it.
- **Nudges about content** ("three tasks are overdue"). Conditions are structural only.

## 4. The framework

### 4.1 Data model (`crates/thock/src/nudges.rs`)

```rust
/// A nudge in the registry: what it offers, when, and what accepting runs.
pub struct Nudge {
    pub id: &'static str,                   // "wrap-yesterday", "week-review"; also the ledger key
    pub skill: &'static str,                // the Routine skill id `thock::RunSkill` resolves
    pub due: fn(&Moment, &VaultSnapshot) -> Option<SharedString>,  // the copy, when due
}

/// The local time the evaluation runs at. Built from `chrono::Local` in the
/// app and by hand in tests.
pub struct Moment { pub today: NaiveDate, pub time: NaiveTime }

/// One cheap structural reading of the vault, taken once per evaluation.
pub struct VaultSnapshot {
    pub skills: HashSet<String>,                 // enabled Routine skill ids
    pub daily: BTreeMap<NaiveDate, NoteFacts>,   // today-3 ..= today-1, existing notes only
    pub week: Option<NoteFacts>,                 // the reviewed week's note (§5.2), if it exists
    pub week_days: BTreeSet<NaiveDate>,          // days of the reviewed week with a daily note (stats only)
}
pub struct NoteFacts {
    pub headings: HashSet<String>,   // level-1 headings, trimmed
    pub template_only: bool,         // the note is the daily template and nothing else (V51)
}

/// An offer the chat shows.
pub struct NudgeOffer { pub id: &'static str, pub skill: &'static str, pub text: SharedString }

pub const REGISTRY: &[Nudge] = &[WRAP_YESTERDAY, WEEK_REVIEW];   // priority order
```

`VaultSnapshot::read(fs, vault, today)` runs on the background executor through the project
`Fs`: it lists the enabled manifests (`routines::enabled_routine_manifests`), loads at most three
daily notes, one weekly note and the daily template by their configured paths
(`Vault::note_path`, `Vault::template_path`), keeps only the level-1 headings, and marks a daily
note `template_only` with V51's `notes::is_template_only` (the template expanded for that date,
`{{time}}` lines skipped). For the reviewed week it only *stats* each day's note path (at most
seven), never loading them. It never walks the vault. A missing note is simply absent.

`due` is a pure function. The registry is a const slice, so adding a nudge is one `Nudge` value
and one line in `REGISTRY`.

### 4.2 Policy (`Ledger`)

```rust
pub struct Ledger {
    snoozed: BTreeMap<String, NaiveDate>,       // id -> the day it was answered
    offered: BTreeMap<NaiveDate, Vec<String>>,  // the ids offered on a day
}
```

`Ledger::pick(&self, offers: Vec<NudgeOffer>, today, pending: Option<&str>) -> Option<NudgeOffer>`:

1. Drop any offer whose id is snoozed today.
2. If an offer's id is the opener already pending, return nothing: it's still up (or folded into
   the pebble's dot, where `cmd-?` brings it back).
3. Count the ids offered today. If an offer's id is among them it may be re-offered without
   counting (a restart or a Reflect opener that replaced it); otherwise, if the count is already
   **2**, return nothing.
4. Return the first remaining offer in registry order and record it under today.

**Go for it** and **Not now** both snooze the id for today: accepting means the ritual is running
(or the person can finish it by hand), and either way the offer doesn't come back the same day.
Left alone, a bubble folds into the dot after 60 seconds (V44 §5.8) and is not snoozed; the next
tick finds it still pending and leaves it there (rule 2).

Days other than today are pruned from both maps on write.

### 4.3 Evaluation moments

The chat (`ChatPanel`) owns one `nudge_tick: Option<Task<()>>` and evaluates at:

1. **Vault open:** when `refresh_vault_status` moves to `VaultStatus::Valid` (that is the mount at
   window open, and a worktree change to another vault).
2. **The top of each hour:** a GPUI timer to the next local `HH:00:05`, re-armed after every run.
   The `00:00` tick is the day rollover. A laptop asleep across a tick fires it late, which is
   fine: the next arm re-aligns.
3. Nothing else. File events, edits, focus changes and saves never evaluate (G2).

An evaluation is skipped entirely, without reading the vault, when `openers` is `off` or the chat
is hidden (an explicit BYO mode, or not a vault). Otherwise it reads the snapshot in the
background, builds `Moment::now()` from `chrono::Local`, runs every registry entry whose `skill`
is in `snapshot.skills`, lets the ledger pick, and hands the pick to `offer_opener` as
`Opener::Nudge(offer)`. The V44 rules then apply unchanged: eight quiet seconds, never over a
question or a modal, folds after a minute, `escape` answers Not now.

**Quiet hours.** Nothing is due before **06:00** local. At 01:00 "yesterday" is still tonight,
and a bubble in the small hours reads as a machine, not a colleague.

### 4.4 Persistence

The ledger lives in the app's key-value store (`db::kvp::KeyValueStore`), the mechanism the
Backlog uses for its collapsed groups, under the key `thock-nudges:<vault root>`, as JSON. It is
loaded when the vault opens and written on every change in a background task. Nothing is
written in the vault: `.thock/state/` is checkpointed by the invisible history, and a file that
changes daily would add noise to every checkpoint, while a snooze is desk-side state anyway.

### 4.5 Gating

A nudge runs only when its `skill` id is among the vault's enabled Routine skills. That is the
same lookup `run_skill_by_id` does, so an offer never reaches a skill the rail wouldn't launch.
Disabling or removing the Timeline Routine ends both shipped nudges at the next evaluation.

## 5. The two nudges

### 5.1 Wrap (`wrap-yesterday`)

- **Skill:** `wrap-yesterday` (Timeline Routine). It appends `# Daily Closure`.
- **The day:** the most recent date in `today-1 ..= today-3` whose daily note exists and is
  more than the template. Three days, so a Monday asks about Friday and a week away asks about
  nothing. A template-only note is not a day: V51 creates today's note at every launch and
  removes past template-only notes at launch and at midnight, so a day nobody wrote in is
  skipped over, never offered, and never wrapped by a nudge about to lose its page.
- **Due when:** that note has no `# Daily Closure` heading, and it is 06:00 or later. If the
  most recent such note is already wrapped, nothing is due; older gaps aren't worth an
  interruption.
- **Copy:** *It looks like yesterday wasn't wrapped up. Should I go for it?* when the day is
  `today-1`; otherwise the weekday is named: *It looks like Friday wasn't wrapped up. Should I go
  for it?*
- **Chips:** **Go for it** / **Not now**.
- **Accepting dispatches** `thock::RunSkill { skill: "wrap-yesterday" }`.
- **Skill text:** `wrap-yesterday.md` says the target is "today minus one day", so a Monday run
  would create an empty Sunday. Its first bullet changes to: *the most recent day before today
  whose daily note has anything beyond the template, looking back at most three days (on a
  Monday that is usually Friday); with none, yesterday*. The nudge and the ritual then agree on
  the day, by hand or by nudge.

### 5.2 Week Review (`week-review`)

- **Skill:** `week-review` (Timeline Routine). It appends `# AI Week Review` to the weekly note.
- **The window:** Friday from **15:00** local through Sunday, and Monday before **12:00**. A
  person who first opens the app on Saturday morning still gets the offer; one who closes the
  week on Monday morning does too. From Monday noon the week is history and the rail is the way.
- **The reviewed week** (`reviewed_week(today)`): the ISO week containing today on Friday,
  Saturday and Sunday; the previous ISO week on Monday. Weekly notes are keyed by their Monday
  (`notes::TimelineEntry`, `Vault::note_path(NoteKind::Weekly, monday)`).
- **Due when:** inside the window, the reviewed week's note is missing or lacks `# AI Week
  Review`, and `week_days` has at least one day: a daily note exists somewhere in the reviewed
  week (Monday to Sunday, or to today). A week with no notes gets no review and no nudge.
- **Copy:** Friday to Sunday: *The week's winding down. Should I run the Week Review?* Monday:
  *Last week never got its review. Should I run it now?*
- **Chips:** **Go for it** / **Not now**.
- **Accepting dispatches** `thock::RunSkill { skill: "week-review" }`.
- **Skill text:** `week-review.md` step 1 says "Monday–Sunday of the week before today", which on
  a Friday would review the previous week. It changes to: *the week being closed: the ISO week
  that contains today when today is Friday, Saturday or Sunday; the previous ISO week otherwise*.
  Same definition as the nudge's.

Registry order is Wrap, then Week Review: on a Monday morning the day comes first and the week
follows at the next tick, within the cap of two.

## 6. The UI

### 6.1 Where and what

A nudge is a V44 opener in the hosted chat: `Opener::Nudge(NudgeOffer)` beside `Opener::Reflect`.
It renders as the opener does today (`render_opener`): the *Thock · 09:12* caption, Thock's
bubble in Petrona with the copy, and two chips under it. Nothing is underlined, because a nudge
is about a ritual, not a line.

```
                                   Thock · 09:12
                         ╭───────────────────────────────────────────╮
                         │ It looks like Friday wasn't wrapped up.   │
                         │ Should I go for it?                       │
                         ╰───────────────────────────────────────────╯
                          ( y  Go for it )  ( n  Not now )            (T)•
```

The pause rule, the dot, the linger and the thread view are V44's and don't change. While a
nudge is up or folded into the dot, the Routines rail is untouched; once accepted, the ritual's
row shows the amber dot as any running ritual does (V44 §5.13).

### 6.2 Keys and actions

Two new actions in `chat_panel.rs`, `thock` namespace, documented for a note-taker:

| Action | Doc | Does |
| --- | --- | --- |
| `thock::AcceptNudge` | "Go for it: run the ritual the Thock Agent just offered." | Snoozes the id for today, folds the bubble, dispatches `thock::RunSkill { skill }` on the workspace, deferred (the Panels rule) |
| `thock::DismissNudge` | "Not now: keep the Thock Agent's offer away until tomorrow." | Snoozes the id for today, folds the bubble, focus back to the page |

Both act whenever a nudge is pending, floated or folded, so the palette reaches it either way.
The chat's key context adds `nudge` while a nudge's chips take keys (`opener_takes_keys`), next to
the existing `choice` and `choice_pair`.

| Keys | Where | Does |
| --- | --- | --- |
| `y` / `n` | `ThockChatPanel && nudge` | `AcceptNudge` / `DismissNudge` (defined after the `choice_pair` block in each keymap, so they win for a nudge; Reflect keeps `AnswerChatChoice`) |
| `1` / `2` | `ThockChatPanel && choice` | as every pair of chips |
| `left`/`right`, `h`/`l` | chat | move the highlighted chip |
| `enter` | chat, `menu::Confirm` | the highlighted chip |
| `escape` | chat, `menu::Cancel` | Not now (V44 §5.8) |
| `cmd-?` / `ctrl-?` | anywhere | focus the bubble, or bring a folded nudge back |

Keymap changes land in `default-macos.json`, `default-linux.json` and `vim.json` together.

### 6.3 How acceptance launches the skill

`AcceptNudge` dispatches `RunSkill { skill: Some(id) }` through the workspace's focus handle, so
`run_skill_by_id` resolves the enabled Routine's skill (name, file, tier) and
`AgentPanel::launch_in_workspace` routes it to the chat when hosted. The nudge module never builds
a `LaunchRequest` itself (G6). A skill that isn't enabled any more gets `run_skill_by_id`'s
existing toast, never a panic.

## 7. Adding a nudge

1. Add a `Nudge` value in `nudges.rs` with an id, the skill id it offers, and a `due` function
   over `Moment` and `VaultSnapshot`. Copy lives in `due`, so it can name a day.
2. If the condition needs a reading the snapshot doesn't carry yet (a monthly note, Reflect's
   marker), add the field to `VaultSnapshot` and read it in `VaultSnapshot::read`, keeping it to
   fixed paths, never a walk.
3. Append it to `REGISTRY` in priority order.
4. Add its due-logic tests (§8.1). No change to the chat, the keymaps or the ledger.

## 8. Tests

### 8.1 Due logic (`nudges.rs`, pure, injected `Moment` and a hand-built `VaultSnapshot`)

- Friday 16:00, this week's note without the heading, a daily note in the week: Week Review due,
  Friday copy. Friday 14:00: not due. Saturday 09:00 and Sunday: due. Monday 09:00 with last
  week unreviewed: due, Monday copy, reviewed week is the previous Monday. Monday 13:00: not due.
  Week note already reviewed, or no daily notes in the week: not due.
- Monday 09:00 with Friday's note unwrapped and no weekend notes: Wrap due, "Friday" in the copy.
  The same with a template-only Sunday: still Friday. Tuesday 09:00 with yesterday unwrapped:
  "yesterday"; yesterday template-only and nothing before it: not due. Friday wrapped and
  Thursday not: not due. No note in three days: not due. Any day at 05:30: not due.
- Friday 16:00 with daily notes only on Monday and Tuesday (outside the three-day window):
  Week Review still due, because `week_days` saw them. Monday 09:00 with last week's notes and
  no review: due.
- `skills` without `wrap-yesterday` or `week-review`: `evaluate` returns nothing (G4).
- `VaultSnapshot::read` on a `FakeFs` vault with configured `[daily]`/`[weekly]` names: reads the
  right paths, collects only level-1 headings, marks a template-only note, stats the reviewed
  week's days without loading them, tolerates missing notes and a missing template.

### 8.2 Policy and persistence (`nudges.rs`)

- A snoozed id is not picked today and is picked tomorrow.
- Two ids offered today: a third isn't picked; one of the two is re-offered without counting.
- The pending id is not re-offered.
- A ledger written to the test app's `KeyValueStore` and loaded again by key carries the snooze
  (restart). Old days are pruned on write.
- The skill texts: `wrap-yesterday.md` names the three-day rule and `week-review.md` step 1 names
  Friday, as string tests over the embedded assets (the way `hosted_agent.rs` checks Wrap Today).

### 8.3 The chat (`chat_panel.rs`, `VisualTestContext` + `simulate_keystrokes`, `MultiWorkspace::test_new`)

- A wrap nudge offered floats after eight quiet seconds as an opener with the *Go for it* / *Not
  now* chips, takes no focus, and the caption reads *Thock · HH:MM*.
- `ctrl-? y` dispatches `thock::RunSkill { skill: "wrap-yesterday" }`: with the Timeline manifest
  in the test vault, the chat's `pending_launch` is titled *Wrap Yesterday* (the connection is
  `Loading`), and the ledger has the id snoozed today. Focus is back in the note.
- `ctrl-? n` and `ctrl-? escape` snooze and fold; `thock::DismissNudge` from the palette does the
  same with the bubble folded into the dot.
- An evaluation with the id snoozed today offers nothing; a vault with no enabled Timeline
  Routine offers nothing; `"openers": "off"` reads nothing.
- The hourly tick: with GPUI executor timers, advancing the clock past the hour evaluates once;
  a vault file event in between does not (G2).
- Rendered text contains no skill id, path or JSON (V44 G7).

## 9. Changes by file

| File | Change |
| --- | --- |
| `crates/thock/src/nudges.rs` (new) | `Nudge`, `Moment`, `VaultSnapshot`, `NudgeOffer`, `Ledger`, `REGISTRY`, the two nudges, `reviewed_week`, `next_tick`, tests (§8.1, §8.2) |
| `crates/thock/src/thock.rs` | `mod nudges` |
| `crates/thock/src/chat_panel.rs` | `Opener::Nudge`, `AcceptNudge` / `DismissNudge`, the `nudge` key context, `nudge_tick`, evaluation on vault open and tick, the ledger in the store, tests (§8.3) |
| `assets/keymaps/default-{macos,linux}.json`, `assets/keymaps/vim.json` | `ThockChatPanel && nudge`: `y` / `n` |
| `crates/thock/assets/routines/timeline/skills/wrap-yesterday.md` | The target day (§5.1) |
| `crates/thock/assets/routines/timeline/skills/week-review.md` | Step 1, the reviewed week (§5.2) |
| `thock/VISION.md` §12 | Roadmap entry, updated when this ships |

No inherited crate is touched.

## 10. Follow-ups

- **A BYO surface:** a one-line offer in the Agent panel for people on their own agent.
- **Next entries:** a month's start (a monthly note without its review), a long gap since
  Reflect last ran (`.thock/state/memory/reflected`), a Backlog that hasn't been triaged in a
  while (`.thock/state/inbox/last-triage`).

## 11. Decisions

| # | Question | Decision |
| --- | --- | --- |
| 1 | Where a nudge renders | **The hosted chat, as a V44 opener.** V44 §5.8 built the bubble, the pause rule, the linger, `escape` as Not now and the `openers` setting for exactly this; §6.4 sketches the wrap offer. The BYO terminal panel stays untouched (V44 decision 15). |
| 2 | Model involvement | **None before acceptance.** Copy is fixed text in `due`. |
| 3 | Evaluation moments | **Vault open and the top of each hour** (midnight included). Never file events. |
| 4 | "Yesterday" | **The most recent daily note in the last three days.** Monday asks about Friday; a week away asks about nothing. `wrap-yesterday.md` adopts the same rule so a by-hand Monday run doesn't create an empty Sunday. |
| 5 | "Friday afternoon" | **Friday 15:00 through Monday 12:00**, local time. A Saturday first-open still offers; Monday morning offers last week. |
| 6 | The reviewed week | **This ISO week on Fri–Sun, the previous one on Monday**, in both the nudge and `week-review.md`, whose "week before today" was ambiguous on a Friday. |
| 7 | Per-day cap | **Two offers a day, one pending at a time.** Both chips snooze the id until tomorrow. |
| 8 | Persistence | **The key-value store, keyed by vault root**, like the Backlog's folds. Nothing in the vault, nothing in notes, no daily churn in the history. |
| 9 | Quiet hours | **Nothing before 06:00.** |
| 10 | How accepting launches | **`thock::RunSkill { skill }` dispatched on the workspace.** One launch path, same gating as the rail. |
| 11 | Keys | **`y` / `n` as named `AcceptNudge` / `DismissNudge` under `ThockChatPanel && nudge`**, with V44's chip keys, `enter` and `escape` unchanged. |
| 12 | A nudge left alone | **Folds into the dot, not snoozed.** It stays reachable with `cmd-?` and isn't offered again that day (it's still pending). |
| 13 | Template-only days | **Not a day to wrap.** V51 creates today's note at every launch and removes untouched past ones; the Wrap nudge skips a note that is only the template, using V51's `notes::is_template_only`. |
| 14 | The reviewed week's notes | **Stats only, up to seven.** The three-day daily window can't see a Friday's Monday, so the week's days are checked for existence without loading. |

## 12. Risks

- **R1: Two openers compete.** Reflect's opener and a nudge share one slot; the newer replaces
  the older (V44 §5.8). The replaced nudge is re-offered at the next tick without counting
  (§4.2 rule 3), so nothing is lost, only delayed an hour.
- **R2: The skill text drifts from the condition.** Both rules live in a user-editable skill. The
  string tests (§8.2) catch the shipped copy changing; a person's own edit is theirs to make.
- **R3: Hourly ticks across sleep.** A tick that fires hours late evaluates once and re-aligns.
  At worst a nudge arrives on wake, which is a fine moment for it.
- **R4: Clock and zone.** `Moment::now()` uses `chrono::Local`, like the reminders. A zone change
  mid-day moves the window edges by the offset, and nothing else.
