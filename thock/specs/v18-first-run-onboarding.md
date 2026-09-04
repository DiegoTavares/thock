# Thock V18 — First-Run Onboarding & the Agent-Agnostic Vault

**Status:** Scope-locked from proposal review (2026-09-03), ready for implementation
**Owner:** Diego · **Date:** 2026-09-03
**Companion docs:** `../VISION.md` (§5.5 Onboarding, §12 Milestone 4), `v5-agent-and-onboarding.md`
(the BYO-agent rails and per-Routine onboarding this builds on), `v7-dynamic-routines.md` (bridge
generation, lockfiles), `v11-routines-rail.md` (the panel this adds a section to)

---

## 1. Summary

V18 is the app-level first-run experience Milestone 4 has been deferring, driven by a concrete
deadline: the first beta tester is a non-developer who has never used an IDE and whose agent is
**Gemini CLI**, not Claude Code.

Two workstreams, one story:

1. **The vault speaks to every agent.** A canonical `AGENTS.md` at the vault root (with `CLAUDE.md`
   and `GEMINI.md` symlinked to it), skill bridges generated for Gemini alongside Claude, and a
   de-Claude pass over shipped copy. A Gemini user gets exactly the experience a Claude user gets.
2. **The first ten minutes are guided.** A rewritten `welcome.md` that opens rendered, a visual
   **Introductory Guide** (vault-local HTML, the dashboard pattern) telling the philosophy, a
   **Getting started** checklist section pinned atop the Routines rail on fresh vaults, and a
   **Welcome Tour** ritual in which the user's own agent introduces Thock, creates today's note with
   them, and demonstrates the append contract.

There is still **no GPUI wizard** — V5 §5.3's doctrine ("onboarding is a skill") holds. The only new
native surface is one panel section.

## 2. Locked decisions (from the 2026-09-03 proposal review)

| # | Decision | Choice |
|---|---|---|
| 1 | Guide medium | **Vault-local static HTML** (`guide/index.html`), opened as a browser link — the dashboard-output pattern, not a markdown chapter. |
| 2 | Agent files | **Symlinks**: `CLAUDE.md → AGENTS.md`, `GEMINI.md → AGENTS.md`. One source of truth; fall back to generated pointer files only if a symlink can't be created. |
| 3 | Checklist audience | **Fresh vaults only.** Existing vaults gain `AGENTS.md`, symlinks, and Gemini bridges via reconcile, but never see the Getting started section. |
| 4 | Scaffolded setup rituals stay quiet | A fresh scaffold's Timeline/Inbox registry entries continue to carry no `onboarding_state`, so Set Up Timeline / Set Up Inbox never badge or auto-launch on first run. This extends V5 locked decision 14 (pre-V5 installs are quiet) to scaffolded installs, now **by documented intent**: a brand-new user must not be greeted by a migration interview. The Welcome Tour and the collapsed Setup rows are the paths to them. |
| 5 | Wizard | None. Files, rituals, and one panel section. |
| 6 | Vault picker | Out of scope. `~/Thock` stays the hardcoded first-run vault. |
| 7 | Naming | The product word remains **"agent"** (V5 decision 15) — "Connect your agent", never "helper" or "assistant", in UI and shipped docs alike. |

## 3. Goals & success criteria

**Primary:** a non-technical person with Gemini CLI installed opens Thock for the first time and,
within ten minutes and without help, has read what Thock is, connected their agent, and completed
one real ritual — with today's note showing their own words above an appended section.

**Definition of done:**

1. A fresh scaffold writes `AGENTS.md` (canonical), `CLAUDE.md` and `GEMINI.md` (symlinks), and
   `guide/index.html`; `reconcile_vault` adds the same to existing vaults, create-if-missing.
2. Every place that generates a `.claude/skills/<id>/SKILL.md` bridge also generates a
   `.gemini/commands/<id>.toml` bridge, with the same materialize / activate / migrate / removal
   lifecycle. `/wrap-today` and `/triage-inbox` work identically in Claude Code and Gemini CLI.
3. `model = "fast"` skills get a derived fast command under Gemini
   (`-m gemini-flash-latest`) as they do under Claude (`--model haiku`); a command that already
   pins a model (`--model` or `-m`) is never second-guessed.
