# Thock V35: Ask on the phone

**Status:** In progress (2026-10-03): design accepted, first release being implemented
**Owner:** Diego · **Date:** 2026-10-03
**Companion docs:** `v33-iphone-companion.md` (§11 Ask, §14 the write contract this amends by one row),
`v34-vault-sync.md` (§13, the ephemeral runner this replaces), `v25-thock-plus-hosted-agent.md` (the
gateway, the allowance loop and the trust model this extends to a second device),
`v27-agent-session-prompt.md` (the context block the phone ports), `v28-agent-memory.md` (the memory the
phone reads and the inbox it appends to)

---

## 1. Summary

Ask is the phone's fifth job: a question about the vault, answered from the vault, away from the desk.

- *When did I deploy the Maestro fix?*
- *Who was responsible for the database migration in March?*
- *Based on my financial plan, does it make sense to buy this car now?*
- *I'm feeling overwhelmed at work. What did I do last time that helped?*

Answers are short by default and longer only when the question needs it. The agent is the same Thock
Agent the desk has: same voice, same memory, same model tier. It works while the desk is closed.

The design in one sentence: **the agent loop runs on the phone.** The phone already holds the whole
decrypted vault (V34), so the phone calls the model gateway directly, as the desk does, and serves the
agent's tools from its own store. No Thock server ever sees a note. V34 §13's ephemeral runner, and the
VISION exception it would have needed, are withdrawn.

## 2. Locked decisions (2026-10-03)

