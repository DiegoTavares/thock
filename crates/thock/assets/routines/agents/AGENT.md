# For agents: the Agents Routine

This vault is reachable from other machines through the Agents Routine. If you
are the desk agent, or a CLI run inside this folder, nothing changes for you:
`AGENTS.md` at the root is your contract. This page says what the bridge adds.

## What arrives from other machines

- Lines in `memory/inbox.md` ending in `· via <harness>@<machine>` were left
  by an agent elsewhere with the `remember` tool. They are agent-derived: a
  model wrote them from the person's own instructions to it. Reflect files
  them with `← <harness>@<machine>, <date>` as the source and ranks them below
  the person's own words when the two disagree.
- A `## From your agents` section in a daily note holds what those agents did
  that day, one line each, prefixed with the agent's name. Wrap Today and Wrap
  Yesterday may fold it into the closure in one line.
- Pages under `memory/agents/<harness>/<machine>/` are a one-way mirror of
  that agent's own memory files (`MEMORY.md` and its topic files). They belong
  to the agent named in the path: read them, never edit them, never copy
  their contents into the other memory pages yourself. Reflect does that,
  with provenance, and only for facts about the person, not a codebase's
  build quirks.

## What the bridge never does

It appends one line at a time to the inbox, and lines under one heading of a
daily note. It never rewrites, deletes or renames anything else, and it
refuses a line that names a topic under **What Thock should not keep** in
`profile.md`, looks like a credential, or repeats a line already in the inbox.
The filters run on the desk whatever the other agent asked.

## The protocol, for the other side

`routines/agents/skills/thock-memory/SKILL.md` is the skill the other agent
follows; `thock-bridge connect` installs it there. Edit it here, in the vault,
and `thock-bridge update` on the other machine fetches your version.
