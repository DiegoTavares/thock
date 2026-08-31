# Thock V14 — The Lifestyle Routine: money in the service of a life you named

**Status:** Implemented (2026-08-29)
**Owner:** Diego · **Date:** 2026-08-29
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.2 Augmentation not replacement,
§4.4 Human-in-the-loop, §4.5 Everything is editable, §4.6 Modular life), `v7-dynamic-routines.md`
(the `routine.toml` this ships as), `v13-inbox-routine.md` (the read-only-toward-sources posture and
the structured-confirmation grammar), `v11-routines-rail.md` (how the section renders),
`v2-invisible-git.md` (the history exclusion this depends on)

---

## 1. Summary

V14 ships the **Lifestyle Routine** — the fork's second real Routine and the first one whose subject
is the user rather than their notes. It replaces VISION §12's long-planned "Friday Finance skill"
with something wider: a routine that starts from **Cal Newport's lifestyle-centric planning** — describe
the life first, in concrete scenes, then work backwards to what money has to do — and runs it on a
weekly loop with **Morgan Housel's** posture toward money as behavior rather than math.

It is deliberately not a budgeting app. A budgeting app knows what you spent. This knows what you
spent it *for*.

Four deliverables, each severable:

1. **The Coach (§6)** — `routines/lifestyle/coach.md`, an editable persona every skill adopts. Four
   operative rules, not a tone: independence is the return, room for error is a feature, reasonable
   beats rational, beware the moving goalpost.
2. **Setup (§7–§8)** — one resumable ritual, **ten questions with no right answer**, that writes
   `lifestyle/vision.md` (the life), wires a data source, and then — only once real numbers have
   landed — writes `lifestyle/plan.md`: cost of the vision → gap → dated targets → weekly mechanics.
3. **The weekly loop (§9–§11)** — a source-agnostic data contract, `lifestyle/site/data.js` as the
   single numeric record, and the **Money Ritual**: propose the week's money movements from account
   rules, check plan drift, tie it back to the vision, and log only what the user confirms they did.
4. **The dashboard (§12)** — `lifestyle/site/index.html`, burn-vs-income headline, computing its own
   analytics from `data.js` exactly as the Weekly Dashboard does.

**Zero Rust.** Every deliverable is a file in the vault. §13 names where a future GPUI panel attaches;
§14 names the one thing V14 genuinely depends on and does not build.

## 2. Goals & success criteria

- **G1 — The plan is traceable.** Every target in `plan.md` can be pointed back to a sentence the
  user actually said in the interview. No generic goals ("buy a house", "become a millionaire") appear
  unless the user's own vision put them there.
- **G2 — It works with zero connectors.** A vault with no MCP server and no statements completes setup
  and runs the ritual conversationally, producing real `data.js` entries marked `manual`. The MCP path
  is a fast lane, never a prerequisite.
- **G3 — Nothing assumes another Routine.** Installing Lifestyle alone into a bare vault works end to
  end. The daily-note pointer (§11.5) degrades silently when there are no daily notes.
- **G4 — Removal leaves the writing.** Removing the Routine deletes only unmodified shipped files.
  `vision.md`, `plan.md`, `log.md` and `data.js` always survive, because by then they are the user's.
- **G5 — Thock never moves money.** Read-only toward every source, always, even where a write tool
  exists. The ritual records confirmed reality, never intent.
- **G6 — The numbers are never invented.** Missing data is an empty state or a question, never a
  plausible figure.

**Success:** after three months, the user opens `plan.md` more often than the dashboard, and the
dashboard's weekly line is unbroken.

## 3. Non-goals

- **Executing anything.** No transfers, no payments, no writes toward any financial service. The user
  acts in their own apps and tells the ritual what they did (§11.4).
- **Product or security advice.** The Coach names strategy — allocation logic, ordering, thresholds,
  what to build and when — and never a fund, broker, ticker, or account product (§6.3). This is a
  hard rule in `coach.md`, not a disclaimer at the bottom of a page.
- **A full transaction ledger in the vault.** Aggregates plus flagged outliers only (§10.2).
- **Multi-currency.** One currency per vault, set at setup (§9.5).
- **A GPUI finance panel.** VISION §12's "finance-dashboard context view" stays pending; §13 records
  the seam.
