# Agents

You probably run another agent for work: Claude Code on a build machine, Codex
in a repo, Gemini in a terminal. It learns things about you and keeps them in
its own memory, where this vault never sees them, and it can't answer "what
did I ship this week" from your notes either. This Routine is the bridge.

While Thock is open with this Routine installed, the app serves what the vault
knows on your local network, and a small program, `thock-bridge`, lets any
agent that speaks MCP reach it. From any machine on your network an agent can:

- **`context`** — see what the vault knows: today's date, where today's and
  this week's notes live, `memory/index.md`, your Routines, your language, and
  the topics you asked Thock never to keep.
- **`search`**, **`read`**, **`recent`** — look through your notes.
- **`remember`** — leave one dated line in `memory/inbox.md`, marked with the
  agent's name (`via claude-code@robin`). The Reflect ritual files it like any
  other line, with that provenance.
- **`note_today`** — add lines under **`## From your agents`** in today's note,
  each prefixed with the agent's name: "Shipped the Plus PR; CI green."

That is the whole of what it can write. Nothing rewrites, deletes or renames
your words.

## Connecting a machine

1. Run **Connect an agent** from the Agents section of the panel, or from the
   command palette. It shows a command with a one-time code.
2. On the other machine, with `thock-bridge` on the path (it ships beside the
   `thock` command; a fleet machine without the app can download it from the
   release page), run that command. The machine is paired; the code is spent.
3. Then `thock-bridge connect claude` (or `codex`, `gemini`). It installs the
   `thock-memory` skill, registers the `thock` MCP server with that CLI, and
   for Claude Code offers to keep its auto memory in a folder that mirrors into
   this vault. Everything it would change is printed first.

The section's status row says what the desk is doing: `Serving on
palloma.local:7557 · 2 agents`, `Not serving: port 7557 is in use`, `Paused`.
**Connected agents** lists each paired machine with when it last called;
open one (enter, or a click) for its details — when it paired, which agent
called last, the address it reached — and a **Disconnect** that asks first.
A disconnected machine can pair again any time with a new code.

## What lives in the vault

- `routines/agents/skills/thock-memory/SKILL.md` — the protocol the agent
  follows, in words: when to look at memory, how to leave a line, what never
  to record. Yours to edit; `thock-bridge update` on the other machine fetches
  your version.
- `memory/agents/<harness>/<machine>/` — a mirror of the agent's own memory
  files, when you pointed it there. Read-only to every ritual except Reflect,
  which may file a fact from there *about you* with its provenance, ranked
  below your own words when they disagree.
- `.thock/routines/agents/bridge.toml` — the port, the bind address, and the
  daily-note heading (`From your agents` unless you change it).

## When the desk is off

The other machine keeps a cache of the last `context`, the notes it read and
the searches it ran, and answers from it, marked as such. `remember` and
`note_today` queue, in order, and land the next time the desk is reachable.
Nothing is lost and nothing lands twice.

## Trust

Pairing is a human act: a code shown here, typed there, good for ten minutes
and once. The desk keeps only a hash of each machine's token. The connection
is plain HTTP on your own network, like the phone's pairing trusts the room;
don't pair across a network you don't trust. **Pause** (the command palette's
Toggle Bridge) stops serving without removing the Routine; removing the
Routine stops it and leaves every note as it was.