| # | Decision | Choice |
|---|---|---|
| 1 | Where the loop runs | **On the phone.** A small Swift tool loop in `ThockKit`, talking to the gateway with a key the backend grants. Replaces V34 §13 (server-side runner) and V34 §15 #6. |
| 2 | Why not the runner | A fixed working set cannot answer whole-vault questions, and feeding the runner the vault means plaintext notes on Thock's server, which V34's encryption promises never happens. |
| 3 | Why not a relay to the desk | Ask must work while the desk is closed. One path, not two that behave differently. |
| 4 | Same agent | Sameness comes from shared words and shared files, not a shared binary: the desk's `SYSTEM.md` sections for voice, memory and language are reused verbatim; the V27 context block is ported; the vault's `AGENTS.md`, `profile.md` and `memory/index.md` are in context; the model is the plan's Default tier. |
| 5 | Memory writes | **The phone may append to `memory/inbox.md`**, one dated line, exactly as the desk agent does in a session (V28 §5.3). Reflect files it at the desk. V33 §14 gains this one row. |
| 6 | Gateway key | **One key per device.** The phone gets its own budget-capped key, minted on first use and revoked with the phone. The allowance is one pool: usage is summed across both keys. |
| 7 | First-release write scope | **Read-only, plus two appends:** the memory inbox (by the agent) and *Keep this* (by the person, under `# Asked on the go` in today's note). Appends to today, the backlog and the inbox by request (V33 §11) are a second pass; the tool is shaped so that pass only widens a guard. |
| 8 | Search | **SQLite full-text search on the phone**, ranked, over every synced note. This is V28's Phase 2 ranked search arriving on the phone first; no embeddings. |
| 9 | Thread | One thread per day, kept on the phone only, gone at midnight. Never written to the vault, never memory (V28 non-goal: no transcript archive). |
| 10 | Rituals | Not on the phone (V33 §3). Asked to run one, the agent says it runs at the desk. |

## 3. Goals & success criteria

**G1. Fact lookup.** "When did I deploy the Maestro fix?" is answered in one or two sentences with the
date and the note it came from, in a vault where one daily note mentions it.

**G2. Synthesis.** "What helped last time I was overwhelmed?" produces an answer drawn from several
notes, each named under the answer.

**G3. Desk closed.** Both work with the desk off, against the last vault the desk uploaded plus the
phone's own waiting writes.

**G4. Same agent.** A fact in `memory/index.md` is known without a tool call. A fact told to the phone
("my manager is Ana now") is in `memory/inbox.md` at the desk after the next sync, and in the index
after the next Reflect.

**G5. Nothing new leaves.** The only parties that see note text are the phone and the model provider,
the same line the desk draws (V25 §4). The backend sees one more route and no content.

**G6. One allowance.** A turn on the phone moves the same usage bar the desk shows. At zero the phone
says so in one sentence and offers nothing that spends.

**G7. Honest about failure.** No connection, an interrupted turn, an exhausted allowance and a lapsed
Plus each have one plain sentence. A turn that fails leaves the question in the thread with *Try again*.

## 4. Non-goals

- **Running rituals, rewriting notes, filing.** V33's pen rules stand.
- **Appends to today, backlog and inbox by request.** Second pass (decision 7).
- **A server-side runner or a desk relay.** Decisions 2 and 3.
- **Embeddings or any index outside the phone's store.**
- **Ask without Plus.** The phone has no vault without Plus (V34). *Ask later* stays a later tier.
- **A model picker.** Tiers only, mapped by the backend (V25 decision 5).
- **Streamed answers, attachments, voice.** Text in, text out, one whole answer at a time.

## 5. Architecture

```
Phone (ThockKit)                         Backend (services/plus)        Gateway
┌───────────────────────────────┐        ┌───────────────────────┐      ┌─────────────┐
│ Ask screen                    │        │ GET /v1/vault/agent   │      │ OpenRouter  │
│ AskAgent: the tool loop       │──grant▶│  phone key, tier map, │      │  per-device │
│  prompt = shared SYSTEM       │        │  balance              │      │  keys, caps │
│   sections + phone sections   │        │ allowance loop sums   │─────▶│             │
│   + context block             │        │  desk and phone usage │ usage│             │
│  tools: search, read, list,   │        └───────────────────────┘      │             │
│   append (memory inbox only)  │──────── chat completions, tools ─────▶│  model      │
│ VaultStore: notes + FTS index │
│ write queue (V34)             │
└───────────────────────────────┘
```

### 5.1 The grant (backend)

`GET /v1/vault/agent`, phone credential only, refused with `plus_lapsed` on a lapsed vault like every
phone write route.

```json
{
  "status": "active",
  "allowance_units": 1000,
  "used_units": 212,
  "remaining_units": 788,
  "warn_at_percent": 80,
  "cycle_ends_at": "2026-10-30T12:00:00Z",
  "gateway": {
    "provider": "openrouter",
    "base_url": "https://openrouter.ai/api/v1",
    "api_key": "sk-or-…",
    "models": {"default": "google/gemini-2.5-flash", "fast": "google/gemini-2.5-flash-lite"}
  }
}
```

- `status` is `active` or `exhausted`. When exhausted, `gateway` is still present; the key is disabled
  at the gateway, which is the hard stop.
- `base_url` is in the grant so a gateway migration (V25: LiteLLM) stays backend-only.
- **The phone key** is minted on the first call with the same dollar cap as the desk key and stored on
  the user row beside it (`phone_gateway_*` columns, migration `0003`). It is revoked when the phone
  device row goes: *Disconnect phone*, a second pairing replacing the phone, vault delete or reset, a
  lapse, and user revocation. The next grant after a renewal or re-pair mints a fresh one.
- **One pool.** The allowance loop (`entitlementFor`) reads usage from both keys; used units are the sum
  above each key's cycle baseline. A cycle rollover resets both baselines. Exhaustion disables both
  keys and a top-up re-enables both. Each key's own cap stays at *its baseline plus the cycle's
  allowance*, so the worst case with the backend unreachable and both keys extracted is twice the
  allowance; accepted under V25 decision 12 (budgets, not DRM) and noted in §9.
- **A key that goes takes nothing back.** When the phone key is revoked mid-cycle (a re-pair, a
  disconnect), what it spent is read first and moved onto the desk key's side of the sum, so pairing
  again never refills the allowance. If the gateway cannot be read at that moment the spend is logged
  and lost for the cycle.
- The grant runs the allowance loop, so the phone asking before each turn is what keeps the balance and
  the hard stop current when the desk is closed.
- `GET /v1/entitlement` (desk) is unchanged in shape; its numbers now include phone usage.

### 5.2 The loop (phone)

`AskAgent` in `ThockKit/Sources/ThockKit/Ask/`:

1. Fetch the grant. Exhausted, lapsed, offline: one sentence, no model call.
2. Build the messages: system prompt (§5.4), today's earlier turns (questions and answers only, the
   last twelve), the new question.