- **Tax, estate, or jurisdiction-specific anything.** Out of scope in every skill.
- **Automatic runs.** No service, no timer, no background poll. Every ritual is user-invoked.
- **A shared, vault-level coach.** Considered and rejected (§16 #7): the persona lives inside the
  Routine that ships it, or "modular life" stops meaning anything.

## 4. Core concepts

### 4.1 The Coach is a file, not a prompt

The routine's character is `routines/lifestyle/coach.md` — vault-visible, editor-agnostic, and adopted
by every skill's first instruction. Swapping that file changes the whole routine's voice without
touching a ritual; deleting a rule from it removes that rule from the ritual's behavior the next time
it runs. This is VISION §4.5 applied to personality: *ship great defaults, not hidden behavior*.

It is also declared as the Routine's `agent_doc`, so an agent working anywhere in this Routine reads
it before acting, whether or not a skill was the entry point.

The coach is not a Claude Code subagent and cannot be. A subagent runs autonomously and returns a
report; the interview and the weekly confirmation both need turn-by-turn dialogue with the human.
The persona has to live in the main loop, which is exactly where a skill runs.

### 4.2 The vision comes first, the plan comes after the numbers

Two documents, two cadences:

- `lifestyle/vision.md` — the life, in the user's words. Revisited yearly at most.
- `lifestyle/plan.md` — what money has to do about it. Revised as life changes, by appending dated
  revisions (§8.3).

The plan cannot be written from the interview alone. Setup runs **vision → source → first sync →
plan**, so the plan's first sentence prices a real life against real balances instead of hypotheticals.
A user who stops after the vision has a complete, useful document and a marker telling them where they
left off (§7.4).

### 4.3 One data contract, three ways in

Monarch over MCP, a folder of statements, or nothing at all — the difference between them ends at the
skill boundary. Whatever the source, the agent reads it and writes **one normalized period entry**
into `data.js`. The ritual and the dashboard read only that. Sources become interchangeable, and the
vault keeps working when a source goes away.

```
Monarch MCP  ┐
statements/  ├──► agent normalizes ──► lifestyle/site/data.js ──► ritual + dashboard
you, talking ┘         (§10)                  (the record)
```

### 4.4 Periods are what they are

Bank statements arrive monthly; the user asked for weekly burn. V14 does not resolve that by faking
weeks. Every entry declares its own period (`week`, `month`, or `statement` range) and its own
`quality` (`measured`, `partial`, `manual`). The dashboard compares like with like and renders
non-measured periods visibly differently. A statements-only vault gets an honest monthly line rather
than a fabricated weekly one.

### 4.5 Writing is versioned; money data is not

The split that governs every path in §5:

| Kind | Files | Invisible history |
| --- | --- | --- |
| Writing | `vision.md`, `plan.md`, `routines/lifestyle/*` | **versioned** — this is what restore is for |
| Money | `log.md`, `site/data.js`, `statements/` | **excluded** — balances and bank PDFs never enter a git history |

`accounts.md` sits on the versioned side because it records accounts **by name and role, never by
balance**. §14 covers the mechanism and its honest status.

## 5. Vault layout

```
lifestyle/
  vision.md                the life, in your words                    versioned
  plan.md                  cost → gap → targets → mechanics           versioned
  log.md                   append-only ritual record                  excluded
  statements/              drop PDFs/CSVs here                        excluded
  site/
    index.html             the dashboard (shipped file)               versioned
    data.js                THE numeric record + window.PLAN           excluded

routines/lifestyle/
  routine.toml
  Lifestyle.md             human explainer
  coach.md                 the persona (also the agent_doc)
  accounts.md              accounts by role + rules, no balances
  sources.md               how to get data in this vault
  skills/
    set-up-lifestyle.md
    connect-finance-data.md
    money-ritual.md
    plan-recheck.md
```

## 6. The Coach — `routines/lifestyle/coach.md`

### 6.1 Who it is

A life coach who thinks about money, drawing on Cal Newport's lifestyle-centric planning and Morgan
Housel's psychology of money. It has a view about what a life should be and how money decisions serve
or corrode it. It is explicitly **not** a financial advisor: it does not care where the user's money is
parked and cares enormously what it is for.

### 6.2 The four operative rules

These are behaviors the coach applies, testable in any run — not adjectives.

| Rule | What the coach actually does |
| --- | --- |
| **Independence is the return** | Every recommendation is scored by how much control over the user's own time it buys. Targets are denominated in autonomy (months of runway, hours reclaimed, a year of choice) before they are denominated in dollars. A proposal that raises net worth and lowers optionality gets called out as a cost. |
| **Room for error is a feature** | The plan must carry explicit slack and a survivable-worst-case line. The coach **refuses** to write a plan that only works if things go right, and says so rather than shipping it with a caveat. |
| **Reasonable beats rational** | Proposals are checked against the user's own stated behavior (interview Q9, §7.2). The coach drops a mathematically better idea the user has told it they won't sustain, and names why it dropped it. |
| **Beware the moving goalpost** | When income rises or a windfall lands, the coach asks what the extra bought *in vision terms* before the plan absorbs it. Lifestyle creep and comparison get named out loud, once, without moralizing. |

### 6.3 What it refuses

- Naming a fund, broker, ticker, or account product.
- Tax, legal, estate, or jurisdiction-specific advice.
- Predicting a market, a rate, or a return. Where a projection needs a growth assumption, it states the
  assumption in the sentence and marks the result as an assumption.
- Restating a goal the user did not express. "Buy a house" appears only if the user's own Tuesday has
  a house in it.
- Preaching. One observation per run, maximum (§11.6).

### 6.4 How skills adopt it

Every skill in this Routine opens with a line to the effect of *"Read `routines/lifestyle/coach.md`
first and answer as the coach it describes."* Nothing is duplicated into the skills. The user edits
one file to change all four.

## 7. Set Up Lifestyle

The Routine's `[onboarding]` skill, `kind = "setup"`. One sitting, resumable.

### 7.1 The budget rule

**Ten questions with no right answer.** Confirmations do not count — reviewing a proposal the agent
built from real data is not a question, and the interview leans on that hard (§7.3). Follow-ups do not
count either: the coach may push on a vague answer as many times as it takes. Only a *new topic* spends
budget.

> "I want freedom" → *"Freedom to do what, on a Tuesday, that you can't do now?"* → still free.

### 7.2 The ten questions

Scene questions, not checklist questions. Each covers several of the ten lifestyle dimensions at once.

| # | Question | Covers |
| --- | --- | --- |
| 1 | Is the money in your life shared with anyone — and who has a vote on these decisions? | household framing (§7.5) |
| 2 | Walk me through an ordinary Tuesday ten years from now. Wake to sleep — where are you, who's in the house, what are you doing at 10am, 3pm, 7pm? | schedule control · intensity · work-life balance · where I live · family |
| 3 | In that life, what are you *for*? When someone whose opinion you actually care about describes what you do, what do they say — and why is it them you care about? | importance · prestige · how others think of me · what I'm known for |
| 4 | Who do you see in an ordinary week, and what has to be true for that to be possible? | social life · where I live |
| 5 | Ten years is far enough away to be safe. What has to be **already true** five years from now for that Tuesday to still be reachable? | the 5-year horizon |
| 6 | Now describe five years from now if it went wrong — as concretely as you just described the good one. | anti-vision |
| 7 | What clocks are running that money can't slow down? | non-money deadlines (kids' ages, parents' ages, a body that won't do this at 60) |
| 8 | Of everything you've described, where is today furthest from that Tuesday — and which of those gaps is money actually the lever for? | current-life delta, and the money/not-money separation |
| 9 | Tell me about a money decision that looked wrong on a spreadsheet but was right for you — or one that looked right and you couldn't stick to. | behavioral calibration; feeds *reasonable beats rational* |
| 10 | How should I get your numbers? | data source (§9) |