4. No shipped skill, doc, or UI string assumes Claude Code. `routines/ROUTINES.md`'s protected-path
   rule covers `.gemini/` alongside `.thock/` and `.claude/`.
5. On a cold start with no vault, `welcome.md` opens **rendered** (markdown preview, the V5 §7.6
   tour surface), rewritten for a note-taker and pointing into the Guide.
6. On a fresh vault, the Routines panel shows a **Getting started** section above the routine
   sections with four rows — *Read the introduction*, *Customize*, *Connect your agent*,
   *Take the tour* — fully keyboard-operable, each checking off on real evidence, the section
   disappearing once all four are done. It never appears in a pre-existing vault.
7. The **Welcome Tour** core skill (`skills/thock/welcome-tour.md`) exists, is agent-agnostic,
   append-only, ends by writing its done marker, and is wired to the checklist's third row.
8. The Agent panel has a default keybinding on macOS and Linux.
9. `DEFAULT_CONFIG_TOML`'s registry versions match the shipped manifests (timeline 9, inbox 2).
10. VISION §12 Milestone 4 "First-run onboarding" flips to shipped in the same change.

## 4. Non-goals (explicitly out of V18)

- **No vault/folder picker**, no "open my Obsidian vault", no multi-vault or recents. The Set Up
  Timeline migration interview remains the "I already have notes" path.
- **No GPUI wizard or modal tour.**
- **No BYO-key cost visibility** (Milestone 4's other line — separate effort).
- **No Codex command bridge.** Codex reads `AGENTS.md` natively, so context is covered; a
  `.codex/` prompt bridge waits for a Codex user.
- **No localization** of the Guide or tour.
- **No changes to the V5 per-Routine onboarding state machine** beyond documenting decision 4 above.

## 5. Design

### 5.1 The agent-agnostic vault

**`AGENTS.md`** — a new core-owned file (asset `crates/thock/assets/AGENTS.md`, materialized by
`materialize_core_files`, create-if-missing so user edits stick). Written *to the agent*, covering:

- what this vault is (a person's notes, not a codebase — write like a thoughtful assistant, not an
  engineer);
- the binding rules: append or insert sections, never rewrite the user's words; confirm anything
  that matters before doing it; never write under `.thock/`, `.claude/`, or `.gemini/`;
- the vault map: `daily/`, `weekly/`, `backlog.md`, `inbox/`, `templates/`, `routines/`, `skills/`;
- how rituals work (skills are files; "Read and execute <path>"), and pointers to
  `routines/ROUTINES.md` and each installed Routine's doc.

**Symlinks.** `CLAUDE.md` and `GEMINI.md` are created as relative symlinks to `AGENTS.md`, only
when the path does not already exist (a user's hand-written `CLAUDE.md` is never touched — the
create-if-missing discipline applies to links too). `AGENTS.md` is the standard Codex/Jules/Cursor
read natively; Gemini CLI defaults to `GEMINI.md` and declined to read `AGENTS.md` by default;
Claude Code reads `CLAUDE.md`. If the filesystem refuses a symlink, fall back to a one-line pointer
file ("Read `AGENTS.md` and follow it.") and log — never fail the scaffold over it.

**Gemini bridges.** `ClaudeBridge` generalizes to an agent bridge set: for each manifest skill,
emit both

- `.claude/skills/<id>/SKILL.md` (unchanged), and
- `.gemini/commands/<id>.toml` — Gemini CLI's project-command format:

```toml
description = "<skill summary>"
prompt = "Read and follow the full instructions in <skill file> (relative to the vault root), then carry out the ritual it describes. It appends to your notes and never rewrites what you wrote."
```

One generator feeds all four call sites (materialize, activate, pre-V7 migration, removal plan), so
lifecycle parity is structural. Removal compares each bridge against its freshly generated content,
exactly as Claude bridges do today.

**Fast tier.** `derived_fast_command` learns `gemini` → append `-m gemini-flash-latest` (the
rolling alias, as `haiku` is for Claude), and its already-pinned check covers `-m` as well as
`--model`.

**Copy sweep.** Shipped skills and docs drop Claude-specific phrasing: "AskUserQuestion in Claude
Code" becomes "your agent's structured question tool (or numbered questions in plain text)";
routine docs describe slash commands as available in Claude Code and Gemini CLI. The Agent panel's
empty-state copy already names all three CLIs and stays.

### 5.2 welcome.md, rewritten and rendered

`DEFAULT_WELCOME` is rewritten for a note-taker: what you're looking at (the vault is a normal
folder of files that belong to you), the panels by their jobs (your days on the left, your day's
shape on the right, your lists at the bottom), your agent, and "start with the Getting started list
in the left rail". It links to `guide/index.html` and stays a plain note the user may delete.

