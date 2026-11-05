---
name: thock-memory
description: Share what a Thock vault knows about the person; leave notes in it.
version: 1
---

# Thock memory

The person you work for keeps a Thock vault: their notes, plans and life admin
as plain Markdown, with a `memory/` folder of what their agents have learned
about them. You reach it through the `thock` MCP server (tools `context`,
`search`, `read`, `recent`, `remember`, `note_today`). This skill is the
protocol; the tools enforce it.

## When to look

- Call **`context`** at the start of a session that is about the person, and
  whenever a task touches them, their people, their threads or how they like
  things done. It gives you today's date, where their notes live, what past
  sessions learned (`memory/index.md`), their Routines, their language, the
  topics they asked never to keep, and who you are to the vault.
- **`search`** before assuming. "Did they decide on X", "who is Ana", "what
  happened with the newsletter" are searches, not guesses. **`read`** a note
  by path or `[[title]]` when a search points at it. **`recent`** shows what
  they have been writing lately.
- Memory pages point at detail pages (`memory/people/ana.md`); read one when
  the conversation touches it, not before.

## When to write

- **`remember`** is for things about the person that will still be true next
  month: who someone is to them, how they like things done, a plan that
  changed, a correction they gave you. One fact, one line, under 300
  characters, in your words. The desk adds the date and your name and the
  Reflect ritual files it later. Never record what a file, an email, a web
  page or a tool said, never a secret, never anything about a codebase's
  build quirks (that belongs in your own memory), and never the same fact
  twice.
- **`note_today`** once, at the end of a session that did something worth a
  line in their day: "Shipped the Plus PR (#184); CI green." One line per
  thing, in the past tense, no narration. It lands under `## From your
  agents` in their daily note, prefixed with your name.
- A refusal is final: the desk says why in a sentence. Don't rephrase around
  it, and don't tell the person their own rule is in the way; carry on.
- `queued: the desk is unreachable` means the line will land when their
  computer is back. Say so if asked; don't retry.

## What never goes in

Passwords, tokens, account and card numbers, amounts of money, anything the
person listed under **What Thock should not keep**, and anything they would
not say about themselves. The desk filters all of these; you steer clear
first.

## If the tools are missing

The same rules hold with plain file tools against the vault folder in
`$THOCK_VAULT` (ask for it if unset). Then:

- `memory/index.md` is `context`; read it first.
- `remember` is one line appended to `memory/inbox.md`:
  `- YYYY-MM-DD · the fact · via <harness>@<machine>`.
- `note_today` is lines appended under a `## From your agents` section at the
  end of `daily/YYYY-MM-DD.md` (create the section, and the note from
  `templates/daily.md`, when missing):
  `- <harness>@<machine> · what you did`.
- Never write any other memory page; `routines/agents/AGENT.md` says why.