All ten Newport dimensions are covered by questions 2–4. Question 8 is the hinge of the whole
interview: it is where the coach separates what money can fix from what it can't, and it is the reason
the plan that follows doesn't drift into generic wealth-building.

Questions 1 and 10 are structural and cheap; the eight in between are the interview.

### 7.3 What gets confirmed rather than asked

After the source is wired and the first sync lands (§9), setup **proposes** the account map and the
user reviews it in one structured confirmation, in the grammar V13 established for triage:

> CIBC Chequing → **spending** · CIBC VISA → **spending card, swept to zero weekly** ·
> Wealthsimple CASH (…0ruw) → **reserve, auto-funded** · CIBC Line of Credit → **debt, residual target**
>
> Accept all · fix some · start over

The result is written to `routines/lifestyle/accounts.md` (§9.4). None of it spent a question.

### 7.4 Resumability

Setup is one skill but the user may stop anywhere. Each completed phase writes a marker under
`.thock/state/onboarded/lifestyle/`:

```
vision      written after lifestyle/vision.md exists
source      written after sources.md names a working source
accounts    written after accounts.md is confirmed
plan        written after plan.md and window.PLAN exist
```

Re-running the skill reads the markers, says where the user left off in one sentence, and continues
from there. A user who has only `vision` still owns a finished document. The Routines rail's Setup
row keeps the skill reachable forever.

### 7.5 Household

Question 1 decides it. If money is shared, every subsequent question is reframed as "you and
<partner>", `plan.md` gains a **Decisions that need both of you** section, and the Money Ritual's
*conversations to have* output (§11.6) becomes a standing part of the run rather than an occasional
one. If money is not shared, none of that machinery appears — no empty headings.