`open_startup_vault`'s fresh-scaffold branch opens it via the markdown-preview path
(`open_abs_path_as_preview`) instead of a raw buffer. Existing vaults keep their existing
`welcome.md` untouched (create-if-missing as today).

### 5.3 The Introductory Guide

`guide/index.html` — one self-contained static HTML file, shipped as a core asset and materialized
create-if-missing (dashboard-output pattern: viewable in any browser, editable like everything
else, no network). Content is the philosophy of VISION §4 in plain language:

- **Your files, forever** — normal files in a normal folder; nothing locked in.
- **Augmentation, not replacement** — the AI adds its part below yours, never rewrites.
- **You confirm anything that matters** — it recommends; you act.
- **The vocabulary** — notes, rituals, Routines, your agent — with a labeled sketch of the
  workspace.
- **Time travel** — every change is kept; nothing you do can lose your words. (The word "git" does
  not appear.)

Reachable forever: the checklist's first row, a `thock: open guide` palette action, and a link in
`welcome.md`.

### 5.4 The Getting started section

Rendered by the Routines panel above the routine sections, only while active. Four rows, each a
standard keyboard row (`menu::Confirm` runs it, arrows/vim motions move, chords render like other
rows):

| Row | Action on confirm | Checks off when |
|---|---|---|
| **Read the introduction** | opens `guide/index.html` in the browser | first opened (marker written) |
| **Customize** | opens `guide/customize.md` as a rendered preview and pops the live theme selector on top — themes first (dark is only the default), then text size, then the shortcuts and how to rebind them (edit the keymap, or ask your agent) | first opened (marker written) |
| **Connect your agent** | opens the Agent panel's connect flow | `agent::resolved_command` returns a command (live evidence, no marker) |
| **Take the tour** | `RunSkill` on `skills/thock/welcome-tour.md` (connect-first interstitial applies, as V5 §7.3) | done marker `.thock/state/onboarded/welcome-tour` appears (watched dir already) |

**State** lives under `.thock/state/getting-started/`, never in `config.toml` (V7 §9 trap 4:
`deny_unknown_fields` means a new config key bricks the vault for older builds):

- `active` — written by `scaffold_vault` **only when it creates the vault** (the same
  `install_default_routines` condition), which is what "fresh vaults only" means mechanically;
- `introduction` / `customize` — written when the guide or the customize page is first opened
  from its row.

The section renders while `active` exists; when all four rows are complete it is removed and the
section disappears (state transitions persist across restarts; completed rows render checked, not
hidden, until then). A `thock: hide getting started` palette action removes it early; selection
survives refresh per the panel rules.

