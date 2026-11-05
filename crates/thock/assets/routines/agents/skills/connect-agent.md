# Connect an agent

Pair another machine with this vault so the agent running there (Claude Code,
Codex, Gemini CLI, or any MCP client) can read what the vault knows and leave
notes in it. Thock does the serving and the pairing; this ritual's job is to
walk the person through the three commands and explain what they will see.

**Reads:** `routines/agents/AGENT.md`.
**Writes:** nothing. The desk writes `.thock/state/bridge/` on its own when a
machine pairs; you never touch `.thock/`.

> **Do not handle codes or tokens yourself.** The pairing code is shown by the
> app and typed on the other machine; the token lives there. Never ask the
> person to paste either into the chat.

## 1. Start the pairing on the desk

1. Ask the person to run **Connect an agent** — the status row of the Agents
   section in the panel (press `enter` on it), or `thock: connect agent` from
   the command palette. A command appears:
   `thock-bridge pair palloma.local:7557 maple-river-stone-ocean-lamp-fox`.
   `up`/`down` switch between the desk's addresses when `.local` names don't
   resolve on their network; `enter` copies it.
2. If the row says **Not serving: port 7557 is in use**, another program has
   the port: change `port` in `.thock/routines/agents/bridge.toml` (suggest
   the line; they edit it) and the desk rebinds on save.

## 2. Run it on the other machine

1. `thock-bridge` ships beside the `thock` command (Thock installs both with
   *Install CLI*). On a machine without the app, download
   `thock-bridge-<os>-<arch>` from the release page, `chmod +x` it, and put
   it on the path.
2. Run the copied command there. It answers `Paired with <vault> on <desk>`
   and the modal on the desk closes by itself. The code is spent; a second
   machine needs a new one.
3. Then `thock-bridge connect claude` (or `codex`, `gemini`). It prints every
   file it would change and asks before each: the `thock-memory` skill under
   `~/.agents/skills/`, a link into that CLI's skills folder, the CLI's own
   `mcp add` for the `thock` server, and for Claude Code the
   `autoMemoryDirectory` setting pointing at the mirror folder. Other MCP
   clients get the config block printed to add by hand.

## 3. Check it worked (only if asked)

- The panel's **Connected agents** row lists the machine with when it last
  called. `thock-bridge status` there says whether the desk is reachable, how
  old the cache is and how many lines are queued.
- In the other agent, ask it what the vault knows; it should call `context`
  and answer from `memory/index.md`.

## 4. Explain the rules (only if asked)

- Two writes, both appends: one dated line at the end of `memory/inbox.md`,
  and lines under `## From your agents` in today's note. Nothing else.
- Lines from an agent carry `via <harness>@<machine>`; Reflect files them with
  that provenance, below the person's own words when they disagree.
- The do-not-keep list in `profile.md` applies; the desk refuses a line that
  touches it, or that looks like a credential.
- With the desk off, the other machine answers from its cache and queues its
  writes; they land, once and in order, when the desk is back.
- **Disconnect agent** in the palette forgets a machine; **Toggle Bridge**
  pauses serving without removing the Routine.