## 8. `vision.md` and `plan.md`

### 8.1 `lifestyle/vision.md`

Written in the user's own words wherever possible; the coach's job is to organize, not to improve.

```markdown
# Vision

_Interviewed 2026-08-29. This is your writing — edit it whenever it stops being true._

## The Tuesday (2036)
<the scene, as told>

## What I'm for
<standing, known-for, whose opinion counts>

## The people and the place
<social life, where I live>

## Already true by 2031
<the five-year checkpoint>

## The version where it went wrong
<anti-vision>

## Clocks money can't slow
- <kid> turns 18 in 2038
- <parent> is 71 this year
- <the body / the window>

## Where today is furthest
| Dimension | Today | The Tuesday | Is money the lever? |
| --- | --- | --- | --- |

## How I actually behave with money
<Q9, in the user's words — the coach reads this before every recommendation>
```

### 8.2 `lifestyle/plan.md`

Cost of the vision → gap → targets → mechanics. Every number carries the sentence it came from.

```markdown
# Plan

_Derived from [[vision]] on 2026-08-29, against balances measured 2026-08-29. Currency: CAD._

## What the Tuesday costs
Annual cost of the life described in the vision, built from its parts, each one
citing the line of the vision it serves. Ends with the capital that sustains it,
and the growth assumption used, stated plainly.

## The gap
Where today's income, obligations and balances sit against that. One paragraph.
Names what is already fine, not only what is short.

## Room for error
The survivable-worst-case line. What breaks first, how much slack absorbs it, and
what the plan does NOT depend on going right.

## Targets
| Target | Denominated in | Amount | By | Why (vision line) |
| --- | --- | --- | --- | --- |
| Twelve months of runway | months of the Tuesday | 96,000 | 2028-12-31 | "so I can say no to a bad year" |

## The mechanics
What happens each period, in the user's real accounts — the rules the Money Ritual
executes. Mirrors accounts.md; accounts.md is what the ritual reads.

## Decisions that need both of you
(only when money is shared)

## Questions I'm not qualified to answer
The things to take to a human professional, phrased as questions.

## Revisions
(appended by Plan Re-check — see §8.3)
```

### 8.3 Revision

Plan Re-check **appends** a dated revision below the existing plan. The document grows downward and
nothing the user wrote is rewritten (VISION §4.2).

```markdown
### Revision — 2026-Q4 (2026-11-14)

**What changed in the life:** …
**What changed in the plan:** target "twelve months of runway" moved 2028-12-31 → 2029-06-30, because …
**Still true:** …
```

Because the current plan gets progressively further from the top of a long-lived file, each revision
also updates the `_Derived from…_` line at the head with a pointer to the newest revision — the one
edit-in-place the routine makes, and only to that line.

## 9. Data

### 9.1 The three ways in

| Mode | How | Notes |
| --- | --- | --- |
| **MCP server** | The agent calls whatever finance MCP tools the user has. | Monarch is the worked example with real tool names; **nothing in the Routine depends on it**. Any server that can report balances and transactions qualifies. |
| **Statements folder** | The user drops PDFs or CSVs into `lifestyle/statements/`. | Parsing recipes are learned once per bank (§9.3). |
| **Conversation** | The user tells the coach the few numbers that matter. | Always available. Produces `quality: "manual"` entries. Never a second-class path. |

Modes mix. A vault can have Monarch for cards and a statements folder for a pension.

### 9.2 `routines/lifestyle/sources.md`

Mirrors Timeline's `sources.md` exactly: the skills read only what this file names, never reaching
for a tool just because it is installed.

```markdown
# Lifestyle Sources

How to get this vault's numbers. Edit freely.

## MCP
- Monarch Money — `mcp__Monarch_Money__get_accounts`, `get_budgets`, `get_transactions`

## Statements
- lifestyle/statements/ — see recipes below

## Recipes
### CIBC chequing PDF (`cibc-chq-*.pdf`)
Transactions start after the "Date Description Withdrawals" header; the closing
balance is the last "Closing balance" line. Amounts in the Withdrawals column are
negative. Categorize by merchant using the map in accounts.md.
```

### 9.3 Learning a bank once

The first time an unrecognized statement format appears, the agent works out how to read it, shows the
user a sample of what it extracted, gets a confirmation, and **appends the recipe to `sources.md`**.
Later runs follow it silently. If a recipe stops matching, the ritual says so and re-derives rather
than guessing — a changed statement layout is a visible event, never a silent mis-parse.

