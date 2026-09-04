# New Routine

> This file is written for the AI agent Thock launches from **New
> Routine with AI**. It's a plain skill — open it, edit it, rerun it whenever
> you like.

You are helping the user create a **new Routine** in their Thock vault:
a life-domain bundle (finance, health, reading, a project…) defined by a
`routines/<id>/routine.toml` file plus the docs and skills it declares.

First, read `routines/ROUTINES.md` — it is the authoritative format
reference, with a worked example and the authoring checklist. Honor it over
anything you remember.

## Ground rules

1. **Interview before acting.** Understand the domain before proposing files.
2. **Confirm before each write batch.** Show the planned layout and get a
   yes first.
3. **Never rewrite user-authored content.** If a file you'd create already
   exists, point at it from `routine.toml` instead of overwriting it.
4. **Never** write under `.thock/`, `.claude/`, or `.gemini/` — except the
   single ready marker in the final step. Thock generates bridges and
   provenance itself at activation.
5. Work inside the vault you were launched in (your current directory).

## The ritual

### 1. Interview

Ask, one question at a time:

- What part of life is this Routine for? What should it be called?
- What files or folders already exist for it (in this vault or elsewhere)?
- What rhythms should it carry — recurring rituals the agent runs (weekly
  review, monthly plan), and what should each one read and write?
- What should be one click away — which files, dashboards, or notes belong
  in the panel as quick links?

### 2. Propose

Sketch the layout before writing anything: the `id`, the folder tree under
`routines/<id>/`, any scaffold dirs for user data, the links, and the skills
with their read/write scopes. Adjust until the user approves.

### 3. Write

- `routines/<id>/routine.toml` — per `routines/ROUTINES.md`.
- `routines/<id>/<Name>.md` — the human explainer (`doc`): what this Routine
  is for and how its pieces fit, written for the user.
- `routines/<id>/AGENT.md` — the agent conventions file (`agent_doc`): data
  sources, guardrails, house style. Future skill runs read this first.
- `routines/<id>/skills/*.md` — one file per ritual, written as
  instructions to an agent: ground rules, steps, and what to append where.
  Skills should append, never rewrite what the user wrote.
- Any declared scaffold dirs.

### 4. Complete

1. Write an empty file at `.thock/state/routine-ready/<id>` (create the
   parent directories if needed) — Thock watches for it and offers
   activation with a toast.
2. Tell the user: the Routine is written, and activating it (the toast, or
   **Add Routine → In this vault** in the Routines panel) is what registers
   it, records provenance, and generates the agent slash-command bridges.
