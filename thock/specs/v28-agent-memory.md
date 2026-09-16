# Thock V28: An agent that remembers

**Status:** Implemented (2026-09-15); the Fast-tier eval (DoD 7 and 9) is still to run
**Owner:** Diego · **Date:** 2026-09-15
**Companion docs:** `../VISION.md` (§4 invariants, §12 roadmap), `v23-personalized-rituals.md`
(the `profile.md` contract this extends), `v25-thock-plus-hosted-agent.md` (the hosted harness and
its token economics), `v27-agent-session-prompt.md` (the generated context block this appends to),
`v5-agent-and-onboarding.md` (the BYO rails that read `AGENTS.md`), `v2-invisible-git.md` (the
history this leans on for undo), research and proposal:
https://claude.ai/artifact/HxTB7tBJVdCt3Yq74EZDsZ

---

## 1. Summary

Every agent session starts blank. The hosted prompt is identity (`SYSTEM.md`), live facts
(`APPEND_SYSTEM.md`), vault conventions (`AGENTS.md`) and a one-time interview (`profile.md`), and
none of them accumulates anything. What the person says in chat ("my manager is Ana", "don't
suggest the gym on Mondays", "I stopped tracking the diploma") is gone when the session ends. The
rituals look back only mechanically: Wrap Today reads two or three prior notes, Week Review reads
the week. On BYOA, Claude Code partly fills the gap with its own auto-memory, but that lives in
`~/.claude/projects/…`, outside the vault, in a form only Claude Code reads. Thock Plus has nothing.

V28 makes the vault the memory and adds the one thing it lacks: a **distilled, bounded layer on
top**, written by reflection instead of by watching every message.

- **Core**, always in context, hard-capped: `profile.md` as today plus a new `memory/index.md`
  holding durable facts as directives and one-line pointers to detail notes.
- **Detail**, read on demand: one topic per file under `memory/` (people, projects, preferences,
  patterns). Every line carries a date and the note or chat it came from.
- **Episodes**, never loaded whole: the daily and weekly notes themselves, found by keyword and
  date search.

Memory is written by a new core ritual, **Reflect**, that runs as the last step of Wrap Today, Wrap
Yesterday and Week Review, and stands alone in the Routines rail for people who chat but never
wrap a day. Mid-session, the agent only appends one-line facts to `memory/inbox.md`; Reflect drains
that inbox into the right files. Nothing is a database, nothing leaves the machine, and every file
is one the person can open, edit, or delete.

The research behind these choices is on the proposal page. The short version: every mature memory
system (Claude Code, Letta, OpenClaw, Zep, mem0, Karpathy's llm-wiki) converges on a small capped
core, a large store behind search, consolidation run offline on a cheap model, dated facts that are
superseded rather than appended, and raw episodes that are never destroyed. The systems that inject
everything are the ones whose answers get worse as they grow.

## 2. Locked decisions (2026-09-15)

| # | Decision | Choice |
|---|---|---|
| 1 | Where memory lives | **`memory/` at the vault root**, plain Markdown, scaffolded with the Timeline Routine like `profile.md`. Pages are written in the second person ("what Thock has learned about you"), in the voice Set Profile uses. No database, no sidecar index, nothing under `.thock/`. |
| 2 | Tiers | **Three.** Core (`profile.md` + `memory/index.md`) is always in context. Detail (`memory/*.md`, `memory/people/`, `memory/projects/`) is read on demand. Episodes are the vault's own notes, found by search. |
| 3 | Index cap | **A config value**, `[memory] index_lines` in `.thock/config.toml`, default 120. Reflect enforces it; the app truncates at the cap with a visible warning line if a hand edit exceeds it. Tokens are not counted in Rust; lines are the unit a person can see. |
| 4 | Write trigger | **Reflection on a schedule, never per message.** Reflect runs inside Wrap Today, Wrap Yesterday and Week Review, and as a standalone ritual. In-session the agent may only append to `memory/inbox.md`. No model call is added to the reply path. |
| 5 | Standalone Reflect | **Yes, with a nudge.** "Reflect on the last few days" appears in the Routines rail under the core rituals. The chat panel suggests it once the memory inbox has been unemptied for a configurable number of sessions (`[memory] nudge_after_sessions`, default 5). |
| 6 | Confirmation | **Keep silently, confirm only forgetting.** New facts are promoted automatically with a date and source. Facts unmentioned for `[memory] stale_after_days` (default 90) are listed under the weekly review as candidates; the person confirms, and the next Reflect removes them. A rewrite that would drop more than a quarter of the index's lines is refused and reported. |
| 7 | Provenance | **Every detail line carries a date and a source** (`← daily/2026-09-12.md` or `← chat, 2026-09-02`). Superseded facts stay one more cycle marked `(supersedes …)` then go; invisible history keeps the rest. |
| 8 | Directives over observations | **The index holds rules and pointers, not anecdotes.** Reflect rewrites "mentioned disliking Monday gym" into "Don't schedule the gym on Mondays." Observations live in detail files. |
| 9 | Untrusted text | **Only the person's own notes and own chat turns are sources.** Inbox items, email bodies, calendar descriptions and anything a Routine imported from outside may be noted as having arrived, never recorded as a fact about the person. This is the memory-poisoning defence and it is not configurable. |
| 10 | What is never kept | **`profile.md` gains `## What Thock should not keep`.** Reflect reads it first and skips those topics in silence, the V23 contract. Also never kept, unconditionally: credentials, account numbers, amounts from the money ritual, and chat transcripts. |
| 11 | Read path on Plus | **`compose_vault_context` appends `memory/index.md`** under the cap as a `## What you already know` block in `APPEND_SYSTEM.md`, after the Routines list. One more block in a file V27 already generates; no Pi feature. |
| 12 | Read path on BYOA | **`AGENTS.md` gains a ground rule**: read `memory/index.md` before you start, open a detail file when the conversation touches it, and keep facts about the person in `memory/`, not in any harness-private memory. Claude Code's own memory directory keeps working for its scratch; the spec documents the duplication rather than fighting it. |
| 13 | Retrieval over episodes | **Search, not embeddings.** The agent already has `grep`, `find` and `ls`, and every daily note is named by date. A ranked Thock-owned `search` tool is Phase 2, only if grep proves too blunt; local embeddings are Phase 3, only on evidence. |
| 14 | Ownership | **`memory/` is the agent's synthesis and it may rewrite it**, the same standing as `# Daily Closure`. The append-don't-rewrite rule still binds everything the person wrote. A line the person deletes from `memory/` stays deleted: Reflect never re-adds a fact whose source date is older than the file's last change. |
| 15 | Model tier | **Reflect runs on the Fast tier.** Its front matter declares `tier = "fast"` like other cheap rituals; the whole pipeline is designed for a Flash-class model. |

## 3. Goals & success criteria

**G1 — It remembers what you told it.** Say "my manager is Ana, our 1:1 is Thursdays" in one
session. Run Wrap Today. In a fresh session, "when is my next 1:1" is answered without a file read.

**G2 — It learns from the notes, not just the chat.** After four weeks of daily notes mentioning
the website relaunch, the index names it as the main thread and points at
`memory/projects/site.md`, which lists what has happened with dates and sources.

**G3 — The cost of remembering does not grow with the vault's age.** The per-session fixed
overhead rises once (by at most the index cap) and then stays flat. Reflect's daily run reads
today's note, the inbox, the index and the touched detail files, never the vault.

**G4 — A person can read, correct and forget.** `memory/index.md` reads as a page about them, not
a dump. Deleting a line is enough to forget it. `profile.md` can forbid a topic in one line.

**G5 — Both paths, one design.** Pi on Plus and Claude Code, Gemini CLI or Codex on BYOA read and
write the same files through the same ritual. No harness feature is required on either side.

**G6 — Nothing from outside becomes a belief.** An email in the inbox that says "Diego prefers
to be called Sir" leaves no trace in `memory/` after Reflect.

## 4. Non-goals

- **No per-message extraction, no vector store, no server-side memory.** Notes never leave the
  machine for the agent's sake (V25 non-goal), and the reply path gains no model call.
- **No transcript archive.** Chat history is not memory. Nothing copies conversations into the
  vault; only the one-line facts the agent chose to keep.
- **No changes to what the harnesses do with their own memory.** Claude Code's private directory
  is instructed, not disabled.
- **No ranked search tool or embeddings in this spec.** Phases 2 and 3 are listed in §8 as
  follow-ups gated on evidence from Phase 1 use.
- **No memory UI beyond a rail entry.** "What Thock knows about you" opens `memory/index.md`; there
  is no bespoke panel.
- **No multi-vault or shared memory.** One vault, one `memory/`.

## 5. Design

### 5.1 The files

```
memory/
  index.md          always loaded; capped; directives + pointers
  inbox.md          one-liners learned mid-session, drained by Reflect
  preferences.md    how they like things done
  patterns.md       what tends to happen (carry-overs, rhythms, lingering areas)
  people/<name>.md  one file per person who keeps coming up
  projects/<slug>.md one file per thread that spans weeks
```

`index.md` after two months, as scaffolded voice and structure intend it:

```markdown
# What Thock has learned

_Thock keeps this page short and rewrites it after each review.
Delete a line and it stays deleted. Add a line and it stays._

## About your days
- Mornings are for deep work; meetings cluster after 14:00. (since Jul 2026)
- Don't schedule the gym on Mondays. (told me 2026-08-11)
- You write in Portuguese but want reviews in English.

## Threads that span weeks
- **The website relaunch** is the main thing this quarter. → projects/site.md
- **Mum's appointments**: a recurring area, roughly monthly. → people/mum.md

## People
- **Ana**, your manager; weekly 1:1 on Thursdays. → people/ana.md

## How you like things
- Short closures, no bullet lists of what you did. → preferences.md
- "Backlog", not "todo list". Never move a task without asking.

## Watch for
- "Fix the newsletter signup" has carried over 5 weeks. → patterns.md
```

A detail file:

```markdown
# Ana

- Your manager since at least July. ← daily/2026-07-03.md
- 1:1 moved from Tuesday to Thursday. ← daily/2026-08-21.md (supersedes 07-03)
- Prefers written updates before the 1:1. ← chat, 2026-09-02
- Last mentioned: 2026-09-11
```

`inbox.md` is append-only during a session and empty after Reflect:

```markdown
- 2026-09-15 · Rui is the contractor on the site build; invoices monthly.
- 2026-09-15 · Correction: it's "backlog", not "todo list".
```

The headings in `index.md` are conventions Reflect keeps stable, not headings the app parses.
Everything under `memory/` is ordinary Markdown the person may edit.

### 5.2 Scaffold and config

- The Timeline Routine scaffold (`routines.rs`, beside `profile.md` and the core skills) creates
  `memory/index.md` and `memory/inbox.md` with their explanatory headers, and
  `skills/thock/reflect.md`. Create-if-missing; never touched again by the app.
- `VaultConfig` gains a `memory: MemoryConfig` section:

```toml
[memory]
index_lines = 120          # cap on memory/index.md; Reflect trims, the app truncates
stale_after_days = 90      # unmentioned this long → proposed for forgetting
nudge_after_sessions = 5   # chat sessions with a non-empty inbox before the panel suggests Reflect
```

- `profile.md` gains a section, and Set Profile one question (asked last, after tone):

```markdown
## What Thock should not keep

- One line per topic Thock must never write down, in your words. Empty is fine.
```

### 5.3 The write path

**In-session (any harness).** A rule in `SYSTEM.md` and in `AGENTS.md`: when the person says
something that will still be true next month, or corrects you, append one dated line to
`memory/inbox.md`. Never edit `index.md` or a detail file outside Reflect. This is one `append`
call the agent already makes; a bad session can at worst leave junk in the inbox, never wreck the
core.

**Reflect, daily mode** (last step of Wrap Today and Wrap Yesterday). Reads: today's note, the
inbox, the index, `profile.md`, and only the detail files a new fact touches. Does, in order:

1. Skip anything matching `## What Thock should not keep`, and anything whose source is imported
   text (decision 9).
2. Promote each inbox line into the right detail file with its date and source; create the file
   if the topic is new and add a pointer line to the index.
3. Rewrite observations as directives before they enter the index (decision 8).
4. Bump `Last mentioned` on any person or project today's note names.
5. If the index exceeds `index_lines`, demote the least recently mentioned pointer lines into
   their detail files until it fits.
6. Empty the inbox. Say in one sentence what was learned, no list.

**Reflect, weekly mode** (last step of Week Review, which has already read the week's notes).
Everything in daily mode, plus:

7. Fold the week's observations into `patterns.md` (carry-overs, lingering areas, rhythms) and
   the project files.
8. Resolve contradictions: the newer fact wins, the older line is kept one cycle marked
   `(superseded)`, then removed.
9. List facts whose `Last mentioned` is older than `stale_after_days` under a
   `## Things Thock could forget` heading appended to the weekly note. The person ticks what to
   forget; the next Reflect removes ticked items and leaves the rest alone for another cycle.
10. Refuse and report any rewrite of `index.md` that would drop more than a quarter of its lines.

**Reflect, standalone** ("Reflect on the last few days"). Weekly mode over the daily notes since
the last Reflect (tracked by a `.thock/state/reflect.last` marker, documented in the skill), capped
at 14 days.

### 5.4 The read path

**Plus.** `compose_vault_context` reads `memory/index.md` and appends it as
`## What you already know` after the Routines block, truncated at `index_lines` with a closing
line `The rest of this page is over its cap; the next Reflect will trim it.` when it had to cut.
The index arrives with the session, like the date. Detail files are opened by the agent when a
pointer is relevant, which `SYSTEM.md` tells it to do.

**BYOA.** `AGENTS.md` ground rule 6 (after `profile.md`):

> **Remember through `memory/`.** Read `memory/index.md` before you start; it is what you already
> know about this person. It points at notes under `memory/`; open one when the conversation
> touches it. When they tell you something that will still be true next month, add one dated line
> to `memory/inbox.md`; the Reflect ritual files it. Facts about this person belong there and
> nowhere else, including any memory of your own outside this folder.

**Skills.** A ritual's front matter may declare `reads = ["memory/people", "memory/patterns"]`
so the kickoff prompt names what to open, the way it names the ritual path today. Wrap Today and
Week Review declare `memory/patterns`; Set Profile declares nothing.

**Episodes.** Unchanged: `grep`, `find`, `ls`, and dated filenames.

### 5.5 Surfaces

- **Routines rail:** the Timeline Routine carries a **Reflect** ritual row (a pointer skill that
  runs the core `skills/thock/reflect.md` in standalone mode, on the Fast tier) and a **What Thock
  Knows** link that opens `memory/index.md`. Both reach the agent through the generic
  `thock::RunSkill` / `thock::OpenLink` actions. Independently of any Routine, `thock::Reflect` and
  `thock::RebuildMemory` run the core rituals from the command palette, beside Set Profile.
- **Rebuild Memory** (`skills/thock/rebuild-memory.md`): for a vault that existed before V28, or
  after notes are imported, walks the daily and weekly notes oldest-first a fortnight at a time,
  applying Reflect's rules chunk by chunk, writing after every chunk, and recording progress in
  `.thock/state/memory/rebuild` so a stopped run resumes. Forgetting and the quarter rule are
  skipped during the walk and applied once at the present. The Timeline setup mentions it after a
  migration.
- **Chat panel:** the activity line names memory in plain language ("checked what I know about
  Ana", "noted one thing for later"). When the inbox has been non-empty for
  `nudge_after_sessions` sessions, the empty-state under the composer shows one line: *Thock has a
  few things to file about you. Reflect now?* with the run action. Dismissing it resets the count.
- **Weekly note:** the `## Things Thock could forget` section is the only place forgetting is
  proposed, and it is a checklist the person edits like any other.

### 5.6 Cost and context

Per-session fixed overhead on Plus goes from roughly 3 to 4k tokens to 5 to 6k and then stops
growing, because the only new always-loaded block is capped. Reflect on a Flash-class tier costs
about $0.07 to $0.60 per user per month at September 2026 list prices (daily run ≈ 12k in / 1k out,
weekly ≈ 45k in / 3k out), against ten to thirty times that for per-message extraction and an
unbounded bill for loading recent notes into every prompt. The proposal page carries the table.

## 6. Definition of done

1. `memory/index.md`, `memory/inbox.md` and `skills/thock/reflect.md` are scaffolded with the
   Timeline Routine; a vault without them keeps working (empty state, not error).
2. `[memory]` parses with the defaults above; a missing section is the defaults.
3. `compose_vault_context` appends the index under the cap with the warning line when cut, covered
   by unit tests over synthetic vaults (empty index, index at cap, index over cap, no `memory/`).
4. `SYSTEM.md` and `assets/AGENTS.md` carry the inbox rule and the read rule; the prompt-and-guard
   test still passes.
5. Reflect ships with daily, weekly and standalone modes; Wrap Today, Wrap Yesterday and Week
   Review end by running it; each is `tier = "fast"`.
6. `profile.md` has `## What Thock should not keep`; Set Profile asks for it; Reflect honours it.
7. Decision 9 holds in a test vault: an inbox email asserting a fact about the person leaves no
   line in `memory/` after Reflect.
8. Rail entries and the chat nudge exist and are keyboard-reachable; the nudge count resets on
   dismiss and on Reflect.
9. A Fast-tier eval of 20 vault-shaped Reflect runs (G1, G2, G6, cap enforcement, the
   quarter-loss refusal) passes on the Plus default model, recorded in the PR.
10. `thock/VISION.md` §12 marks the entry shipped in the same change.

## 7. Implementation notes (2026-09-15)

- New module `crates/thock/src/memory.rs`: scaffold, capped index read, inbox check, nudge counter
  under `.thock/state/memory/sessions-with-inbox`, all unit-tested.
- `[memory]` in `vault.rs` (`index_lines`, `stale_after_days`, `nudge_after_sessions`); never
  written unless set. `compose_vault_context` appends `## What you already know`.
- Existing vaults gain `memory/` and the two core rituals on their next reconcile pass. Reflect and
  Rebuild Memory are shipped core files, so V29's reconcile keeps them current; Wrap Today, Wrap
  Yesterday and Week Review upgrade in place where they were never edited, and otherwise wait
  under `.thock/pending/` for the Update Rituals ritual.
- The chat nudge is one row above the composer with "Reflect now" and "Not now";
  `thock::DismissMemoryNudge` and `thock::Reflect` are the keyboard paths.

## 8. Risks

- **Cheap-model discipline.** A Flash-class model may promote anecdotes, skip the source marker, or
  rewrite more than it should. The eval in DoD 9 is the gate; the quarter-loss refusal and the
  inbox-only rule for sessions bound the damage; history is the undo.
- **Duplication with Claude Code's private memory** on BYOA. Instructed, not prevented; a user may
  see two memories drift. Documented as a known gap; revisit if it confuses anyone in practice.
- **Index cap as lines, not tokens.** Very long lines defeat it. Reflect is told to keep lines
  short; the app truncates by lines regardless. Accepted for readability.
- **Stale marker for standalone Reflect** under `.thock/state/` is one more agent-written state
  file. Same convention the ready and done markers already use.
- **Forgetting proposals nobody reads.** If the weekly checklist is ignored, stale facts persist
  indefinitely. Acceptable: the cost is a few index lines, and the cap demotes them out of context
  anyway.

## 9. Follow-ups (not in this spec)

- **Phase 2, visibility and search:** a ranked `search` tool (keyword with recency boost) exposed
  to Pi through the existing extension mechanism, only if grep proves too blunt in Phase 1 use;
  memory files named in the activity line with diff previews.
- **Phase 3, semantic recall:** a small local embedding model over the vault, indexed off file
  events, behind the same tool. Justified only by Phase 2 evidence that keyword recall misses what
  people ask for. Still no server; the index is rebuildable from the files at any time.