### 9.4 `routines/lifestyle/accounts.md`

What makes a generic ritual possible: the ritual is the same for everyone, the file carries the
specifics. **Names and roles only — no balances**, which is what keeps it on the versioned side of
§4.5.

```markdown
# Accounts

Roles: spending · reserve · debt · investment · income

| Account | Role | Rule |
| --- | --- | --- |
| CIBC Chequing | spending | keep a $200 operational floor |
| CIBC VISA | spending card | sweep to zero each period |
| Wealthsimple CC | spending card | sweep to zero each period |
| Wealthsimple CASH …0ruw | reserve (rent) | auto-funded; NOT available to sweep |
| CIBC Line of Credit | debt | residual: everything above the floor after sweeps |

## Order of operations
1. Sweep spending cards to zero.
2. Refill any reserve that is short.
3. Residual to debt, above the floor.

## Never
- Reserve balances are never available for sweeps or debt payments.
- Never propose a payment that takes chequing below the floor.
```

The user edits this by hand and the ritual follows it the next run. There is no re-setup.

### 9.5 Currency

One currency per vault, chosen at setup, recorded in `accounts.md` and stamped on every `data.js`
entry. Foreign-currency accounts are the user's to convert before recording; the coach says so once
rather than silently mixing units.

## 10. `lifestyle/site/data.js` — the record

One file. It is the numeric truth, it is what the dashboard reads, and it is plain text the user owns.
There is no second numeric record: `log.md` narrates the ritual and cites figures, but it is not a
parallel ledger.

### 10.1 Shape

```js
window.CONFIG = { currency: "CAD" };

window.PLAN = {
  revised: "2026-08-29",
  horizons: { five_year: "2031", ten_year: "2036" },
  annual_cost_of_vision: 96000,
  assumption: "3% real growth",
  targets: [
    { id: "runway",  label: "Twelve months of runway", unit: "CAD", target: 96000, by: "2028-12-31" },
    { id: "loc",     label: "Line of credit cleared",  unit: "CAD", target: 0,     by: "2027-12-31" },
    { id: "savings", label: "Savings rate",            unit: "rate", target: 0.30, by: null }
  ]
};

window.PERIODS = [
  {
    id: "2026-W35",
    period:  { kind: "week", start: "2026-08-24", end: "2026-08-30" },
    quality: "measured",                    // measured | partial | manual
    income:  4210,
    spend:   { total: 3180, categories: { groceries: 612, dining: 188, kids: 340 } },
    balances:{ chequing: 2140, visa: -1203, loc: -18400 },
    outliers:[ { label: "Bike repair", amount: 420, category: "shopping" } ],
    actions: [ { label: "Paid CIBC VISA to zero", amount: 1203 } ],
    progress:{ runway: 41200, loc: -18400, savings: 0.24 },
    amended: null                           // or the date it was corrected
  }
];
```

`progress` is keyed by target id, so the dashboard renders plan progress without ever reading
`plan.md`. Setup writes `window.PLAN`; Plan Re-check rewrites it; the Money Ritual appends to
`window.PERIODS` and never touches `window.PLAN`.

### 10.2 Granularity

Aggregates plus flagged outliers. Category totals, balances, income, and the handful of transactions
the ritual surfaced (unplanned above a threshold, or a category blowout) — enough to explain a bad
period a year later, without a full ledger's privacy surface.

### 10.3 Re-runs and amendments

`window.PERIODS` is append-only **except** for a re-run of a period that already has an entry. Then
the entry is replaced with corrected numbers, `amended` is stamped with the date, and `log.md` records
that it was amended and why. The append-only truth is the log; `data.js` always reads as the current
best understanding.

### 10.4 Validity

After any write the skill verifies the file still parses:

```bash
node -e "global.window={};require('./lifestyle/site/data.js');console.log(window.PERIODS.length,'periods')"
```

A failed parse is reported to the user, not swallowed.

## 11. The Money Ritual

`routines/lifestyle/skills/money-ritual.md`, `kind = "ritual"`. Named for what it does, not for a day
— nobody else's Friday is the user's Friday. It runs whenever invoked, notes in passing if it has been
much longer than a period since the last entry, and never scolds.

### 11.1 Read

`coach.md`, `plan.md` (targets and mechanics), `accounts.md`, `sources.md`, the last few `data.js`
entries, and then the source itself.

### 11.2 When there is no data

The ritual **degrades to a conversation** rather than failing. It asks for the few balances that
actually matter to the accounts' rules, records a `quality: "manual"` entry, and says plainly that
this period is self-reported. The habit survives a broken connector. What it never does is estimate a
number the user didn't give it.