The customize page (`guide/customize.md`, a core-materialized vault note, also reachable via
`thock: open customize`) exists for two first impressions: the dark editor ("is this the only
look?") and the command palette combo — the one piece of IDE vocabulary a non-technical user
can't discover on their own. Confirming the row dispatches `theme_selector::Toggle` after the
preview opens, so the first thing that happens is the app changing look live as the user arrows
through themes; the page underneath then covers text size ("⌘+"/"⌘-", `buffer_font_size`), leads
the keys with "⌘⇧P — do anything", lists the panel toggles (rail `cmd-2`, planner `cmd-alt-p`,
backlog `cmd-alt-u`, agent `cmd-alt-t`, `esc` back to the note), and closes with how to change
them — including "ask your agent", with the keymap path named so the agent can act.

### 5.5 The Welcome Tour ritual

`skills/thock/welcome-tour.md`, a core-owned skill materialized beside `new-routine.md`. Written to
the agent, agent-agnostic, with the V5 contract shape (role & ground rules, conventions, playbook,
completion protocol). The beats:

1. Introduce yourself; ask two light questions, one at a time (their name; what they hope Thock
   helps with). Conversational — no forms, no jargon.
2. Create today's note (create-if-missing from the template; `thock: open today` also exists).
   Invite them to type three things on their mind as `- [ ]` checkboxes under `# Day planner`,
   and point at the Day Planner rail drawing them live.
3. Demonstrate the append contract in miniature: add a short `# Getting started` section *below*
   their words, and say plainly: I only ever add, I never rewrite what you wrote.
4. Introduce the Routines rail — these are your rituals; run them any time — and offer a tiny
   Wrap Today to close the loop (their call; skipping is fine).
5. Write the done marker `.thock/state/onboarded/welcome-tour` with a one-line summary, and say
   the Getting started list will complete itself.

Everything appended, everything confirmed — the trust contract taught by experiencing it.

### 5.6 Small repairs riding along

- `DEFAULT_CONFIG_TOML` registry versions: timeline `7 → 9`, inbox `1 → 2` (cosmetic today, but a
  version-gated check must not be misled).
- Agent panel and Day Planner default keybindings (macOS + Linux, same chords: `cmd-alt-t` /
  `cmd-alt-p` and ctrl equivalents), added inside the existing Thock keymap blocks — entries only,
  no restructuring. The Day Planner had a `ToggleDayPlannerFocus` action but no binding, so the
  shortcuts page had nothing to teach for it.
- Default `buffer_font_size` `15 → 17` in `assets/settings/default.json` (the V12 precedent file
  for setting-level flips): vault notes are prose, and 15 is a code size.

## 6. Implementation notes

- **Crate placement:** everything in `crates/thock` and its assets. Upstream touch-points: the two
  keymap files (one line each) and nothing else — `open_startup_vault` and the panel are already
  Thock-owned.
- **Symlinks:** `std::os::unix::fs::symlink` with relative target `AGENTS.md`; check existence
  with `symlink_metadata` (a dangling symlink still counts as present — never overwrite). The
  pointer-file fallback keeps scaffold infallible on exotic filesystems.
- **Bridge refactor:** rename `ClaudeBridge` → `AgentBridge`, `claude_bridge_files` →
  `agent_bridge_files` returning both bridges per skill; all call sites (materialize, activate,
  migration §1480, removal plan §1777) pick the change up mechanically. TOML string escaping via
  `toml` serialization, not hand-rolled quoting.
- **Checklist state:** plain marker files, read on the panel's existing vault-refresh path; the
  connect-command check rides the panel's existing background resolve (it already races config
  edits safely). No new watcher: `.thock/state/` is already in the watched set for onboarding
  markers.
- **Guide asset:** self-contained HTML, no external requests, dark/light via
  `prefers-color-scheme`; keep it well under the size where `include_str!` hurts.
- **Testing:** unit — scaffold writes AGENTS.md + links + guide + `getting-started/active`;
  symlink fallback; bridge parity (every Claude bridge has a Gemini twin; removal plan deletes
  unmodified Gemini bridges and keeps edited ones); `derived_fast_command` for gemini and the `-m`
  pin check; checklist state transitions. Integration — the V5 fake-agent script pattern: a shell
  script touches `.thock/state/onboarded/welcome-tour` and the checklist completes. Live — fresh
  `$HOME` first-run (V1's live-test pattern), then a full ten-minute dry run with Gemini CLI
  before the beta tester gets a build.

## 7. Phasing & deliverables

| Phase | Ships | Mergeable alone? |
|---|---|---|
| **1 — Agent-agnostic vault** | AGENTS.md + symlinks, Gemini bridges, fast tier, copy sweep, ROUTINES.md rule | **Yes** — immediately dogfoodable with `gemini` |
| **2 — Introduction** | welcome.md rewrite + rendered open, `guide/index.html`, `thock: open guide` | Yes |
| **3 — Getting started** | Panel section + state, keybinding, registry-version fix | Yes, atop 1–2 |
| **4 — Welcome Tour** | `skills/thock/welcome-tour.md` + marker wiring | Yes, atop 3 |

## 8. Future work (explicitly deferred)

- **Vault picker / existing-folder import** as a real first-run step (VISION §5.5 step a).
- **Codex command bridge** (`.codex/` prompts) when a Codex user materializes.
- **Onboarding telemetry of any kind** — none; the only signals are the local markers.
- **A "first ritual" celebration surface** beyond the checklist completing.
- **Guide localization** and a non-English tour.