3. Call `POST {base_url}/chat/completions` with the Default-tier model and the tool definitions. Tool
   calls are run locally and their results appended; repeat until the model answers without calling a
   tool. Replies are not streamed in the first release: answers are short, the activity line carries
   the wait, and taking each reply whole lets the phone send the model's own message back untouched on
   the next call, which keeps provider fields it does not know about (reasoning signatures) intact.
4. At most **12 model calls per turn**. On the last one `tool_choice` is `none` so the model must
   answer with what it has.
5. Record the turn: question, answer, the notes it read, in the phone's store.

A turn must finish while the app is in the foreground or within the short grace iOS gives a task after
backgrounding. An interrupted turn keeps its question and shows *Try again*.

### 5.3 Tools

Four, all served from `VaultStore`. Paths are vault-relative, as the person sees them.

| Tool | Arguments | Returns |
| --- | --- | --- |
| `search` | `query` (words), `folder` (optional prefix) | Up to 12 notes ranked by relevance: path and a short excerpt around the match. Words are matched as prefixes, any of them, diacritics folded, so *deploy* finds *deployed* and *migracao* finds *migração*. |
| `read` | `path`, `from_line` (optional) | The note, up to 400 lines from `from_line`, with a closing line saying how many remain when cut. A missing note is a plain sentence, not an error. |
| `list` | `folder` (optional) | Paths under the folder, newest name last, capped at 200 with a count of the rest. Daily notes are named by date, so this is how a date range is found. |
| `append` | `path`, `text` | Appends lines to the end of a note through the phone's write queue. **Refused unless `path` is `memory/inbox.md`**; the refusal returns as a sentence the model can act on. The second pass widens this guard to today, tomorrow, the backlog and the inbox (V33 §11). |

There is no shell, no network tool, no edit and no write. A prompt injection sitting in a note can at
worst waste a turn or leave a line in the memory inbox, which Reflect's own rules (V28 decision 9)
then judge at the desk.

Search index: an FTS5 table over the store's `files` rows (path and content), kept current by triggers
so every store write path is covered, built once for stores that predate it. `.thock/**` and
non-Markdown files are indexed too but `search` only returns `.md` and `.txt`.

### 5.4 The prompt

Composed per turn from four sources, in this order:

1. **Phone sections** (`Ask/Prompts/PHONE.md`, prose, no placeholders): who the agent is here, how to
   answer on a phone, the four tools, what stays at the desk.
2. **Shared sections**, verbatim from the desk's `SYSTEM.md`: *How you speak*, *What you remember*,
   *Language*. The phone bundles a copy of the desk file; a test fails when the copy and
   `crates/thock/assets/hosted-agent/SYSTEM.md` differ, or when one of the three headings is gone.
3. **The context block**, a Swift port of `compose_vault_context` (V27, V28 decision 11): today's date
   and ISO week, where today's and this week's notes and the backlog live, the vault's language, and
   `memory/index.md` under the `[memory] index_lines` cap as *What you already know*. The Routines list
   is left out: the phone runs no rituals.
4. **The vault's own words:** `AGENTS.md` and `profile.md` when present, each capped at 200 lines.

Answer style, stated in the phone sections: lead with the answer; one to three sentences unless the
question asks for judgment, a plan or a summary; name the note a fact came from; say plainly when the
vault does not hold the answer instead of guessing; for a judgment question (the car), say what in the
vault the view rests on and what is missing.

### 5.5 The screen

The existing Ask sheet (quick action, icon shortcut, `-thock-open ask`), behind the same unlock as
every read screen (V33 §4.6):

- The day's thread: the person's bubble, the agent's amber block, and under each answer the notes it
  read as vault-relative paths (V26 grammar).