A source that fails with an auth or network error is named explicitly, with what to re-run — never
dropped silently.

### 11.3 Propose

Execute `accounts.md`'s order of operations against the current balances and present the result as an
ordered action list with the post-ritual state spelled out. If the rules can't be satisfied — the floor
would be breached, a card can't be cleared — the ritual **stops and says so**, and does not reach into
a reserve to make the arithmetic work. Overspending is a fact to surface, not a hole to paper over.

### 11.4 Confirm, then log

Thock cannot move money and says so. The user acts in their own apps, then confirms in a single
structured question — *accept all · some · none* — and only confirmed actions are recorded. Nothing is
written before that answer.

### 11.5 Write

1. Append or amend the period entry in `data.js` (§10.3).
2. Append the run to `lifestyle/log.md`: date, actions confirmed, watch list, conversations, and any
   amendment note.
3. If daily notes exist in this vault, append a one-line `# Money` entry to today's note with a
   `[[wikilink]]` to the log. If they don't, skip it silently — G3.

### 11.6 Say

Four outputs, in this order:

- **Plan drift** — on / ahead / behind, in the plan's own terms and against its own targets. Never a
  generic benchmark.
- **Vision tie-back** — one line connecting this period's money to the life. *"This week bought nine
  days of the 2031 runway."* This is the sentence that makes it not a budgeting app.
- **Watch list** — one to three things trending wrong.
- **Conversations to have** — decisions needing a partner before the next run, surfaced explicitly
  rather than buried in advice. Only when money is shared (§7.5).

Then, at most, **one coach observation** — and only when the data actually supports one. Silence is
the default, not filler. The weekly run stays fast and factual; the coach speaks fully in setup and in
Plan Re-check, where there is room for it.

Finally, if the last recorded plan revision is older than the configured interval, one line:
*"Plan last revised seven months ago — run Plan Re-check?"* No timers, no service.

## 12. The dashboard

`lifestyle/site/index.html`, a self-contained page in the Weekly Dashboard's idiom: same One-Dark-ish
palette, same light/dark handling, computes everything itself from `data.js`, opens through a
`kind = "browser"` link.

**Headline: burn vs. income.** The period's spend against the period's income, and the gap — because
the gap is what is actually buying the future. Below it:

1. **Burn sparkline** across periods, with `manual` and `partial` periods rendered visibly differently
   from `measured` ones so an estimate is never mistaken for a measurement.
2. **Category breakdown with drift** — this period against the trailing average, biggest movers first.
3. **Plan targets progress** — one row per `window.PLAN` target: current, target, and the date it
   lands at the current rate.

The page starts empty with a one-line "run the Money Ritual to fill this in" state, and never assumes
`window.PLAN` exists.

**Not on the page:** the vision itself. It stays in `vision.md`, where it is read deliberately rather
than glanced at.

## 13. Plumbing

### 13.1 `routines/lifestyle/routine.toml`

```toml
schema    = 2
id        = "lifestyle"
name      = "Lifestyle"
version   = 1
summary   = "Money in the service of a life you named — a vision, a plan, and a weekly ritual."
icon      = "person"
doc       = "routines/lifestyle/Lifestyle.md"
agent_doc = "routines/lifestyle/coach.md"

[[link]]
name = "Plan"
open = "lifestyle/plan.md"
kind = "editor"

[[link]]
name  = "Vision"
open  = "lifestyle/vision.md"
kind  = "editor"
group = "The long view"

[[link]]
name = "Money Dashboard"
open = "lifestyle/site/index.html"
kind = "browser"

[[scaffold]]
kind = "dir"
path = "lifestyle"

[[scaffold]]
kind = "dir"
path = "lifestyle/statements"

[[scaffold]]
kind   = "file"
path   = "lifestyle/site/index.html"
source = "assets/index.html"

[[scaffold]]
kind   = "file"
path   = "lifestyle/site/data.js"
source = "assets/data.seed.js"

[[scaffold]]
kind   = "file"
path   = "routines/lifestyle/coach.md"
source = "coach.md"

[[skill]]
id      = "money-ritual"
name    = "Money Ritual"
file    = "routines/lifestyle/skills/money-ritual.md"
summary = "Read the numbers, propose the moves, tie them back to the life, log what you did."
reads   = ["lifestyle/**", "routines/lifestyle/**", "the sources sources.md names"]
writes  = ["lifestyle/site/data.js (append or amend)", "lifestyle/log.md (append)", "daily/<today>.md (append, if daily notes exist)"]

[[skill]]
id      = "plan-recheck"
name    = "Plan Re-check"
file    = "routines/lifestyle/skills/plan-recheck.md"
summary = "Has the life changed? Revise the plan against it."
reads   = ["lifestyle/**", "routines/lifestyle/**"]
writes  = ["lifestyle/plan.md (append a revision)", "lifestyle/site/data.js (window.PLAN)"]

[[skill]]
id      = "connect-finance-data"
name    = "Connect Finance Data"
kind    = "setup"
file    = "routines/lifestyle/skills/connect-finance-data.md"
summary = "Wire up where the numbers come from — an MCP server, a folder of statements, or you."
reads   = ["routines/lifestyle/sources.md", "lifestyle/statements/**"]
writes  = ["routines/lifestyle/sources.md", "routines/lifestyle/accounts.md"]

[[skill]]
id      = "set-up-lifestyle"
name    = "Set Up Lifestyle"
kind    = "setup"
file    = "routines/lifestyle/skills/set-up-lifestyle.md"
summary = "Ten questions about the life you want, then a plan built backwards from it."
reads   = ["lifestyle/**", "routines/lifestyle/**", ".thock/config.toml"]
writes  = ["lifestyle/vision.md", "lifestyle/plan.md", "routines/lifestyle/{sources,accounts}.md", "lifestyle/site/data.js (window.PLAN)", ".thock/state/onboarded/lifestyle/*"]

[onboarding]
skill = "routines/lifestyle/skills/set-up-lifestyle.md"
```

