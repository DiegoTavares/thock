# Connect Finance Data

Wire up where this vault's numbers come from — an MCP server, a folder of statements, the
user themself, or any mix — then run a first sync and confirm the account map. Runs inside
Set Up Lifestyle, and stands alone whenever a source changes later.

**Read `routines/lifestyle/coach.md` first and answer as the coach it describes.**

**Reads:** `routines/lifestyle/sources.md`, `lifestyle/statements/`, whatever source the
user names.
**Writes:** `routines/lifestyle/sources.md`, `routines/lifestyle/accounts.md`,
`.thock/state/onboarded/lifestyle/{source,accounts}`.

> **Read-only toward every source, always.** Never call a tool that writes, transfers, or
> modifies anything in a financial service, even where one exists. Thock reads and records;
> the user acts.

## 1. Ask how the numbers should arrive

One question, three honest answers (modes mix — a vault can have an MCP server for cards
and a statements folder for a pension):

| Mode | What it means |
| --- | --- |
| **MCP server** | The agent calls whatever finance MCP tools the user has connected. Monarch Money is the worked example; any server that can report balances and transactions qualifies. |
| **Statements folder** | The user drops PDFs or CSVs into `lifestyle/statements/`; parsing recipes are learned once per bank (§4). |
| **Conversation** | The user tells the coach the few numbers that matter, each run. Entries are marked `quality: "manual"`. Never a second-class path — never stall waiting for a connector. |

Also ask (or confirm from the data): **which single currency this vault keeps its books
in**. Foreign-currency accounts are the user's to convert before recording — say so once,
rather than silently mixing units.

## 2. Write `routines/lifestyle/sources.md`

The skills read only what this file names — never reach for a tool just because it is
installed. Create or update it in this shape:

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

List only the modes the user actually chose. Then write the empty marker
`.thock/state/onboarded/lifestyle/source` (create directories as needed).

## 3. First sync

Pull current balances (and recent transactions where available) from every named source.
Conversation-mode: ask for the balances that matter, and take exactly what is given — a
number the user didn't state does not exist. A source that fails with an auth or network
error is named explicitly, with what to re-run — never dropped silently, never papered
over with an estimate.

## 4. Learning a bank once (statements mode)

The first time an unrecognized statement format appears: work out how to read it, show the
user a **sample of what you extracted**, get a confirmation, and append the recipe to
`sources.md`. Later runs follow the recipe silently. If a recipe stops matching, say so
and re-derive with the user rather than guessing — a changed statement layout is a visible
event, never a silent mis-parse.

## 5. Propose the account map

From the synced data, **propose** — don't interview. Present every discovered account with
a role (`spending · reserve · debt · investment · income`) and a rule, in one structured
confirmation:

> CIBC Chequing → **spending** · CIBC VISA → **spending card, swept to zero weekly** ·
> Wealthsimple CASH (…0ruw) → **reserve, auto-funded** · CIBC Line of Credit → **debt,
> residual target**
>
> Accept all · fix some · start over

Use your structured question tool if you have one (AskUserQuestion in Claude Code, for
example), otherwise a plain numbered question — a single question with "Accept all" and
"Fix some" options; re-assignments and prose land in "Other". Nothing is written before
the answer.

## 6. Write `routines/lifestyle/accounts.md`

Record the confirmed map — **names and roles only, never balances** (that is what keeps
this file safely versionable). Shape:

```markdown
# Accounts

Currency: CAD
Roles: spending · reserve · debt · investment · income

| Account | Role | Rule |
| --- | --- | --- |
| CIBC Chequing | spending | keep a $200 operational floor |
| CIBC VISA | spending card | sweep to zero each period |
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

The order of operations and the Never list come from the user's confirmed rules — propose
sensible ones from the roles, confirm, and write. The user edits this file by hand later
and the Money Ritual follows it the next run; there is no re-setup.

Finish with the empty marker `.thock/state/onboarded/lifestyle/accounts`, and tell the
user what was wired, in one sentence.
