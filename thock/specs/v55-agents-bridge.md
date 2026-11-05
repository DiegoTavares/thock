# Thock V55 - The Agents Routine: your other agents share the vault's memory

**Status:** Implemented (2026-11-04)
**Owner:** Diego · **Date:** 2026-11-04
**Companion docs:** `v28-agent-memory.md` (the memory tiers, Reflect, decisions 9 and 12 amended
here), `v27-*` and `crates/thock/src/hosted_agent.rs` (`compose_vault_context`, the block the
desk agent already gets), `v34-vault-sync.md` §7.1 and §8.1 (pairing shape, the write vocabulary
`note_today` reuses), `v7-dynamic-routines.md` and `crates/thock/assets/routines/ROUTINES.md`
(the manifest), `v29` reconcile (shipped core files), research page
<https://claude.ai/artifact/Y3J6TN1mztLbU95XAsysfi> (Hermes, twelve harnesses, prior art)

---

## 1. Summary

A person who keeps a Thock vault usually also runs another agent for work: Claude Code on a build
machine, Codex in a repo, Gemini in a terminal. That agent learns things about the person and
their projects, keeps them in its own private memory, and the vault never sees them. The vault's
own agent, in turn, cannot answer "what did I ship this week" because that happened elsewhere.
Today the only bridge is rule 6 of `AGENTS.md`, which only loads when the other agent's working
directory *is* the vault.

V55 adds an opt-in **Agents** Routine. Activating it makes the running desk app serve the vault's
memory over a small local HTTP endpoint, and ships a tiny client binary, **`thock-bridge`**, that
any harness can speak to over stdio MCP. From any machine on the person's network, an agent can
ask what the vault knows (`context`, `search`, `read`, `recent`), leave one dated line in
`memory/inbox.md` (`remember`), and log what it did to today's note (`note_today`). The harness's
own memory files can live in the vault too, under `memory/agents/<harness>/<host>/`, mirrored
one way by the client. The Reflect ritual stays the only thing that curates `memory/index.md`.

Nothing about this is Claude-specific: the tools are plain MCP, the protocol the agent follows is
an Agent Skill (`SKILL.md`) that lives in the vault and is editable, and `thock-bridge connect`
knows how to register both with the harnesses Thock already detects. When the desk is off, the
client answers from a cache and queues writes in an outbox that drains when the desk returns.

## 2. Goals & success criteria

- **G1: Opt-in, invisible until then.** A vault without the Agents Routine serves nothing, binds
  no port, and changes no file. Activating the Routine is the only switch; removing it stops the
  server and leaves the vault readable as before.
- **G2: Every harness, one protocol.** The same `SKILL.md` and the same six tools work from Claude
  Code, Codex, Gemini CLI, Hermes, Pi, Goose or any MCP client. Nothing in the tool contract names
  a harness.
- **G3: The fleet works offline.** With the desk unreachable, `context` and `read` answer from the
  client's cache (marked as such), `remember` and `note_today` queue, and the queue drains in order,
  once, when the desk is back. No line is lost and no line is written twice.
- **G4: Two write paths, both append.** The server writes exactly two kinds of thing on an agent's
  behalf: one dated line at the end of `memory/inbox.md`, and lines under a named heading of
  today's daily note. Agent areas under `memory/agents/` are the harness's own files. No tool
  rewrites, deletes or renames anything else.
- **G5: Reflect still owns the index.** Every fact from another agent reaches `memory/index.md`
  only through Reflect, with provenance. The person's own words outrank an agent's; the forbidden
  list in `profile.md` applies; "a deleted line stays deleted" still holds.
- **G6: Pair once, revoke any time.** A client pairs with a one-time code shown on the desk. The
  desk keeps only a hash of each client's token and lists them by name with a Disconnect action.
- **G7: Never silent.** An unreachable desk, a refused token, a rejected line and a queued write
  are all reported to the agent in the tool result, in words the agent can relay. The desk shows
  the server's state (serving, port, connected clients) in the Routine's section.

## 3. Non-goals

- **A relay through Thock Plus.** V55 is LAN/tailnet only, free, no server involved. A relay for
  fleets off the home network is a follow-up (research page §8).
- **Two desks serving one vault.** One app instance serves. A second machine with the same vault
  open is undefined and the client takes the first desk it paired with.
- **A persisted search index.** V1 search is an in-memory keyword index rebuilt on open and kept
  current from the worktree's file events. SQLite FTS and embeddings stay V28 phases 2 and 3.
- **TLS.** Plain HTTP with a bearer token on the local network, like the phone's pairing QR trusts
  the room. A self-signed certificate pinned at pairing is listed under open decisions.
- **The desk reading the harness's memory back into the harness.** Mirroring is one way, client to
  vault. The vault copy is for Reflect, search and time-travel, not a sync.
- **Changing the hosted (Pi) agent.** It keeps the pasted index and its `append` extension. Giving
  it the same tools is a follow-up once the server exists.
