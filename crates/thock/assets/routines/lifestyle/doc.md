# Lifestyle

Money in the service of a life you named. This Routine is deliberately **not a budgeting
app** — a budgeting app knows what you spent; this one knows what you spent it *for*. It
starts from a concrete description of the life you want, works backwards to what money has
to do about it, and then keeps the two honest with a short weekly ritual.

## The three documents

- **`lifestyle/vision.md`** — the life, in your own words: an ordinary Tuesday ten years out,
  what you're for, who you see in a week, and the version where it went wrong. Written once
  in the setup interview, revisited yearly at most. It is your writing — edit it whenever it
  stops being true.
- **`lifestyle/plan.md`** — what money has to do about it. Cost of the vision → the gap →
  dated targets → the mechanics that run each week. Every number carries the sentence of the
  vision it came from. Revisions are **appended**, dated, below the original — nothing you
  wrote is ever rewritten.
- **`lifestyle/log.md`** — the append-only record of every ritual run: what was proposed,
  what you confirmed you actually did, and what to watch.

## The coach

Every ritual here answers as the coach described in `routines/lifestyle/coach.md` — a life
coach who thinks about money, in the tradition of Cal Newport's lifestyle-centric planning
and Morgan Housel's psychology of money. It measures money in autonomy before dollars,
refuses plans that only work if things go right, and never names a fund, broker, or ticker.
The file is yours: edit a rule and every skill follows the new one on its next run.

## The Money Ritual

Run **Money Ritual** whenever you sit down with your money — it's named for what it does,
not for a day. It reads your numbers, executes the rules in
`routines/lifestyle/accounts.md` (sweep the cards, refill the reserve, residual to debt),
and proposes the period's movements. **Thock never moves money.** You act in your own apps,
confirm what you actually did, and only that gets recorded — to `lifestyle/site/data.js`
(the one numeric record) and to the log. Then it tells you four things: plan drift, what
this period bought in vision terms, a short watch list, and — when money is shared — the
conversations to have.

## Where the numbers come from

Three ways in, all equal, declared in `routines/lifestyle/sources.md`:

- **An MCP server** (Monarch Money is the worked example — any server that reports balances
  and transactions qualifies).
- **A folder of statements** — drop PDFs or CSVs into `lifestyle/statements/`; the first
  time a new bank format appears, the ritual learns to read it and writes the recipe down.
- **You, talking** — tell the coach the few balances that matter. Entries are marked
  `manual`, and that is a first-class path, not a fallback.

Whatever the source, the ritual writes one normalized period entry into `data.js`, and the
**Money Dashboard** (`lifestyle/site/index.html`) computes everything from that file alone:
burn vs. income, category drift, and progress toward the plan's own targets. Estimated
periods render visibly differently from measured ones — an estimate is never dressed up as
a measurement.

## Privacy, stated plainly

Your writing (`vision.md`, `plan.md`, this Routine's files) belongs in invisible history —
that is what restore is for. Your money data does not: setup writes a vault-root
`.thockignore` naming `lifestyle/statements/`, `lifestyle/site/data.js`, and
`lifestyle/log.md` so balances and bank PDFs never enter a version history. **Honest
status:** the checkpoint service that reads that file is still in progress; until it lands,
nothing in the vault is versioned anyway, so the promise holds — and `.thockignore` makes
it real the day checkpointing arrives.

## Make it yours

- `routines/lifestyle/coach.md` — the persona. Edit one file, change every ritual's voice.
- `routines/lifestyle/accounts.md` — your accounts by name and role (never balance) and the
  rules the ritual executes. Edit it by hand; the next run follows it. No re-setup.
- `routines/lifestyle/sources.md` — where the numbers come from, including the statement
  recipes the ritual has learned.
- Removing the Routine deletes only unmodified shipped files. `vision.md`, `plan.md`,
  `log.md`, and your `data.js` always survive — by then they are yours.
