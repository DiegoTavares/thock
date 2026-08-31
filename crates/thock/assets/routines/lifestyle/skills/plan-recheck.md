# Plan Re-check

Has the life changed? Revise the plan against it. This is the deliberate sit-down — run it
when something real moved (a raise, a move, a child, a slow drift the ritual kept flagging)
or when the Money Ritual notes the plan has gone stale.

**Read `routines/lifestyle/coach.md` first and answer as the coach it describes.** This is
one of the two runs where the coach speaks fully.

**Reads:** `lifestyle/vision.md`, `lifestyle/plan.md`, the recent `lifestyle/site/data.js`
entries, `routines/lifestyle/accounts.md`.
**Writes:** `lifestyle/plan.md` (append a revision; one head-line edit), `lifestyle/site/data.js`
(`window.PLAN` rewritten).

## 1. Read the life before the numbers

Read the vision first and ask the user what has changed **in the life** since the last
revision — not in the accounts. If the Tuesday itself has changed, say so: a plan revision
can absorb a new timeline, but a changed destination deserves an edit to `vision.md` (the
user's edit, in their words — offer to help organize it, never rewrite it for them).

Then read the recent periods in `data.js` for what reality has been saying: sustained
drift, targets landing early, a savings rate the user never actually hits. Rule 3 applies
— a target missed every single period is not a discipline problem, it is a wrong target.

## 2. Windfalls and raises get the goalpost question

If income rose or a windfall landed since the last revision, ask what the extra bought *in
vision terms* before folding it into the plan. Name lifestyle creep once, plainly, without
moralizing, and move on.

## 3. Append the revision

Never rewrite the plan. Append a dated revision at the end of `lifestyle/plan.md`, under
`## Revisions`:

```markdown
### Revision — 2026-Q4 (2026-11-14)

**What changed in the life:** …
**What changed in the plan:** target "twelve months of runway" moved 2028-12-31 → 2029-06-30, because …
**Still true:** …
```

Every changed target keeps the room-for-error discipline (state what breaks first) and
cites the vision line it serves. **Still true** matters: name what is already fine, not
only what moved.

The one edit-in-place this routine ever makes: update the `_Derived from…_` line at the
head of `plan.md` with a pointer to the newest revision, so the current state is one hop
from the top of a long-lived file. Touch only that line.

## 4. Rewrite `window.PLAN` in the same step

`plan.md` and `window.PLAN` are written together, always — if they drift, the dashboard
shows targets the plan no longer holds. Rewrite `window.PLAN` in
`lifestyle/site/data.js` (leave `window.PERIODS` untouched): bump `revised`, carry the
horizons, restate `annual_cost_of_vision` and `assumption`, and one row per target.

**Target ids are stable across revisions.** A renamed target keeps its id; a retired
target is removed; a new target gets a new id it will keep forever — the dashboard's
history for each row keys off it. Then verify the parse:

```bash
node -e "global.window={};require('./lifestyle/site/data.js');console.log(window.PERIODS.length,'periods')"
```

A failed parse is reported and fixed, not swallowed.

## 5. Close

Read the revision back in two or three sentences: what moved, what it serves, what is
still true. One coach observation at most, and only if the re-check earned it.