- **Harness installers beyond Claude Code, Codex and Gemini.** The skill and the MCP endpoint work
  everywhere; `connect` only automates the three CLIs `agent.rs` already detects.

## 4. Core concepts

### 4.1 The desk serves, the client speaks stdio

Every harness in the survey speaks stdio MCP; most also speak Streamable HTTP, but only the stdio
client can keep a cache and an outbox when the desk is off. So the client is a small binary that
the harness launches per session: it talks JSON-RPC on stdin/stdout, forwards each call to the desk
over HTTP, and falls back to its cache when the desk does not answer.

```
harness ──stdio MCP──▶ thock-bridge (client) ──HTTP, bearer──▶ Thock app (server) ──Fs──▶ vault
                          │ cache: index, read notes, last searches
                          │ outbox: remember / note_today, in order
                          └ mirror: ~/.local/share/thock/bridge/agents/<harness>/ ──▶ memory/agents/<harness>/<host>/
```

### 4.2 Three layers a person can stop at

1. **The skill alone.** `routines/agents/skills/thock-memory/SKILL.md` is the protocol in words:
   when to look at memory, how to leave a line, what never to record. A harness run inside the
   vault, or one whose user copied the vault to that machine, follows it with plain file tools.
2. **Skill + client.** `thock-bridge connect <harness>` installs the skill and registers the MCP
   server. Reads and writes go through the tools, which enforce what the skill asks for.
3. **Agent areas.** The harness's own memory directory is pointed at the client's mirror folder
   (Claude Code: `autoMemoryDirectory`) and reaches the vault as `memory/agents/<harness>/<host>/`.

### 4.3 Three kinds of source

V28 decision 9 knows two kinds of text: the person's own words (a source) and imported text
(never a source). An agent's memory file is a third kind, **agent-derived**: written by a model,
from the person's own instructions to it. V55 admits it as a source with provenance
(`← agents/claude-code/robin/MEMORY.md`), ranked below the person's own words when they conflict,
and subject to the forbidden list like everything else. See §10.

## 5. Architecture

| Piece | Where | Role |
|---|---|---|
| `thock_bridge` library | `crates/thock-bridge/thock_bridge.rs` | Protocol and rules shared by server and client: JSON-RPC/MCP framing, tool schemas, the inbox line, the forbidden-list and secret filters, path validation, search scoring, cache and outbox formats. No GPUI, no `fs` crate; pure functions over strings plus a `ToolHost` trait. |
| `thock-bridge` binary | `crates/thock-bridge/main.rs` | The client: `serve` (stdio MCP proxy), `pair`, `connect <harness>`, `status`, `mirror`. Depends on the library, `paths`, `serde_json`, `tiny_http`-free HTTP via `ureq`-style blocking client (see decision 7). |
| `bridge_service.rs` | `crates/thock/src/` | The server inside the app: binds when the Agents Routine is enabled, accepts HTTP on a background thread, hands each request to the foreground, implements `ToolHost` over the project `Fs`, keeps the search index current from worktree events, owns the client list and pairing codes. |
| `connect_agent.rs` | `crates/thock/src/` | The *Connect an agent* modal: shows the pairing command and code until a client pairs or the user closes it. Mirrors `connect_phone.rs`. |
| Agents Routine | `crates/thock/assets/routines/agents/` | `routine.toml`, `Agents.md`, `AGENT.md`, the `thock-memory` skill, the `memory/agents/README.md` seed. |
| Amended core files | `assets/AGENTS.md`, `assets/skills/reflect.md`, `assets/hosted-agent/SYSTEM.md` | Rule 6 and Reflect learn about `memory/agents/` and the `via` marker (§10). |

The library is the contract: both halves and the tests depend on it, the app only implements
`ToolHost`. `context_server` depends on `gpui` outright (verified in its `Cargo.toml`), so the
library defines the handful of JSON-RPC structs itself, field-compatible with
`context_server::types` (`Tool` with `inputSchema`, `CallToolResponse` with `isError`,
`ToolResponseContent`, `Resource`, `ResourceContents`), and both halves depend on nothing heavier
than `serde` (decision 13).

## 6. Files and state

### 6.1 In the vault

```
routines/agents/
  routine.toml
  Agents.md                      human explainer, opened from the panel
  AGENT.md                       what an agent should know before acting here
  skills/thock-memory/SKILL.md   the portable protocol (agentskills.io frontmatter)
memory/agents/
  README.md                      seed: what this folder is, who writes it
  <harness>/<host>/…             mirrored harness memory, harness-owned
.thock/routines/agents/bridge.toml   port, bind address (created on activation)
.thock/state/bridge/clients.toml     paired clients: name, token hash, paired_at, last_seen
.thock/state/bridge/pending.toml     the live pairing code hash and its expiry
```

`bridge.toml`:

```toml
port = 7557
bind = "0.0.0.0"
```

