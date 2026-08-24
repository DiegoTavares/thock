# Set Up Inbox

A short, direct interview — six questions, once — whose answers become an **editable file**
rather than hidden behavior: `routines/inbox/triage-policy.md`, the judgment the Triage Inbox
ritual follows. Editing that file later changes the ritual with no re-setup.

**Reads:** the vault's folder layout, `routines/` (which Routines are installed),
`.thock/config.toml` (note and backlog paths).
**Writes:** `routines/inbox/triage-policy.md`, `.thock/state/onboarded/inbox`.

> Ask the questions **one at a time**, offering concrete options drawn from what actually
> exists in this vault — never a hypothetical folder. Short answers are fine; silence on a
> question keeps the shipped default for that row.

## 1. Look around first

Scan the vault's top-level folders and `routines/*/routine.toml` so every option you offer is
real: which Routines are installed, which folders hold notes, whether a reading-list note
already exists, and what the daily note's sections are called.

## 2. The six questions

1. **Where may triage file notes?** Offer the folders and Routines that exist. The answer
   bounds the "kept as a note" destination.
2. **Where do links-to-read go?** A reading-list note (name it), Backlog · Someday, or a
   folder.
3. **Where do raw ideas go?**
4. **What does "act on this soon" mean here?** Backlog · Soon, or today's note — and if the
   note, which section.
5. **Anything that should always be discarded on sight?** (Newsletters, receipts, empty
   captures…)
6. **When triage is unsure — ask, or default to Someday?**

## 3. Write the policy

Rewrite `routines/inbox/triage-policy.md` from the answers, keeping its shipped shape: a
`## Proposals` table of *if it looks like → propose*, a `## Destinations` paragraph naming the
real paths, and a `## When unsure` paragraph recording question 6's answer. Plain Markdown,
no front matter — the user will edit this file by hand.

## 4. Mark setup done

Write an empty file at `.thock/state/onboarded/inbox` (create directories as needed) so Thock
knows the interview ran. Then show the user the policy you wrote and remind them it's theirs
to edit — and that the phone-side transports (Google Tasks, the `thock/inbox` Gmail label) are
connected separately via **`thock: connect google workspace`**.
