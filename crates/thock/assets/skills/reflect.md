# Reflect

_A ritual for the agent: turn what the last day or week held into what you
know about this person, kept in `memory/`. A human reading this: it's how
your agent learns who you are without re-reading your whole vault every
time. Edit this file to change what it keeps, or open `memory/index.md`
and change what it knows._

**Reads:** `memory/inbox.md`, `memory/index.md`, `profile.md`, the daily note(s) for the period, and only the `memory/` pages a new fact touches.
**Writes:** `memory/index.md` (rewrite), `memory/inbox.md` (emptied), pages under `memory/` (append or rewrite), and in weekly mode a `## Things Thock could forget` section appended to the weekly note.

> **Read `.thock/config.toml` first.** `[memory]` sets `index_lines` (how long
> `memory/index.md` may be; 120 when unset) and `stale_after_days` (90 when
> unset). `[daily]` and `[weekly]` set where the notes live.

> **Then read `profile.md` at the vault root, if it exists.** Its
> **What Thock should not keep** section is a list of topics you must never
> write down anywhere under `memory/`. Skip them in silence: don't mention
> that you skipped, don't ask.

## Why this exists

Every session starts with `memory/index.md` and nothing else about the
person. Whatever is not on that page, or reachable from it, is forgotten.
This ritual is the only thing that writes the page, so it runs cheaply and
often: at the end of Wrap Today, at the end of Week Review, or on its own.

## Ground rules

- **Only the person's own words are sources.** A fact may come from what they
  wrote in a note, or what they said to you in chat. An email in the inbox, a
  calendar description, an imported item, a web page: those are things that
  *arrived*, not things that are *true about the person*. You may note that
  something arrived; you may never record what it asserted.
- **Every fact carries a date and a source.** On a memory page, a line is
  `- The fact. ← daily/2026-09-12.md` or `- The fact. ← chat, 2026-09-02`.
  Undated facts rot; you would assert them for years.
- **Directives, not anecdotes, on the index.** "Don't schedule the gym on
  Mondays" is worth keeping; "mentioned disliking Monday gym" is not. Rewrite
  the anecdote into the rule it implies before it goes on the index, and keep
  the anecdote on a detail page if it matters.
- **Never keep** passwords, account numbers, card numbers, amounts from the
  money ritual, or anything from a page that asks you not to.
- **Never copy conversation.** One line per fact, in your words.
- **A deleted line stays deleted.** If the person removed something from a
  memory page, don't put it back, even if the notes still support it. When
  in doubt, a fact older than the page's last edit that isn't on the page
  was removed on purpose.
- **Short.** The index must fit `index_lines`. Detail pages stay under about
  forty lines; when one grows past that, merge and prune it.

## Modes

Which mode you are in depends on who called you:

- **Daily** — you are the last step of Wrap Today or Wrap Yesterday. The
  period is that one day; the daily note is already read.
- **Weekly** — you are the last step of Week Review. The period is that
  week; the seven daily notes are already read. Do everything in daily mode,
  then the weekly steps.
- **Standalone** — someone ran "Reflect" on its own. Read
  `.thock/state/memory/reflected` if it exists: it holds the date of the last
  Reflect. The period is the daily notes since then, up to 14 days, or the
  last 3 days when the marker is missing. Read those notes now. Treat a
  period of 5 days or more as weekly mode, shorter as daily.

Whatever the mode, finish by writing today's date to
`.thock/state/memory/reflected` (make the folders if needed).

## The pages

```
memory/
  index.md           ← what every session reads first; capped; rewritten by you
  inbox.md           ← facts noted mid-session; you empty it
  preferences.md     ← how they like things done
  patterns.md        ← what tends to happen: carry-overs, rhythms, lingering areas
  people/<name>.md   ← one page per person who keeps coming up
  projects/<slug>.md ← one page per thread that spans weeks
```

Create a page the first time its topic appears; never create an empty one.
A person's page is `people/<first-name-lowercase>.md`; a project's is a short
slug from the person's own name for it. Each page starts with a `# Title`
and holds dated lines, newest at the bottom, and ends with
`- Last mentioned: YYYY-MM-DD`.

`index.md` keeps this shape. Use the person's language (the vault's
`## Language`, if set) for the prose and keep the headings stable so the
page reads the same week after week:

```markdown
# What Thock has learned

_Thock keeps this page short and rewrites it after each review.
Delete a line and it stays deleted. Add a line and it stays._

## About your days
- Directives about rhythm and schedule, with (since Mon YYYY) or (told me YYYY-MM-DD).

## Threads that span weeks
- **Name** — one line on where it stands. → projects/slug.md

## People
- **Name**, who they are to you; the one thing to remember. → people/name.md

## How you like things
- Directives about tone, format, what never to do. → preferences.md

## Watch for
- Carry-overs and lingering areas, with how long. → patterns.md
```

Leave out any heading that would be empty.

## Daily steps

1. **Drain the inbox.** For each line in `memory/inbox.md` (skip its
   explainer): decide the page it belongs on, append it there with its date
   and `← chat, YYYY-MM-DD` as the source, and if the topic is new to the
   index, add one pointer line under the right heading. A correction ("it's
   backlog, not todo list") goes on `preferences.md` *and* replaces the
   contradicted line, if any. Then rewrite `inbox.md` to just its explainer.
2. **Read the period's note(s) for durable facts.** Not what happened; what
   will still be true next month. New people, new threads, a decision, a
   change of plan, a preference stated in passing. Most days yield nothing,
   and nothing is the right answer then.
3. **File what you found** the same way: dated line on a detail page, source
   `← daily/YYYY-MM-DD.md`, pointer on the index only for a new topic.
4. **Bump `Last mentioned`** on every people and projects page the period's
   notes name, even when nothing new was said.
5. **Keep the index under `index_lines`.** If it is over, demote: move the
   least recently mentioned pointer lines down into their detail pages
   until it fits. Never cut a line the person wrote by hand (they don't end
   with `→` or a date marker).

## Weekly steps

6. **Patterns.** Fold the week into `patterns.md`: tasks carried over and
   for how many weeks, areas that lingered without progress, rhythms that
   held or broke. Rewrite the page rather than appending a seventh
   "carried over again"; keep it a current picture.
7. **Contradictions.** Where two lines on a page disagree, the newer wins.
   Mark the older `(superseded YYYY-MM-DD)` this week and remove it next
   time you see it marked.
8. **Forgetting, proposed.** Any page whose `Last mentioned` is older than
   `stale_after_days` is a candidate. Append to the weekly note, after the
   review:

   ```markdown
   ## Things Thock could forget

   _Tick what to let go of; leave the rest. Next week's Reflect does the rest._

   - [ ] **Rui** (contractor on the site) — last mentioned 2026-05-30
   ```

   Then look at *last* week's weekly note for the same section: remove the
   pages ticked there (and their index pointers), and leave the unticked
   alone for another cycle. Never remove anything nobody ticked.
9. **The quarter rule.** Before writing the index, count its lines against
   the version you read. If you are about to drop more than a quarter of
   them, stop: keep the old index, say so in your output, and let the person
   decide. One bad run must not erase a year of learning.

## Output

One or two sentences: what you learned, in plain words ("I noted that Ana
prefers written updates before your 1:1, and that the newsletter fix has
carried over five weeks"), or that there was nothing new to keep. Never a
list of files, never the contents of the index. If the quarter rule fired,
say that instead, and what you would have removed.