No `model = "fast"` anywhere: every skill here is judgment, not filing.

### 13.2 How the rail renders it

**Notes:** Plan · Money Dashboard. **The long view** (collapsed): Vision. **Rituals:** Money Ritual ·
Plan Re-check. **Setup** (collapsed): Connect Finance Data · Set Up Lifestyle.

`log.md` deliberately gets no row — it is reached from the daily note's pointer or the project panel,
and the rail is worth more than the archive.

### 13.3 Installation

Offered in **Add Routine**, not default-installed. Nothing about a vault implies its owner wants to be
interviewed about their life.

### 13.4 Removal

Standard V7 rules: only declared files unmodified since activation are deleted. In practice that means
`index.html` and the seed `data.js` if untouched; `vision.md`, `plan.md`, `log.md`, a written-to
`data.js`, `coach.md` once edited, `accounts.md` and `sources.md` all survive — they are user content
by then (G4).

### 13.5 The panel seam (not built)

VISION §12's finance context view would be a right-dock panel that follows the active editor item and
renders `data.js` for the open period — the same relationship the Day Planner has to a daily note. It
would need a `data.js` reader in `crates/thock/`, which is exactly the thing V14 avoids. The `data.js`
schema in §10.1 is the contract it would read; nothing else has to change.

## 14. Privacy and invisible history — the one real dependency

§4.5 promises that balances and bank PDFs never enter the vault's history. The checkpoint service that
would honor that is still *in progress* in VISION §12, and no per-path exclusion mechanism exists yet.

**V14 declares the intent and ships inert.** Set Up Lifestyle writes a vault-root `.thockignore`
(create-if-missing, append-only, never deleted on removal) containing:

```
lifestyle/statements/
lifestyle/site/data.js
lifestyle/log.md
```

Until the checkpoint service reads that file, nothing in the vault is versioned anyway, so the promise
holds by accident and becomes real when V2 lands. This is stated plainly in `Lifestyle.md` rather than
implied.

**Risk, stated honestly:** V2 owns the final mechanism and may choose a different one. If it does,
`.thockignore` becomes a compatibility shim or a migration. The alternative — reaching into the hidden
history git-dir's exclude file from a Routine — was rejected as a Routine touching another subsystem's
internals. Blocking V14 on V2 was rejected as unrelated work gating a self-contained routine.

## 15. Implementation notes

**Everything here is prose.** There is no Rust in V14 and no test suite in the usual sense. The
verification path is the dogfood vault:

- Install into `~/Thock` from Add Routine and run Set Up Lifestyle end to end, once with the Monarch
  MCP wired and once with it deliberately absent (G2).
- Run the Money Ritual in a bare vault with no Timeline installed and confirm the daily-note pointer
  is skipped without complaint (G3).
- Re-run the ritual for a period that already has an entry and confirm the amendment path (§10.3).
- Remove the Routine and confirm every written file survives (G4).
- Validate `data.js` with the §10.4 one-liner after each run.

Two things worth getting right the first time:

- **`window.PLAN` and `plan.md` are written in the same step, always.** Setup and Plan Re-check each
  author both. If they ever drift, the dashboard shows targets the plan no longer holds, and the
  routine's central claim — that the numbers serve a named life — quietly becomes false.
