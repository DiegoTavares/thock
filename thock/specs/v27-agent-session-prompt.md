# Thock V27: The agent's session prompt

**Status:** Implemented (2026-09-14)
**Owner:** Diego · **Date:** 2026-09-14
**Companion docs:** `../VISION.md` (§4 invariants, §9 trust model, §12 roadmap),
`v25-thock-plus-hosted-agent.md` (the harness this configures),
`v26-agent-chat-refinement.md` (decision 12 deferred this work to its own spec),
`v19-vault-language.md` (the `## Language` contract this makes reachable),
`v7-dynamic-routines.md` (the manifests the context block reads)

---

## 1. Summary

The hosted agent has a system prompt — `assets/hosted-agent/SYSTEM.md`, written to Pi's config
directory by `hosted_agent::write_pi_config` — and it works. What it lacks is **everything that is
true only right now**. Pi assembles its prompt as `SYSTEM.md` → `APPEND_SYSTEM.md` →
`<project_context>` (the vault's `AGENTS.md`) → `Current working directory: …`, and nothing in that
chain carries a date. That is the whole of V26's wrong-year defect: the agent resolved "last week"
to 2023 because it has never been told what year it is.

V27 fixes that and finishes the prompt around it. `SYSTEM.md` becomes the agent's **identity** — who
it is to a person who does not program, how it speaks, what it may touch, and what it honestly
cannot do. A new generated `APPEND_SYSTEM.md` carries the **live facts**: today's date, where
today's and this week's notes live, the vault's configured language, and which Routines are
installed with which rituals.

The split matters. `SYSTEM.md` is a static asset a reader can open and edit; the context block is
composed from `Vault` and the Routine manifests at launch and is never hand-written.

## 2. Locked decisions (2026-09-14)

| # | Decision | Choice |
|---|---|---|
| 1 | Where live facts go | **A generated `APPEND_SYSTEM.md`**, beside the static `SYSTEM.md` in the same Pi config directory. Pi loads it immediately after the system prompt and before `AGENTS.md`. Keeps the asset readable prose and the composition unit-testable. |
| 2 | Per-turn vs. per-session | **Per-session.** The block is composed once, when the process launches. A per-turn preamble would land in the transcript the user reads and fight V26's whole point. Cost: a session running past midnight keeps yesterday's date (risk R1). |
| 3 | Overlap with `AGENTS.md` | **The safety rules are stated in both; the map is stated once.** `AGENTS.md` is the user's file — editable, translatable, deletable — so the prompt cannot depend on it for append-never-rewrite, vault scope, or the `.thock/` rule. The vault map, by contrast, lives in `AGENTS.md` and the context block, not in `SYSTEM.md`. |
| 4 | Language | **The prompt points at the binding source rather than restating it.** Today's `Write in the language the vault is written in` is a guess; the contract is V19's `## Language` section, with `[language]` from `.thock/config.toml` echoed in the context block. With neither set, mirror the language the person writes in. |
| 5 | Opening files | **No new capability.** The agent cannot open an editor tab, and the prompt says so plainly so it names the file instead of claiming it opened one. A real open-note tool is deferred. |
| 6 | Config directory key | **Keyed by tier *and* vault.** `pi_config_dir` is keyed by tier alone today, which is safe only while its contents are vault-independent. A vault-specific `APPEND_SYSTEM.md` makes two open vaults race over one file. |
| 7 | Routine detail | **Name, folder, explainer doc and the list of rituals with their paths.** Enough for the agent to follow a ritual when asked for what it covers, without pasting the rituals themselves into every session. |

## 3. Goals & success criteria

**G1 — The agent knows what day it is.** Ask "what did I do last week" in a fresh session and it
resolves to the right ISO week of the right year, without asking, and without being told.

**G2 — It knows the person's vault.** Without reading a file first, it can name today's note, this
week's note, the backlog file and its configured headings, and the Routines that are installed.

**G3 — It speaks the vault's language from the first word.** In a vault where the Set Language
ritual ran, the greeting is already in that language — not the second sentence, after it reads
`AGENTS.md`.

**G4 — It never claims to have opened something.** Asked to open a note, it names the file and says
the person opens it. Verifiable by reading the prompt, not by a test.

**G5 — The prompt is a file a person can read.** `SYSTEM.md` stays prose with no placeholders. The
generated block is separate, and its composition is covered by unit tests over synthetic vaults.

## 4. Non-goals

- **An open-note tool, or any new tool surface on the hosted path** (decision 5).
- **Per-turn context injection** (decision 2).
- **Touching `agent_panel.rs` or the BYO-CLI path.** Those agents read `AGENTS.md` and nothing else;
  V27 does not give them a system prompt.
- **Translating the prompt.** `SYSTEM.md` and the context block stay English; the *instruction* to
  speak another language is what makes the agent switch. Consistent with V19 decision 1.
- **Changing `AGENTS.md`.** Its content is right; V27 only stops the system prompt from
  contradicting it.
- **Prompt-level enforcement of skill scopes.** Still V25 Stage 2's sandbox.

## 5. Design

### 5.1 Two files, one order

Pi's `buildSystemPrompt` concatenates, in order: the custom prompt, the append block, each context
file inside `<project_context>`, then the cwd line. V27 uses all three slots deliberately:

| Slot | File | Owner | Content |
|---|---|---|---|
| custom prompt | `SYSTEM.md` (static asset) | Thock | Identity, tone, the rules that must survive a deleted `AGENTS.md` |
| append | `APPEND_SYSTEM.md` (generated) | Thock, per launch | Today's date, the vault's paths and language, installed Routines |
| context | `AGENTS.md` | the user | The vault's own conventions — last word, because it is theirs |

Only the global (`agentDir`) copies are used. Pi also honors project-local `.pi/SYSTEM.md`, but only
for a trusted project, and the hosted harness pins `defaultProjectTrust: "never"`.

### 5.2 `SYSTEM.md`

Rewritten around six beats: **who you are** (the one part of a notes app that can read and write,
talking to someone who does not program), **how you speak** (plain, short, vault-relative paths,
never claim what you did not do), **how you work in the vault** (append, create-if-missing, stay
inside, never under `.thock/`, hand over the irreversible), **rituals and Routines** (what
"Read and execute <path>" means, follow the ritual rather than improvise, never assume a Routine
exists), **what you cannot touch** (the panels are not yours; you cannot open a tab), and
**language** (decision 4).

### 5.3 The context block

Composed from `Vault`, the enabled Routine manifests, and today's date:

```markdown
# Right now

Today is Monday, 14 September 2026 (2026-09-14) — week 2026-W38.
This person's vault is the folder /Users/…/Thock. Everything you do happens inside it.

- Today's note: `daily/2026-09-14.md` (from `templates/daily.md` when it doesn't exist yet).
- This week's note: `weekly/2026-W38.md` (from `templates/weekly.md`).
- Tasks: `backlog.md`, under the headings `Soon`, `Someday`, `Completed`.
- The day's plan is the `## Today` section of the daily note.
- `profile.md` says who this person is and what you may look at — read it before you start.

## Language

This vault is set to **Portuguese (Brazil)** (`pt-BR`). Speak and write in it, including your
first greeting. Leave file names, folder names, the task syntax (`- [ ]`) and the headings
named above exactly as written.

## Installed Routines

- **Timeline** — `routines/timeline/`, explained in `routines/timeline/doc.md`.
  Rituals: Wrap Today (`routines/timeline/skills/wrap-today.md`), …
```

Every line is conditional on what the vault actually has: no `profile.md` line without the file, a
"no language set — answer in the language they write to you in" line when `[language]` is absent, a
"no Routines installed yet" line when none are enabled. Headings come from the *configured*
`BacklogConfig` and `DayPlannerConfig`, so a translated vault's block names the translated headings
— the same forgiveness V19 built into the parser.

Paths are vault-relative everywhere except the one line that states the vault root, mirroring V26 G2.

### 5.4 Plumbing

`hosted_agent` gains a pure `compose_vault_context(…) -> String` and a blocking
`gather_vault_context(vault) -> String` that reads the manifests and probes `profile.md` around it.
`prepare_launch` takes the `Vault` the chat panel already holds, gathers the block on a background
thread, and hands it to `write_pi_config`, which writes both files. When the workspace is not a
vault, or a previous launch left one behind, `APPEND_SYSTEM.md` is removed rather than left stale.

`pi_config_dir` grows a vault component (decision 6): a short hash of the canonical vault root
appended to the tier segment, so two open vaults never overwrite each other's context block.

## 6. Definition of done

1. `SYSTEM.md` covers the six beats of §5.2 and contains no placeholder syntax.
2. `APPEND_SYSTEM.md` is written beside it on every launch, and removed when there is no vault.
3. The block states today's date, the ISO week, the vault root, today's and this week's note paths,
   the backlog file with its configured headings, and the day-plan heading.
4. `[language]` present → the block names it and says to speak it; absent → it says to mirror the
   person's language. Neither case contradicts a `## Language` section in `AGENTS.md`.
5. Enabled Routines are listed with folder, doc and rituals; none installed renders its own line.
6. `pi_config_dir` is distinct for two different vault roots on the same tier.
7. Unit tests cover the composition: a fresh English vault, a Portuguese vault with translated
   headings, a vault with no Routines, and the not-a-vault fallback.
8. VISION §12 gains a shipped entry in the same change.

## 7. Risks

**R1 — A session outliving the date.** The block is composed at launch (decision 2), so a session
left open past midnight believes it is yesterday. Sessions are one process per action and short in
practice. If it bites, the fix is recomposing on the first message of a turn whose date differs, not
a per-turn preamble.

**R2 — Prompt drift against `AGENTS.md`.** Two files now state the safety rules (decision 3). They
will diverge unless changes to one prompt a read of the other. Mitigated by keeping the overlap to
the four rules that must survive `AGENTS.md` being edited away, and by stating that boundary here.

**R3 — Context that is wrong is worse than absent.** A stale or mis-parsed heading in the block
sends the agent writing under a section the panel does not read. Mitigated by composing from the
same resolved `BacklogConfig`/`DayPlannerConfig` the panels use, never from a parallel default.

**R4 — Length.** The block grows with installed Routines. At the shipped catalog it is a few dozen
lines; a vault with ten Routines would be more. Revisit by summarizing rituals per Routine if a real
vault gets there.