- One quiet activity line while a turn runs: *Looking through your notes for "Maestro"*, *Reading
  daily/2026-09-12.md*, *Noted one thing for later*.
- The composer at the bottom, send on return, disabled while a turn runs; a stop control cancels it.
- **Keep this** under an answer appends the question and the answer to today's note under
  `# Asked on the go` (V33 §11), through the write queue, once per answer.
- The balance is not shown until it matters: a line at the warn threshold, a sentence at zero.
- The practice notebook has no gateway; the sheet says Ask starts once the phone is connected to a
  desk.

Vocabulary stays V33's: note, today, the desk. Never model, token, key, sync.

## 6. Amendments to other specs (made in this change)

- **V33 §11:** the agent runs on the phone; first release is read-only plus the two appends.
- **V33 §14:** one new row: `memory/inbox.md`, append one line, by the agent during Ask. The sentence
  "Not `memory/`" becomes "Not the rest of `memory/`".
- **V34 §13, §15 #6, §16 #7, §17 #4:** the runner is withdrawn; the data-side obligation that remains is
  that the phone holds every allow-listed file, which it already does.
- **V25 §4:** the non-goal "no server-side agent execution" stands with no exception.
- **VISION §12, Milestone 6:** an entry for this spec.

## 7. Tests

- **Backend:** the grant mints one phone key and returns the same one on a second call; desk
  credentials are refused; a lapsed vault is refused; usage on either key moves `used_units`; exhaustion
  disables both keys and a top-up re-enables both; rollover resets both baselines; device revoke,
  re-pair, vault delete, lapse and user revoke each revoke the phone key; pairing again keeps the old key's spend counted.
- **Search:** ranked hits, prefix and diacritic folding, folder filter, an index that follows writes,
  snapshots, tombstones and wipe, and the one-time build over an existing store.
- **Tools:** `read` cut and continuation, missing note, `list` cap, `append` refusal outside the memory
  inbox and the queued write inside it.
- **Prompt:** the bundled `SYSTEM.md` equals the desk's; the three shared headings exist; the context
  block over synthetic vaults (no memory folder, index at and over the cap, language set and unset).
- **Loop:** against a scripted model: a direct answer; search then read then answer, with sources
  recorded; several tool calls in one reply; the model's own message sent back untouched; the 12-call
  cap; an exhausted grant, a gateway refusal and a dropped connection each surfacing as a sentence.
- **Thread:** a turn persists, yesterday's thread is gone today, *Keep this* writes once.

## 8. Delivery

1. Spec and amendments (this change).
2. Backend: migration, grant route, summed allowance loop, revocations, tests, README.
3. ThockKit: search index, tools, prompt, grant client, loop, thread store, tests.
4. App: the Ask screen.
5. Second pass, separately: appends by request; a desk one-liner in Wrap Today so `# Asked on the go`
   reads as the agent's voice (V33 §15 #4, not yet shipped).

## 9. Risks

- **Two harnesses.** Pi at the desk, a Swift loop on the phone. Shared prompt sections and the copy
  test bound the drift in words; behavior can still differ in tool use. Accepted; the phone's tool
  surface is deliberately tiny.
- **Keyword recall.** "What helped last time" depends on the words in the notes. The model can search
  several times; if that proves too blunt, V28 Phase 3 (local embeddings) is the follow-up.
- **Default-tier judgment.** The car and overwhelm questions lean on model quality. The tier map is
  backend config (V25 decision 14), so this is tunable without an app release; it is the same open
  question as at the desk.
- **Twice the cap, worst case** (§5.1). Bounded, and only with the backend down and both keys abused.
- **A key on the phone.** In the Keychain-backed store's memory only for the session; fetched per
  grant, never written to the vault or the database. Revoked with the device.
- **Stale vault.** Answers reflect the last upload from the desk. With the desk closed that is the
  latest state there is.
- **App review and privacy copy.** Note text goes to the model provider from the phone; the privacy
  page and the App Store privacy answers need the same sentence the desk's hosted agent already has.
