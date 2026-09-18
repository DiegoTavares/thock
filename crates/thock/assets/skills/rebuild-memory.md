# Rebuild Memory

_A ritual for the agent: read through the notes this vault already holds and
build `memory/` from them, one stretch of days at a time, so a vault that
existed before Thock could remember starts with a memory instead of a blank
page. A human reading this: run it once on an old vault, or again after you
bring in notes from somewhere else. It can be stopped and picked up later._

**Reads:** every daily and weekly note, oldest first, in chunks; `profile.md`; `memory/`; `skills/thock/reflect.md`.
**Writes:** pages under `memory/`, `memory/index.md`, and `.thock/state/memory/rebuild` (progress, so a stopped run resumes).

> **Read `skills/thock/reflect.md` first, in full.** Every rule there binds
> here too: sources, dates, directives over anecdotes, what is never kept,
> the `What Thock should not keep` list in `profile.md`, the shape of the
> pages, the index cap. This ritual only changes *which notes* Reflect
> reads and *how it paces itself*.

## Why this is its own ritual

Reflect reads a day or a week. Reading a year at once would cost more than a
month of ordinary use and would produce a worse index, because the latest
facts would drown in the oldest. So this ritual walks the vault forward in
time, a fortnight at a time, letting later chunks supersede earlier ones the
way they did in life.

## Before you start

1. Resolve the daily and weekly note folders from `.thock/config.toml`.
2. List the daily notes and sort them by date. Say how many there are and
   how far back they go, then say what you are about to do in one sentence:
   *"I'll read them two weeks at a time, oldest first, and build up what I
   know as I go. Stop me any time; I can pick up where I left off."*
3. Look for `.thock/state/memory/rebuild`. If it holds a date, you are
   resuming: start from the chunk after that date and say so. If it holds
   `done`, ask whether they want to rebuild from scratch (then move the
   existing `memory/` pages aside as `memory/before-rebuild/` rather than
   deleting them) or only catch up on notes newer than the last run.
4. If `memory/index.md` already has entries and you are not resuming, ask
   one question: keep what is there and add to it, or start over. Default to
   keeping.

## The walk

For each chunk of up to **14 days** of daily notes, oldest first:

1. Read the chunk's daily notes and any weekly note whose week falls in it.
   Prefer a weekly note's review over re-deriving the week from its days
   when both exist: it is already a synthesis.
2. Run Reflect's **daily steps 2–5** over the chunk as if it were the
   period, then **weekly steps 6–7** (patterns and contradictions). Skip
   step 8 (forgetting) and step 9 (the quarter rule) — during a rebuild the
   index is supposed to change a lot, and nothing is stale until the walk
   reaches the present.
3. Write the pages after every chunk, not at the end, so a stopped run has
   lost nothing. Then write the chunk's last date to
   `.thock/state/memory/rebuild`.
4. Keep the index under `index_lines` at every chunk, demoting as Reflect
   does. Facts from early chunks that later chunks contradict get
   `(superseded YYYY-MM-DD)` and are removed at the next chunk.
5. Say one short line per chunk so the person can follow along, like
   *"March 2026: met Ana, started the site relaunch."* Never list files.

Pace yourself: if a chunk's notes are unusually long, split it in two. If
the vault has more than about a year of notes, ask after the first year
whether to continue now or another time.

## When you reach the present

1. Run Reflect's **weekly step 8** once, over everything: propose forgetting
   for pages whose `Last mentioned` is older than `stale_after_days`, in the
   current weekly note (create it from its template if it is missing).
2. Read `profile.md`'s **What you track** areas, if any, and make sure the
   index's threads use those names where they fit.
3. Write `done` to `.thock/state/memory/rebuild` and today's date to
   `.thock/state/memory/reflected`, so the next Reflect starts from here.

## Output

Three or four sentences: how many notes you read and the span they cover,
the handful of things that stand out (a person, a thread, a pattern), where
the page lives (`memory/index.md`), and that they can open it and change or
delete anything. If the run was stopped early, say where it stopped and
that running this again picks up from there.
