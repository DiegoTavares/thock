# Money Ritual

Read the numbers, propose the period's money movements from the user's own account rules,
tie them back to the life they named, and log only what they confirm they did. Named for
what it does, not for a day — it runs whenever invoked. If it has been much longer than a
period since the last entry, note it in passing; never scold.

**Read `routines/lifestyle/coach.md` first and answer as the coach it describes.** The
weekly run is fast and factual — the coach speaks fully in setup and Plan Re-check, where
there is room; here it gets at most one earned observation.

**Reads:** `routines/lifestyle/{coach,accounts,sources}.md`, `lifestyle/plan.md` (targets
and mechanics), the last few `lifestyle/site/data.js` entries, then the sources themselves.
**Writes:** `lifestyle/site/data.js` (append or amend one period entry),
`lifestyle/log.md` (append), today's daily note (one line, only if daily notes exist).

> **Thock never moves money, and says so.** Read-only toward every source, even where a
> write tool exists. The ritual records confirmed reality, never intent — and it never
> invents a number: missing data is an empty state or a question, not a plausible figure.

## 1. Read

The plan's targets and mechanics, the account roles and rules in `accounts.md`, the
sources named in `sources.md` (only those — never a tool that merely happens to be
installed), and the last few entries of `data.js` for trend context. If `plan.md` or
`accounts.md` is missing, this vault hasn't finished setup — say so once, offer **Set Up
Lifestyle**, and continue with whatever exists rather than erroring out.

## 2. Fetch — or degrade to a conversation

Pull the period's balances, income, and spending from the named sources. New statement
files follow the recipes in `sources.md`; an unrecognized format gets the learn-once
treatment described in **Connect Finance Data** (extract, show a sample, confirm, append
the recipe).

- **No source, or a source is down:** degrade to a conversation, don't fail. Ask for the
  few balances the account rules actually need, record the entry as `quality: "manual"`,
  and say plainly that this period is self-reported. The habit survives a broken connector.
- **Auth or network failure:** name the source and what to re-run (`gh auth login`-style,
  whatever the source needs) — never drop it silently.
- **Partial data** (some accounts measured, some told): the entry is `quality: "partial"`.

Periods are what they are: a week if the data is weekly, a month or statement range if
that's what arrived. Never allocate a monthly statement across fabricated weeks — declare
the real period and let the dashboard compare like with like.

## 3. Propose

Execute `accounts.md`'s **Order of operations** against the current balances and present
the result as an ordered action list with the post-ritual state spelled out:

```
1. Pay CIBC VISA to zero            — $1,203
2. Refill rent reserve to target    — $400
3. Residual to Line of Credit       — $610   (chequing floor $200 kept)

After: chequing $200 · VISA $0 · reserve full · LoC −$17,790
```

If the rules can't be satisfied — the floor would be breached, a card can't be cleared —
**stop and say so.** Never reach into a reserve to make the arithmetic work, and never
propose past a rule in the Never list. Overspending is a fact to surface, not a hole to
paper over.

## 4. Confirm, then log

The user acts in their own apps. Ask one structured question (AskUserQuestion in Claude
Code): which of the proposed actions did they actually do — **accept all · some · none**?
Amounts that differed land in "Other". Nothing is written before that answer, and only
confirmed actions are recorded.

## 5. Write

1. **`lifestyle/site/data.js`** — append one period entry to `window.PERIODS` (never touch
   `window.PLAN`):

   ```js
   {
     id: "2026-W35",
     period:  { kind: "week", start: "2026-08-24", end: "2026-08-30" },  // or "month" / "statement"
     quality: "measured",                    // measured | partial | manual
     income:  4210,
     spend:   { total: 3180, categories: { groceries: 612, dining: 188, kids: 340 } },
     balances:{ chequing: 2140, visa: -1203, loc: -18400 },
     outliers:[ { label: "Bike repair", amount: 420, category: "shopping" } ],
     actions: [ { label: "Paid CIBC VISA to zero", amount: 1203 } ],
     progress:{ runway: 41200, loc: -18400, savings: 0.24 },
     amended: null
   }
   ```

   Aggregates plus flagged outliers only — category totals, balances, income, and the
   handful of transactions worth surfacing (unplanned above a threshold, or a category
   blowout). No full ledger. `progress` is keyed by the plan's target ids so the dashboard
   never has to read `plan.md`; compute a value for every target you can, omit the rest.

   **Re-run of a period that already has an entry:** replace that entry with corrected
   numbers, stamp `amended` with today's date, and record in the log that it was amended
   and why. `data.js` always reads as the current best understanding; the log is the
   append-only truth.

   After writing, verify the file parses — a failure is reported and fixed, not swallowed:

   ```bash
   node -e "global.window={};require('./lifestyle/site/data.js');console.log(window.PERIODS.length,'periods')"
   ```

2. **`lifestyle/log.md`** — append the run (create the file if missing, newest at the
   bottom): date, period id, actions confirmed (with amounts), watch list, conversations
   surfaced, and any amendment note. The log narrates and cites; it is not a second
   ledger.

3. **Daily-note pointer** — if daily notes exist in this vault, append a one-line
   `# Money` entry to today's note with a `[[wikilink]]` to the log. If they don't, skip
   silently — nothing here assumes another Routine is installed.

## 6. Say

Four outputs, in this order, then stop:

1. **Plan drift** — on / ahead / behind, in the plan's own terms against its own targets.
   Never a generic benchmark.
2. **Vision tie-back** — one line connecting this period's money to the life. *"This week
   bought nine days of the 2031 runway."* This sentence is why this isn't a budgeting app.
3. **Watch list** — one to three things trending wrong. No padding to reach three.
4. **Conversations to have** — decisions needing a partner before the next run, surfaced
   explicitly. Only when the vision says money is shared; otherwise this output doesn't
   exist.

Then, at most, **one coach observation** — only when the data actually supports one.
Silence is the default.

Finally, if the plan's last revision (`window.PLAN.revised`) is old — months, not weeks —
one line: *"Plan last revised N months ago — run Plan Re-check?"* No timers, no nagging;
just the one sentence.
