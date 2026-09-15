# Thock V29: Updates that reach existing vaults

**Status:** Implemented (2026-09-15)
**Owner:** Diego · **Date:** 2026-09-15
**Companion docs:** `../VISION.md` (§4 "Everything is editable", §12 roadmap),
`v7-dynamic-routines.md` (the files lock and create-if-missing scaffold this extends),
`v18-first-run-onboarding.md` (`AGENTS.md` and the core files), `v19-vault-language.md` (the
`## Language` section this must carry over), `v28-agent-memory.md` (the change that exposed the gap)

---

## 1. Summary

Everything Thock scaffolds into a vault (rituals, Routine docs, `AGENTS.md`, the core skills) is
written create-if-missing and then belongs to the person. That is the right ownership, and it has
a cost: a vault that predates a release never sees what the release changed in those files. V28
made it concrete: the Reflect step appended to Wrap Today reaches new vaults only.

V29 closes the gap in two halves, ordered by how much of the problem each solves:

- **Mechanical, no agent.** The app already records a content hash per shipped Routine file. On
  every reconcile pass, a shipped file whose on-disk hash still matches what the app last shipped
  has never been edited and is replaced in place. Core files gain the same lock. Most files in most
  vaults are untouched, so most upgrades land silently and safely.
- **Agent-assisted, for edited files.** When the on-disk content is neither the current shipped
  text nor the previously shipped text, the person edited it. The new shipped version is staged
  at `.thock/pending/<vault path>` and a core ritual, **Update Rituals**, merges it with their
  edits, showing each result and asking before writing. The rail shows a "rituals to update" row
  while anything is pending.

## 2. Locked decisions (2026-09-15)

| # | Decision | Choice |
|---|---|---|
| 1 | What upgrades mechanically | **Routine docs and skills, and the core files** (`AGENTS.md`, `routines/ROUTINES.md`, the core rituals, `guide/customize.md`). Not scaffold seeds (`weekly/site/data.js`, the dashboard page): once written they are the person's data. Not `routine.toml`, which keeps its existing unmodified-only upgrade. Not templates or notes, ever. |
| 2 | Unmodified means | **On-disk hash equals the hash the app recorded when it last shipped the file.** Routines already keep that in `.thock/routines/<id>/files.lock`; core files gain `.thock/core-files.lock`. A vault with no core lock yet (first run after V29) cannot prove a core file is pristine, so it stages instead: safe, one-time. |
| 3 | Where an edited file's update waits | **`.thock/pending/<vault-relative path>`**, holding the shipped version verbatim. App-owned state, never in the vault proper. |
| 4 | Once per shipped version | **`.thock/updates.lock`** records the hash of every shipped version staged. A version the person declined is never staged again; the next release that changes the file is. |
| 5 | Pending clears when | The vault file becomes identical to the shipped text (by hand or by the ritual taking the shipped version), or the ritual removes the pending file after handling it. |
| 6 | The ritual | **`skills/thock/update-rituals.md`**, `thock::UpdateRituals`, default tier (merging is judgment). Their words win; shipped text they never touched may be replaced; new sections are added unless they deleted that section; `## Language` in `AGENTS.md` is carried over untouched; nothing is written without a yes per file. |
| 7 | Surface | **One row in the Routines rail** under the addable Routines, "N rituals to update", with the pending paths in its tooltip; clicking or `thock::UpdateRituals` runs the ritual. No chat nudge. |
| 8 | Trust | This is the one place the no-gates model (V25 decision 16) still confirms: the files are the person's own edits, so each merge is shown and approved. History remains the undo. |

## 3. Goals

**G1 — A release changes Wrap Today; every vault that never edited it has the new text after its
next open, with no agent run.**

**G2 — A vault that edited Wrap Today keeps its edits, sees a "1 ritual to update" row, and after
the ritual has the new step and its own edits both.**

**G3 — `AGENTS.md` with a `## Language` section keeps that section through an update.**

**G4 — Declining an update is respected until the next release changes that file.**

## 4. Non-goals

- **No three-way merge.** The app does not ship the previous release's text, so the ritual merges
  two versions with section-level judgment and shows the result. Good enough for Markdown rituals;
  revisit if merges go wrong in practice.
- **No upgrades of templates, notes, `profile.md`, `memory/` or config.** They are content, not
  scaffolding.
- **No automatic runs of the ritual.** The row waits; nothing is merged unattended.

## 5. Design

### 5.1 Reconcile pass

`upgrade_shipped_file(vault_root, relative, packaged, previous_hash)` in `routines.rs`:

| On disk | Action |
|---|---|
| missing | write shipped |
| equals shipped | nothing; clear any pending copy |
| hash equals `previous_hash` | replace in place; clear any pending copy |
| anything else | stage under `.thock/pending/` unless `updates.lock` already holds this shipped hash |

`materialize_routine` runs it for every declared file flagged `upgradeable` (docs, skills), with
`previous_hash` from the Routine's files lock, before the lock is rewritten with the current
shipped hashes. `materialize_core_files` runs it for the core file list with `.thock/core-files.lock`
as the previous lock and writes the current hashes after.

### 5.2 Pending and the lock

`pending_updates(vault_root)` lists staged paths. `refresh_fingerprint` folds them in, so the rail
re-renders when the ritual removes one. `updates.lock` is a `FilesLock` keyed by vault path.

### 5.3 The ritual

Steps in `update-rituals.md`: list pending; explain once; per file read both versions, work out
the person's edits section by section, build the merge by the ground rules, show what changes and
the full merged file, ask write / skip / take shipped; write with the `write` tool on approval;
delete the pending file in every case; finish with counts and a reminder that history holds the
originals.

## 6. Definition of done

1. An unmodified older shipped Routine skill is replaced in place on reconcile; the user-edited one
   is staged verbatim and the edit survives (tests).
2. Declining (deleting the pending file) is remembered for that shipped version; a hand-restored
   file clears a stale pending copy (test).
3. Core files: an edited `AGENTS.md` is staged; a pristine older core skill upgrades (test).
4. `skills/thock/update-rituals.md` ships with the core files; `thock::UpdateRituals` runs it.
5. The rail shows the row while anything is pending, keyboard-reachable through the action.
6. VISION §12 marks the entry shipped.

## 7. Risks

- **First run after V29 stages rather than upgrades core files** that differ, because no core lock
  existed. One ritual run clears it; afterwards core upgrades are mechanical.
- **Two-way merge on a cheap model** may misjudge a section. The per-file approval and history
  bound the damage; the ritual is told to fall back to "whole shipped or whole yours" when it
  cannot tell what changed.
- **`.thock/` gains two locks and a folder.** Older builds ignore unknown files under `.thock/`;
  nothing is added to `config.toml`.
