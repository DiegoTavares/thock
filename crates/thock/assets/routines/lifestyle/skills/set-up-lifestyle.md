# Set Up Lifestyle

Interview the user about the life they want, write it down in their words, wire up where
their numbers come from, and then — only once real numbers have landed — write the plan
that prices that life. One sitting when it can be, resumable when it can't.

**Read `routines/lifestyle/coach.md` first and answer as the coach it describes.** The
interview below is where that persona matters most — this is the one run where the coach
speaks fully.

**Reads:** `lifestyle/**`, `routines/lifestyle/**`, `.thock/config.toml`,
`.thock/state/onboarded/lifestyle/`.
**Writes:** `lifestyle/vision.md`, `lifestyle/plan.md`,
`routines/lifestyle/{sources,accounts}.md`, `lifestyle/site/data.js` (`window.PLAN`),
`.thockignore` (append), `.thock/state/onboarded/lifestyle/*`.

> **The budget rule: ten questions with no right answer, across all of setup.**
> Confirmations do not count — reviewing a proposal you built from real data is not a
> question. Follow-ups do not count either: push on a vague answer as many times as it
> takes ("I want freedom" → *"Freedom to do what, on a Tuesday, that you can't do now?"*
> is still free). Only a **new topic** spends budget. Ask one question at a time, through
> the structured question tool where one exists, and wait.

## 0. Resume

Look under `.thock/state/onboarded/lifestyle/` for phase markers:

```
vision      lifestyle/vision.md exists
source      sources.md names a working source
accounts    accounts.md is confirmed
plan        plan.md and window.PLAN exist
```

If any exist, say where the user left off in **one sentence** and continue from the first
missing phase. A user who stopped after `vision` still owns a finished document — treat the
re-run as a continuation, never a restart, and never re-ask what a written file already
answers.

## 1. The interview (phases: vision)

Ten questions. 1 and 10 are structural and cheap; the eight in between are the interview.
These are scene questions, not checklist questions — each covers several dimensions of a
life at once, and the answers arrive as stories. Your job is to collect concrete detail,
not to improve it.

1. **Is the money in your life shared with anyone — and who has a vote on these
   decisions?** If shared, reframe every later question as "you and <partner>", and carry
   the household machinery through the plan (§4). If not, none of that machinery appears —
   no empty headings.
2. **Walk me through an ordinary Tuesday ten years from now. Wake to sleep — where are
   you, who's in the house, what are you doing at 10am, 3pm, 7pm?**
3. **In that life, what are you *for*? When someone whose opinion you actually care about
   describes what you do, what do they say — and why is it them you care about?**
4. **Who do you see in an ordinary week, and what has to be true for that to be
   possible?**
5. **Ten years is far enough away to be safe. What has to be *already true* five years
   from now for that Tuesday to still be reachable?**
6. **Now describe five years from now if it went wrong — as concretely as you just
   described the good one.**
7. **What clocks are running that money can't slow down?** (Kids' ages, parents' ages, a
   body that won't do this at 60.)
8. **Of everything you've described, where is today furthest from that Tuesday — and which
   of those gaps is money actually the lever for?** This is the hinge of the whole
   interview: it separates what money can fix from what it can't, and it is why the plan
   that follows won't drift into generic wealth-building.
9. **Tell me about a money decision that looked wrong on a spreadsheet but was right for
   you — or one that looked right and you couldn't stick to.** This calibrates *reasonable
   beats rational* — read it back before every recommendation you ever make here.
10. **How should I get your numbers?** (Asked in phase 2 — see §3.)

Push until the answers are concrete enough to cost. A vision made of abstractions
("comfortable", "secure", "free") cannot be priced and will not survive into the plan.

## 2. Write the vision

Write `lifestyle/vision.md` in the user's own words wherever possible — organize, don't
improve. Shape:

```markdown
# Vision

_Interviewed YYYY-MM-DD. This is your writing — edit it whenever it stops being true._

## The Tuesday (YYYY)
## What I'm for
## The people and the place
## Already true by YYYY
## The version where it went wrong
## Clocks money can't slow
## Where today is furthest
| Dimension | Today | The Tuesday | Is money the lever? |
| --- | --- | --- | --- |
## How I actually behave with money
```

Fill "Where today is furthest" from question 8 and "How I actually behave with money" from
question 9, quoting the user. Show the finished document, then:

1. Append to the vault-root `.thockignore` (create if missing, never overwrite existing
   lines) so money data stays out of invisible history:

   ```
   lifestyle/statements/
   lifestyle/site/data.js
   lifestyle/log.md
   ```

2. Write an empty marker at `.thock/state/onboarded/lifestyle/vision` (create directories
   as needed).

## 3. Wire the source (phases: source, accounts)

Question 10, then follow `routines/lifestyle/skills/connect-finance-data.md` — it conducts
the source wiring, the first sync, the structured account-map confirmation, and the
currency choice, and it writes the `source` and `accounts` markers. Nothing in it spends
interview budget: it is built from confirmations.

If the user has no connector and no statements, that path still completes: the numbers
arrive conversationally and are recorded as `quality: "manual"`. Never treat the manual
path as second-class, and never stall setup waiting for a connector.

## 4. Write the plan (phase: plan)

Only now — with the vision written and real balances landed — write `lifestyle/plan.md`.
The plan's first sentence prices a real life against real numbers, not hypotheticals.
Shape (every number carries the sentence of the vision it came from):

```markdown
# Plan

_Derived from [[vision]] on YYYY-MM-DD, against balances measured YYYY-MM-DD. Currency: XXX._

## What the Tuesday costs
## The gap
## Room for error
## Targets
| Target | Denominated in | Amount | By | Why (vision line) |
| --- | --- | --- | --- | --- |
## The mechanics
## Decisions that need both of you   ← only when money is shared (question 1)
## Questions I'm not qualified to answer
## Revisions
```

Rules the coach enforces on itself here:

- **Denominate targets in autonomy first** — months of runway, hours reclaimed, a year of
  choice — then in currency. Cite the vision line each target serves; no target the user's
  own words didn't put there.
- **Room for error is not optional.** Refuse to write a plan that only works if things go
  right — say what breaks first and what absorbs it. Where a projection needs a growth
  assumption, state the assumption in the same sentence.
- **The mechanics mirror `accounts.md`** — the rules the Money Ritual will execute each
  period. `accounts.md` is what the ritual reads; keep the two telling one story.
- **"Questions I'm not qualified to answer"** collects the product, tax, and legal
  decisions for a human professional, phrased as questions to take to one.

In the **same step**, write `window.PLAN` into `lifestyle/site/data.js` (replace the seed's
`window.PLAN`, leave `window.PERIODS` alone) with `revised`, `horizons`,
`annual_cost_of_vision`, `assumption`, and one `targets` row per plan target —
`{ id, label, unit, target, by }`. Target ids are stable forever; the dashboard's history
keys off them. Also set `window.CONFIG = { currency: "..." }` from the confirmed currency.
Then verify the file parses:

```bash
node -e "global.window={};require('./lifestyle/site/data.js');console.log(window.PERIODS.length,'periods')"
```

A failed parse is reported and fixed, not swallowed. Finally write the
`.thock/state/onboarded/lifestyle/plan` marker.

## 5. Close

Show the plan. Remind the user: the vision and plan are theirs to edit; the ritual is
**Money Ritual**, run whenever they sit down with their money; and the Money Dashboard
fills in from the first run. One coach observation at most, and only if the interview
actually earned one.
