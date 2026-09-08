# Daily & Weekly (the Timeline Routine)

This Routine closes the loop on your daily and weekly rhythm: it turns a week
of notes into a reviewed, visualized record. This page is the tour; each
section ends with something to try right now.

The rituals below read `profile.md` at the vault root before they run, so
they ask about *your* weeks. If you haven't written one, run **Set Profile**
(`thock: set profile`). It's a short interview, and it's what decides
whether these rituals go looking for code or leave your notes alone.

## The Timeline

The Routines panel in the left sidebar carries this Routine's section:

- **Today** / **Yesterday** open daily notes (`daily/YYYY-MM-DD.md` by
  default), created from `templates/daily.md` the first time.
- **This Week** / **Last Week** open weekly notes (`weekly/GGGG-Www.md` by
  default), created from `templates/weekly.md`.
- The same entries (plus **Tomorrow**) live in the command palette:
  `thock: open today` and friends — they work even with this Routine's
  section hidden.

Existing notes are only ever opened — never overwritten.

> **Try it now:** open **Today** and jot one line under Journal.

## The Day Planner

Add timed tasks to today's note under the `## Day planner` heading —

```
- [ ] 09:00 - 10:30 Deep work
- [ ] 14:00 Standup
```

— and the **Day Planner** panel (right sidebar) mirrors them as a vertical
day grid. Click a block to jump to its line.

> **Try it now:** add a timed task to today's note and watch the grid.

## The skills

Skills are rituals your **own agent** runs — plain markdown files you can
open, read, and edit. Hover a skill in this Routine's section and press the
run button, use `thock: run skill` from the palette, or invoke them as
slash commands inside an agent conversation:

- **Wrap Today** (`/wrap-today`) closes out today's note: tasks, recent
  context, anything your profile lets it pull in from outside (commits, for
  a vault that tracks code), then an appended `# Daily Closure` review.
- **Wrap Yesterday** (`/wrap-yesterday`) — the same closure for yesterday,
  for when the day got away from you.
- **Week Review** (`/week-review`) aggregates the week's notes into a
  summary by area, appends an `# AI Week Review` to the weekly note, and
  feeds the dashboard.
- **Set Up Timeline** — the guided migration that (maybe just) ran; rerun it
  any time more old notes turn up.

Every skill appends; none of them rewrite or delete what you wrote.

> **Try it now:** run **Wrap Today** from the Routines panel and watch your
> agent work in the Agent panel.

## The backlog

Unfinished tasks shouldn't die in yesterday's note. The vault keeps a holding
pen at `backlog.md` with three sections: **Soon** (you mean to do it in the
coming days), **Someday** (worth keeping, no commitment), and **Completed**
(a dated history of what got done). The **Backlog** panel in the bottom dock
renders it as a live checklist — click a task's text to edit it, check it off
to record it as done in today's note and file it under Completed.

The wrap skills feed it: when **Wrap Today**, **Wrap Yesterday**, or
**Week Review** find unfinished tasks, they ask whether to move **all, none,
or some** of them to the backlog (Soon by default — say "someday" for the
no-rush ones). Nothing moves without your answer, and tasks already in the
backlog are never duplicated.

> **Try it now:** open the Backlog panel and add one task to Soon.

## The weekly dashboard

`weekly/site/index.html` — click **Weekly Dashboard** in this Routine's
section to open it in your browser. It computes per-week stats, sparklines,
goal completion, and warnings (time sinks, carry-overs, lingering projects)
from the feed in `weekly/site/data.js`. It starts empty; each Week Review
appends one entry.

The page fits itself to you. `window.PROFILE` at the top of `data.js` says
whether this vault tracks code: when it doesn't, the pull-request panel, its
three stat tiles, and its timeline bar disappear, and carried-over goals and
personal items take their place. Left on `"auto"`, the page decides from
whether your weeks actually carry any. `focus` there keeps each of your
areas the same colour week after week.

## Make it yours

Everything is a plain file:

- `routines/timeline/routine.toml` — this Routine's definition: its name,
  quick links, and skill list. Edit it and the panel follows.
- `templates/daily.md`, `templates/weekly.md` — what new notes start from.
- `routines/timeline/skills/*.md` — the rituals themselves. Edit one and the
  next run honors your edit; the agent always reads the live file.
- `profile.md` (vault root): who you are, the areas you track, and the only
  outside sources any ritual may look at. **Set Profile** writes it; you can
  edit it directly.
- `routines/timeline/sources.md` — the repositories the wrap and review skills
  read from, when your profile lets them. The skills ask once and record your
  answer here; edit the list and they follow it. Nothing is queried unless
  it's on this list.
- `.thock/config.toml` — where notes live, how they're named, and this
  vault's agent command override.

Removing this Routine never touches your notes, and any shipped file you have
edited is kept.