`clients.toml` holds `[[client]] name = "robin" token_sha256 = "…" paired_at = "…"
last_seen = "…"`. Tokens are never stored in the clear on the desk. `.thock/` is never synced
to the phone (V34 §6.4) and is not a note.

### 6.2 On a client machine

```
~/.config/thock/bridge.toml                 desk url, token, vault name, client name
~/.local/share/thock/bridge/cache/          context.json, index.md, notes/<path>, searches/<hash>.json
~/.local/share/thock/bridge/outbox.jsonl    queued remember / note_today calls
~/.local/share/thock/bridge/agents/<harness>/   the mirror folder a harness writes its memory into
~/.agents/skills/thock-memory/SKILL.md      installed by `connect`, symlinked into harness dirs
```

Paths come from the `paths` crate's config and data directories so macOS lands under
`~/Library/…` the same way the app does. The token file is written `0600`.

### 6.3 The inbox line

An agent's `remember` lands as:

```markdown
- 2026-11-04 · Prefers PR titles in the imperative · via claude-code@robin
```

Date is the day the agent called (the client's clock, kept when the line was queued). The
`via <harness>@<host>` marker is appended by the server from the paired client's identity, never
from the agent's text. Reflect files it as `← claude-code@robin, 2026-11-04`.

### 6.4 Today's note section

`note_today` appends under `## From your agents` (configurable per vault in `bridge.toml` as
`today_heading`), one line per call, prefixed by the agent's identity:

```markdown
## From your agents

- claude-code@robin · Shipped the Plus row-level-security PR (#184); CI green.
```

The section is created at the end of the note when missing; the note is created from the daily
template when missing, through the same `Append { create_from_template: true }` write the phone
uses (V34 §8.1), so the rule is one.

## 7. Protocol

All routes are under the port in `bridge.toml`. Every route but `/v1/pair` requires
`Authorization: Bearer <token>`; a missing or unknown token is `401` with a JSON body
`{"error":"unknown client; pair again from the desk"}`.

| Route | Body | Reply |
|---|---|---|
| `POST /v1/pair` | `{"code":"…","name":"robin","version":"1.26.0"}` | `{"token":"…","vault":"Diego's vault","desk":"palloma"}` once; `410` after use or expiry |
| `GET /v1/health` | | `{"vault":"…","desk":"…","version":"…","serving_since":"…"}` |
| `POST /mcp` | one JSON-RPC request | one JSON-RPC response, `Content-Type: application/json` |
| `GET /mcp` | | `405` (no server-initiated stream in V1) |
| `GET /v1/skill` | | the vault's `SKILL.md` text |
| `PUT /v1/agents/{harness}/{path}` | file bytes, `X-Thock-Client` resolved from the token | `204`; writes `memory/agents/{harness}/{client}/{path}` |

`/mcp` implements the MCP Streamable HTTP transport in its stateless, JSON-reply form: the server
answers each POST directly and never opens an event stream. The client sends
`MCP-Protocol-Version: 2025-11-25`. Methods served: `initialize`, `notifications/initialized`,
`ping`, `tools/list`, `tools/call`, `resources/list`, `resources/read`. Anything else is
`-32601`.

**Pairing** (G6): *Connect an agent* generates a 6-word code (the `qrcode` path is not needed; the
command is typed on another machine), stores its SHA-256 and a ten-minute expiry in
`pending.toml`, and shows:

```
thock-bridge pair palloma.local:7557 maple-river-stone-ocean-lamp-fox
```

The client posts the code with its hostname, receives a 32-byte random token (hex), and writes
`bridge.toml`. The desk appends the client to `clients.toml` with the token's hash and closes the
modal. A second pairing with the same name replaces the entry (and the old token stops working).
*Disconnect* removes the entry. The desk's address in the command is the machine's hostname with
`.local` when mDNS resolves, otherwise its first non-loopback IPv4; the modal lets the user copy the
command with any address they prefer.

## 8. Tools

Names are short because harnesses prefix them (`mcp__thock__search`). Every tool returns text
content; `is_error: true` with a sentence the agent can relay on failure.

| Tool | Input | Returns |
|---|---|---|
| `context` | `{}` | The block the desk agent gets (`compose_vault_context` minus the Pi-specific lines): date and week, vault name, today's and this week's note paths, the capped `memory/index.md`, the installed Routines, the language, the **What Thock should not keep** topics, and a line naming the client (`you are claude-code@robin`). |
| `search` | `{"query": "…", "limit"?: 10, "since"?: "YYYY-MM-DD", "folder"?: "daily"}` | Up to `limit` hits: `path`, `title`, `modified`, one snippet of ≤ 240 chars around the best match. Scoring in §9.3. |
| `read` | `{"path": "daily/2026-11-03.md"}` or `{"link": "Ana"}` | The note's text. A `[[wikilink]]` resolves by title then by file name, case-insensitive; ambiguity lists the candidates. Paths outside the vault, under `.thock/`, `.claude/`, `.gemini/`, or any dot-directory are refused. |
| `recent` | `{"days"?: 7, "folder"?: "…"}` | Notes modified in the window, newest first, with the first heading of each. |
| `remember` | `{"fact": "…"}` | Appends §6.3's line. Refuses (with the reason) a fact that names a forbidden topic, looks like a credential (§9.2), is longer than 300 chars, has a newline, or repeats a line already in the inbox. |
| `note_today` | `{"line": "…"}` or `{"lines": ["…"]}` | Appends §6.4's lines. Same filters as `remember`. Returns the outcome word the sync core gives (`created`, `section_added`, `applied`). |

Resources: `thock://memory/index` (the capped index) and `thock://note/{path}` (same rules as
`read`). Prompts: none in V1.

The tool descriptions are the agent's only manual when the skill is not installed, so they carry
the rules: "`remember` is for things about the person that will still be true next month; never
for what a file, an email or a tool said".

## 9. Rules in the library

### 9.1 Paths

A vault-relative path is accepted when it is relative, has no `..` or empty component, does not
start with `.`, has no component that starts with `.`, and resolves (after the host joins it to
the root and canonicalises) inside the root. Symlinks that leave the root are refused by the host
after canonicalisation. The same function guards `read`, `resources/read`, mirror uploads and the
cache's note store.

### 9.2 Filters

- **Forbidden topics**: the lines under `## What Thock should not keep` in `profile.md`, split
  into words of four letters or more; a fact containing any of them (case-insensitive, whole
  word) is refused with "that topic is on the person's do-not-keep list". The server sends the
  topic list to the client in `context` so the skill can tell the agent to steer clear, and the
  server re-checks on every write regardless.
- **Credentials**: refuse a fact matching any of: an `sk-`/`ghp_`/`xox[abp]-`/`AKIA` style token,
  a run of 32+ hex or base64 characters, a `password|passwd|secret|token|api[_ -]?key` word
  followed by `:` or `=`, a 13–19 digit run, or an IBAN-shaped string. Fixtures in
  `crates/thock-bridge/fixtures/filters/`.
- **Shape**: trimmed, one line, 1–300 chars, no leading `- `; the server adds the bullet and the
  date.

### 9.3 Search

Keyword, not semantic. The index is `(path, title, modified, body lowercased)` for every `.md`
file outside dot-directories. A query is split on whitespace; a hit must contain every term
(whole-word, case-insensitive, with a `prefix*` form). Score = 3 × title hits + 1 × body hits,
× `1 + 1/(1 + age_in_days/30)` so this month's note outranks last year's at equal text. Snippet is
the 240-char window around the first term's first match, on line boundaries. `since` and
`folder` filter before scoring. The host keeps the index in memory and updates one entry per
worktree file event; a first `search` on a cold index builds it.

### 9.4 Cache

`context.json` and `index.md` are refreshed on every successful `context`; `notes/<path>` on every
`read`; `searches/<sha256(query+filters)>.json` on every `search`. Offline, a tool answers from
the cache and appends `(from cache, as of 2026-11-04 08:12; the desk is unreachable)`. A cache
miss offline is an error that says so and names the desk address.

### 9.5 Outbox

One JSON line per queued call: `{"id": uuid, "at": "2026-11-04T08:12:03Z", "tool": "remember",
"args": {…}}`. On every start and before every forwarded call the client drains the outbox in
order, stopping at the first failure so order holds. The server treats `id` as idempotent for
24 hours (`.thock/state/bridge/seen.jsonl`, pruned on open), so a drain that lost its reply does
not double-write. A queued call answers the agent `queued: the desk is unreachable; this will land
when it is back`, with `is_error: false`.

### 9.6 Mirror

`thock-bridge mirror` walks `~/.local/share/thock/bridge/agents/<harness>/`, uploads files whose
mtime is newer than the last recorded upload (`mirror-state.json`), and the server writes each
whole file under `memory/agents/<harness>/<client>/<path>`. Only `.md`, `.txt` and `.json` under
1 MiB, paths through §9.1. The client runs a mirror pass at `serve` start, after every
`remember`/`note_today`, and at `serve` exit. Deletions do not propagate in V1.

Verified at implementation (Claude Code docs, `/memory`, 2026-11-04): `autoMemoryDirectory` names
**one flat directory** holding `MEMORY.md` and one topic file per memory; the default is
`~/.claude/projects/<project>/memory/`, one per repository, and a value set at user scope replaces
that for every project, so all of them share the one directory. The mirror folder
`~/.local/share/thock/bridge/agents/claude-code/` is therefore that one directory, mirrored as-is
(`MEMORY.md`, `user_role.md`, …) under `memory/agents/claude-code/<host>/`. The value must be an
absolute path or start with `~/`; `connect claude` writes the absolute path.

## 10. Memory rules, amended

Changes to shipped core files, upgraded in place by the V29 reconcile:

- **`AGENTS.md` rule 6** gains: "An agent working from another machine reaches this folder through
  the Agents Routine's tools; the rules are the same. Pages under `memory/agents/` belong to the
  agent named in their path: read them, never edit them, and never copy their contents into the
  other memory pages yourself; Reflect does that with provenance."
- **`skills/thock/reflect.md`**: `memory/agents/**` joins the sources, as *agent-derived*: file a
  fact from there with `← agents/<harness>/<host>/<file>`, only when it is about the person or a
  thread of theirs (not about a codebase's build quirks), and when it contradicts a line sourced
  from the person's own words, keep the person's line and note the disagreement on the detail
  page. Inbox lines ending in `via <harness>@<host>` are filed with `← <harness>@<host>, <date>`.
  The forbidden list and the quarter rule apply unchanged.
- **Wrap Today / Wrap Yesterday**: when the day's note has a `## From your agents` section, the
  closure may summarise it in one line. No change to their write rules.
- **V28 decisions 9 and 12** are amended in that spec's decision table with a pointer here.

## 11. Desk side (`crates/thock`)

- **`BridgeService`**, one per workspace with a valid vault, created by `thock.rs` init like
  `VaultSyncService`. It observes the Routine list: when `agents` becomes enabled it reads or
  creates `bridge.toml`, binds `bind:port` on a background thread (a `std::net` accept loop with
  one worker per connection; see decision 24 for why not `tiny_http`), and starts the foreground
  task that drains the request channel. When the Routine is removed or the vault closes it stops the thread and drops the
  listener. A bind failure (port taken) is a visible holding state in the section, with the port.
- **Request handling.** A worker thread reads the request head, routes it and checks the token
  against `clients.toml` (constant-time compare of hashes) before reading a byte of body (capped
  at 1 KiB for `/v1/pair`, 2 MiB once paired, 10 s socket timeouts), then sends
  `(Incoming, oneshot::Sender)` over a `futures` channel. The foreground task implements `ToolHost` with the project `Fs`: reads through
  `Fs`, writes through `thock_sync_core::apply` with a `Write` built by the library, so every
  write fires file events and takes the same history checkpoint (`history::checkpoint_before_ai_write`)
  the hosted agent's `append` takes. The thread writes the reply and closes.
- **`ToolHost` in the app** reuses `memory::read_index_capped`, `compose_vault_context`'s pieces
  (factored so the Pi-specific lines are separable), `notes::ensure_note` for the daily note, and
  the worktree snapshot for the search index and `recent`.
- **UI.** In the Routines panel the Agents section gets a status row above its links:
  `Serving on palloma.local:7557 · 2 agents` / `Not serving: port 7557 is in use` / `Off`.
  Enter on the row opens *Connect an agent*; a `Connected agents` collapsed row lists clients with
  `last seen` and a Disconnect action. Actions: `thock::ConnectAgent` (opens the modal),
  `thock::DisconnectAgent` (picker over clients), `thock::ToggleBridge` (stop/start without
  removing the Routine). All three in the command palette; the modal is `menu`-navigable and
  `escape` closes it.
- **Keymap.** No new chords; the actions are palette-reachable. The modal's `KeyContext` is
  `ThockConnectAgentModal`.

## 12. Client side (`crates/thock-bridge`)

Subcommands:

- `thock-bridge pair <host:port> <code>` — posts the code, writes `bridge.toml`, prints the vault
  name.
- `thock-bridge serve` — the stdio MCP proxy. Drains the outbox, runs a mirror pass, then loops:
  read a JSON-RPC message, forward `/mcp` with the token, on connection failure or `5xx` answer
  from the cache (§9.4) or queue (§9.5). `initialize`, `tools/list` and `resources/list` are
  answered locally from the library's schemas so a harness can start offline.
- `thock-bridge connect claude|codex|gemini [--dry-run]` — fetches `/v1/skill` into
  `~/.agents/skills/thock-memory/SKILL.md`, symlinks it into the harness's skills directory
  (`~/.claude/skills/`, `~/.codex/skills/` or `~/.agents/skills` as the harness reads it,
  `~/.gemini/skills/`), registers the server with the harness's own command (`claude mcp add
  --scope user thock -- thock-bridge serve`, `codex mcp add thock -- thock-bridge serve`,
  `gemini mcp add -s user thock thock-bridge serve`; exact flags verified against each CLI's
  docs at implementation, see below, falling back to printing the config block to add), and for
  Claude Code offers to set `autoMemoryDirectory` to the mirror folder in `~/.claude/settings.json`.
  Every file it would change is printed first and confirmed with `y`, except under `--yes`.

  Verified 2026-11-04 against the official docs; the commands `connect` runs, each with
  `serve --harness <id>` so the desk can name the harness behind the machine:

  | Harness | Command | Stored in | Manual block |
  |---|---|---|---|
  | Claude Code | `claude mcp add --scope user --transport stdio thock -- thock-bridge serve --harness claude-code` | `~/.claude.json` | `mcpServers.thock = {type: stdio, command, args}` |
  | Codex | `codex mcp add thock -- thock-bridge serve --harness codex` | `~/.codex/config.toml` | `[mcp_servers.thock] command = …, args = […]` |
  | Gemini CLI | `gemini mcp add -s user thock thock-bridge serve --harness gemini` | `~/.gemini/settings.json` | `mcpServers.thock = {command, args}` |

  Gemini's `--scope` defaults to `project`, hence `-s user`; its transport defaults to stdio.
  Codex's `--env` goes before `--`; none is needed.
- `thock-bridge status` — desk reachability, cache age, outbox length, mirror state.
- `thock-bridge mirror` — one mirror pass.
- `thock-bridge update` — refresh the installed skill from `/v1/skill`.

Logging goes to stderr only; stdout is the MCP channel. The binary builds without GPUI, `fs` or
`project`. Its HTTP client is the library's own: `ureq` is not in `Cargo.lock`, and `reqwest`'s
blocking client drags tokio into a binary that otherwise needs no runtime, so the client speaks
plain HTTP/1.0 over `std::net::TcpStream` (one request per connection, `Content-Length` bodies,
chunked replies unwrapped), which is all the V1 contract asks for (decision 14). TLS, when it
comes, is the moment to pick a crate.

Distribution: `script/bundle-linux` and `script/bundle-mac` add `--package thock_bridge` next to
`cli`, and the bundle places the binary beside the `thock` CLI so *Install CLI* puts both on
`PATH`. The release workflow also uploads `thock-bridge-<target>` as its own asset, for a fleet
machine without the app: `curl` it, `chmod +x`, `pair`.

## 13. The Agents Routine

`routines/agents/routine.toml`:

```toml
schema  = 2
id      = "agents"
name    = "Agents"
version = 1
summary = "Let the agents you use elsewhere read what this vault knows and leave notes in it."
icon    = "terminal"
doc     = "routines/agents/Agents.md"
agent_doc = "routines/agents/AGENT.md"

[[link]]
id   = "agent-memory"
name = "What your agents keep"
open = "memory/agents/README.md"
kind = "editor"

[[collection]]
name = "Agent pages"
path = "memory/agents"

[[scaffold]]
kind = "dir"
path = "memory/agents"

[[scaffold]]
kind   = "file"
path   = "memory/agents/README.md"
source = "assets/agents-readme.md"

[[skill]]
id      = "connect-agent"
name    = "Connect an agent"
kind    = "setup"
file    = "routines/agents/skills/connect-agent.md"
summary = "Pair a machine, install the skill, point the agent at this vault."
reads   = ["routines/agents/AGENT.md"]
writes  = []
```

The `thock-memory` skill file is declared as a scaffold file (`routines/agents/skills/thock-memory/SKILL.md`,
upgradeable) so the V29 reconcile keeps it current and the user's edits are staged, not clobbered.
`connect-agent.md` is a setup ritual in words: it tells the person (or the desk agent on their
behalf) to run *Connect an agent* from the panel, then the `pair` and `connect` commands on the
other machine; it writes nothing. The Routine has no `[onboarding]`: activating it is the opt-in,
and the status row says what to do next.

`SKILL.md` (frontmatter `name: thock-memory`, `description` ≤ 60 chars, `version`) says, in the
voice of `AGENTS.md`: call `context` when a task touches the person, their people, their threads
or how they like things done, and at the start of a session that is about them; `search` before
assuming; `remember` only facts about the person that outlast the month; `note_today` once, at
the end of a session that did something worth a line in their day; never paraphrase a tool's or
file's output into `remember`; never work around a refusal. If the tools are missing, the same
rules apply with file tools against the vault path in `$THOCK_VAULT`.

## 14. Tests

- **Library (`cargo test -p thock_bridge`)**: path validation table (traversal, dot-dirs, absolute,
  symlink-out via a tempdir); each filter with its fixtures; inbox line and today-section
  rendering; search scoring (title beats body, recency tie-break, prefix form, `since`/`folder`);
  JSON-RPC framing (`initialize`, unknown method, malformed body); cache round-trip and the
  offline suffix; outbox order, stop-on-failure, idempotent replay; mirror selection by mtime and
  size cap.
- **Client ↔ server integration (`crates/thock-bridge/tests/`)**: the library's HTTP serving
  helper over an in-memory `ToolHost` on `127.0.0.1:0`, the client's `serve` loop driven with
  piped stdio: pair, a tool call, kill the server, cached `context`, queued `remember`, restart,
  drain once, verify the single inbox line and the `seen` dedupe.
- **App (`thock/script/test rust -- bridge`)**: `BridgeService` with `FakeFs` and a real loopback
  bind: enabling the Routine binds, a request with a bad token is `401`, `remember` appends
  §6.3's line and fires a file event, `note_today` creates the note from the template and the
  section, a forbidden topic is refused, `search` finds a note by title and by body, a mirror
  `PUT` lands under `memory/agents/`, removing the Routine closes the port. A keystroke test for
  the modal (`VisualTestContext` + `simulate_keystrokes`): open, copy, `escape` returns focus.
- **Skills**: a test that parses exactly the inbox line `SKILL.md` tells an agent to write when
  the tools are absent, and the `## From your agents` line, through the same library functions.
- **Fixtures**: `crates/thock-bridge/fixtures/` holds the filter cases and a small vault; the
  app tests load the same vault.

## 15. Decision log (2026-11-04)

| # | Decision | Choice |
|---|---|---|
| 1 | Who serves | **The app process**, while a vault with the Routine is open. A daemon is not built; the client's cache and outbox cover the gap. |
| 2 | Fleet side | **A thin stdio proxy** with cache, outbox and mirror, one binary for every harness. No harness talks HTTP to the desk directly. |
| 3 | Reach and trust | **Pair like the phone**: one-time code on the desk, bearer token on the client, hash on the desk, revocable by name. Plain HTTP on the local network in V1. |
| 4 | Opt-in surface | **The Agents Routine.** No new key in `config.toml`; port and bind live in `.thock/routines/agents/bridge.toml`. |
| 5 | Write scope | **Inbox + today's note + agent areas.** Agent areas are harness-owned whole files; the two tool writes are appends through the sync core's `Write`. |
| 6 | Topology | **One desk per vault.** |
| 7 | Tier | **Free.** |
| 8 | Harness installers | **Claude Code, Codex, Gemini** in V1; everything else by hand with the printed config block. |
| 9 | Search | **In-memory keyword index** in the app, kept current from worktree events. No SQLite in V1. |
| 10 | Agent-derived text | **A source, with provenance and lower rank** than the person's own words (§4.3, §10). |
| 11 | Mirror direction | **Client to vault only.** |
| 12 | Daily section name | **`## From your agents`**, overridable in `bridge.toml`. |

Added at implementation (2026-11-04):

| # | Decision | Choice |
|---|---|---|
| 13 | JSON-RPC types | **The library's own**, field-compatible with `context_server::types`; that crate pulls GPUI (§5). |
| 14 | Client HTTP | **`std::net` HTTP/1.0** in the library; `ureq` is not in the lock file and `reqwest` blocking brings tokio (§12). |
| 15 | Who is `via <harness>@<host>` | **`host` is the paired machine's name** from `pair`; **`harness`** comes from `serve --harness <id>` (set by `connect`), else from MCP `initialize`'s `clientInfo.name`, else `agent`. It travels as the `X-Thock-Harness` header and the desk sanitises it (`[a-z0-9-]`, ≤ 40). |
| 16 | Idempotency carrier | **`params._meta.call_id`** and **`_meta.date`** on `tools/call`, set by the client on every `remember`/`note_today` (online too), kept from the queued entry on a drain. The desk answers a seen id with the recorded outcome and writes nothing. |
| 17 | `ToolHost` shape | **Async methods, generic dispatch** (`dispatch<H: ToolHost>`), so the desk implements it over the async `Fs`; the client answers offline directly from its cache and outbox without a host. |
| 18 | Action name | **`thock::ConnectOtherAgent`**: `thock::ConnectAgent` already exists (the terminal agent's connect flow). `DisconnectAgent` and `ToggleBridge` as specced. |
| 19 | Upgradeable scaffold files | `routine.toml` scaffold files gain **`upgrade = true`** (documented in `ROUTINES.md`), so `SKILL.md` and `AGENT.md` upgrade with the app and an edited copy is staged, without declaring the protocol as a runnable `[[skill]]`. |
| 20 | `context` composition | **`compose_vault_context` is reused whole** and the bridge appends a `## You, from here` block (identity, how the two writes land, the do-not-keep topics). Its lines about `append`/templates read fine through `read`/`note_today`; factoring Pi-specific lines out was more churn than the block is worth. |
| 21 | Disconnect on a row | **`enter` on a client row disconnects it** (the row says `Disconnect`); the toast says it can pair again. `thock::DisconnectAgent` opens a picker over the clients. |
| 22 | Release asset | `thock-bridge-<os>-<arch>` is published as its own artifact and the update manifest lists it under **asset `thock-bridge`**, so the releases index can serve it by `?asset=thock-bridge`. |
| 23 | Stop ordering (review H1) | **Replies before the listener.** `receiver_task` is dropped first so a worker blocked on a reply answers 503 at once; the accept loop is woken with a local connection and joined on a background task that the next bind awaits. Never a join on the foreground. |
| 24 | Serving (review H2) | **The desk's own accept loop over `std::net`**, one worker thread per connection (16 at once, 503 beyond), 10 s socket timeouts, route and token from the head before any body, `/v1/pair` bodies capped at 1 KiB, authenticated ones at 2 MiB. `tiny_http` is out: it reads bodies lazily with no timeout. The head/body/response helpers live in the library (`read_request_head`, `read_body`, `write_response`), which the client's integration test serves with too. |
| 25 | Client dates (review M1) | **`_meta.date` is honoured within [today − 7, today]** (`CALL_DATE_WINDOW_DAYS`, the outbox horizon). Outside it `remember` labels the line with today and `note_today` refuses with the window in the sentence. |
| 26 | Links (review M2, M4) | **Symlinks are never followed**: the index skips any linked entry, and every write or read resolves its deepest existing ancestor and requires it under the canonical root (`resolves_inside`), mirror `PUT` included. |
| 27 | Queue hygiene (review M3) | **Validate before queueing, drop what the desk refuses for good.** A queued `remember`/`note_today` passes the library's argument parsers and the credential/shape filter first; on a drain, HTTP 4xx, JSON-RPC `-32602`/`-32601` and an `is_error` tool result drop the entry and are logged to stderr and `refused.log` (shown by `status`); only a desk that does not answer stops the drain. |
| 28 | Mirrored files (review M5) | **(a) The desk scans every mirrored file** line by line with the credential filter and the person's do-not-keep list and answers 422 with the line and reason; the client skips it, reports it in `mirror`/`status`, keeps going, tries again next pass. **(b) `connect claude` says before the y/N** that every project's `~/.claude/projects/*/memory` stops loading, counts them, and `--import` copies each into the mirror under `imported/<project-slug>/` for Reflect and search. |
| 29 | Outbox sharing (review L2, L3) | Each entry records the **harness** that queued it and drains under that name; an advisory `flock` on `outbox.lock` wraps every push and drain. |
| 30 | Modal close (review L6) | The modal closes when the **pending code is spent** or the client list changes, so a known machine re-pairing closes it too. |
| 31 | Pairing address (§7) | `.local` leads **only when it resolves here**; otherwise the IPv4 leads and the name follows as a hint. |

## 16. Open decisions, for the first iterations

- **TLS.** A self-signed certificate generated at activation, its fingerprint carried in the
  pairing command and pinned by the client, would make the token safe on an untrusted network at
  the cost of a `rustls` server in the app. Decide after the first week on a trusted LAN.
- **Claude Code's `autoMemoryDirectory` granularity.** Resolved, see §9.6: one flat directory,
  shared by every project when set at user scope; the mirror folder is that directory.
- **Desk address in the pairing command.** `.local` names depend on mDNS; a tailnet name may be
  what the person wants. The modal shows the alternatives it can see.
- **Index growth.** A vault with tens of thousands of notes makes the in-memory index heavy;
  measure on a real vault before deciding whether phase 2's SQLite comes forward.

## 17. Delivery

Definition of done, in order:

1. `thock_bridge` library with the rules in §9 and their tests.
2. `thock-bridge` binary: `pair`, `serve` (online path), `status`.
3. `BridgeService` in the app: bind on Routine activation, auth, the six tools and two resources,
   history checkpoint on write, status row, *Connect an agent* modal, Disconnect.
4. The Agents Routine assets, the skill, the README seed, the amended `AGENTS.md`, `reflect.md`,
   `SYSTEM.md`, and V28's decision table pointing here.
5. Offline path: cache, outbox, idempotent drain, the integration test in §14.
6. Mirror: `mirror` subcommand, `PUT /v1/agents`, the Reflect rule for `memory/agents/`.
7. `connect claude|codex|gemini`, `update`, bundling in both bundle scripts and the release
   workflow asset.
8. VISION §12 entry; `TESTING.md` learns the new package in `thock/script/test`'s mapping.

## 17a. Review follow-ups (2026-11-04)

An independent review before the PR found the items decisions 23–31 record. Also fixed without a
decision: the token file is created `0600` (not chmod'd after), `percent_decode` works on bytes, a
4xx from the desk reaches the agent as `is_error` instead of being queued, mirror passes skip and
report a refused file instead of stopping, `AGENTS_ROUTINE_ID` is defined once in `routines.rs`,
Wrap Yesterday carries the `## From your agents` note like Wrap Today, the skill description is
60 characters and the test holds it there, `remember`'s desk test asserts the worktree file event,
and the Routines panel's Agents rows have a keystroke test (enter opens the modal, enter on a
machine disconnects it) with `j`/`k` bound for the modal's address list under vim.

## 18. Risks

- **A token in the clear on the LAN.** Mitigated by opt-in, by the pairing step being a human
  act, by hashes on the desk and revocation by name; closed by the TLS follow-up.
- **The app must be open.** Stated in the Routine's doc and the status row; the cache and
  outbox make the fleet degrade gracefully rather than fail.
- **An agent that over-remembers.** Capped by the filters, by the inbox being the only path, and
  by Reflect's judgement; the weekly forget checklist still lists what nobody mentions again.
- **Mirror files that are not about the person.** Build quirks and repo notes will land under
  `memory/agents/`. That is fine: Reflect is told to file only what is about the person; the rest
  is searchable context and time-travelled history, which is what the person asked for.
- **Two tools, two clocks.** A queued line keeps the client's date. A desk whose day has rolled
  over still appends to the date the agent meant.