- **The `progress` map is keyed by target id, and target ids are stable across revisions.** A revision
  that renames a target keeps its id, or the dashboard's history for that row breaks.

## 16. Decision log (design interview, 2026-08-29)

| # | Decision | Rejected alternatives, and why |
| --- | --- | --- |
| 1 | **Data source first, then the plan** | *Plan from the interview alone*: setup finishes in one sitting with no connector, but the plan's numbers would be hypothetical and the "cost of the vision" exercise (§8.2) would price a life against a guess. *Targets-only, no figures*: honest, and it produces a plan nobody can act on. |
| 2 | **Ten questions total, across all of setup** | *Ten for the vision, a separate budget for mechanics*: richer vision, longer setup, and it turns a single sitting into a project. Making confirmations and follow-ups free (§7.1) recovers most of the depth without spending budget. |
| 3 | **Scene questions over dimension questions** | *One question per Newport dimension*: complete coverage, ten shallow answers, and an interview that feels like a form. *Adaptive from a checklist*: flexible, and it makes the ritual unreproducible — two users get two different products. |
| 4 | **Push hard; follow-ups are free** | *One push per question*: bounded and comfortable, and it lets "I want freedom" survive into the plan. The risk accepted is an interview that runs long; the coach's job is to notice when it has enough. |
| 5 | **`data.js` is the only numeric record** | *Normalized markdown snapshots per period*: reads better in any editor, and it creates two representations of the same numbers with no mechanism keeping them equal. *Both, appended together*: the same divergence, wearing more discipline. `log.md` narrates and cites; it does not tally. |
| 6 | **Period-agnostic entries** | *Weekly spine with estimated fills*: gives everyone a weekly line, and a monthly statement allocated across four weeks is a number nobody measured. *Weekly requires a live source*: honest, and it makes two different products out of one routine. |
| 7 | **The coach lives inside the Routine** | *A vault-level coach other Routines adopt* (considered, then reversed by Diego): a persona that outlives any one Routine — and a Routine's file reaching outside itself, which is the exact shape "modular life" forbids. If a second Routine ever wants this voice, it copies the file. |
| 8 | **`coach.md`, not a `.claude/agents/` definition** | *Ship the coach as a Claude Code subagent*: a standing persona summonable outside a ritual — but a subagent runs autonomously and reports back, so it **cannot conduct an interview**, and `.claude/` is one vendor's path in a BYO-LLM product. Routines are forbidden from writing under `.claude/` anyway. |
| 9 | **Read-only toward every source, always** | *Read-only now, execution seam documented*: identical behavior today, and it leaves a door in the file that someone eventually walks through. Thock proposing money movements it cannot execute is the feature, not a limitation to route around. |
| 10 | **Writing versioned, money data excluded** | *All of `lifestyle/` excluded* (Diego's first answer, reversed on the consequence): maximum privacy, and `vision.md` and `plan.md` — the least replaceable writing in the vault — would have no restore when an append goes wrong. *Nothing excluded*: bank PDFs in a git history forever. |
| 11 | **Operator weekly, coach on cadence** — since softened to one earned observation per run (§11.6) | *Full coach every run*: the richest version, and the fastest way to stop running a ritual that is supposed to take five minutes. *Never coach weekly*: keeps it fast, and severs the weekly numbers from the reason they exist. |
| 12 | **Named "Lifestyle"** | *"Finance"*: matches VISION's existing language and undersells a routine whose first act is a life interview. *"Money & Life"*: names both halves, reads like a magazine. *"Wealth"*: Housel's word, and it reads as aspirational finance to most people. |
| 13 | **Monarch as worked example, generic contract** | *A first-class Monarch adapter*: best experience for the author today, and a maintenance surface tied to one vendor's MCP. *No vendor names at all*: zero coupling, and every user re-derives what one worked example would have given them. |
| 14 | **Single currency per vault** | *Per-account currency with a display currency*: correct for cross-border households, and it drags in rate freshness, rounding, and a conversion the coach would have to defend in every total. Deferred until someone actually needs it. |

## 17. Deferred

- **The finance context panel** (§13.5) — the `data.js` schema is its contract.
- **Multi-currency** (§16 #14).
- **A shared persona across Routines** (§16 #7) — if journaling or career Routines ever want this
  voice, the question reopens with a real second user.
- **Windfall and structural-change handling** — a raise, an inheritance, a job loss deserve their own
  ritual rather than a Plan Re-check that happens to run afterward. The *moving goalpost* rule (§6.2)
  is the seam it would attach to.
