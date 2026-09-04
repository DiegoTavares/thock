# This vault

You are inside a Thock vault — one person's notes, plans, and life admin,
kept as plain Markdown files in a normal folder. This is not a codebase.
Write like a thoughtful assistant, not an engineer: plain language, short,
no jargon.

_(`CLAUDE.md` and `GEMINI.md` are links to this file — every agent reads the
same instructions.)_

## Ground rules

These are binding for any agent working in this vault:

1. **Append, never rewrite.** Add your synthesis as new sections (or new
   lines in an existing section) below what the user wrote. Never reword,
   reorder, or delete the user's own words unless they explicitly ask for
   an edit.
2. **The human confirms anything that matters.** Propose, then wait.
   Filing, moving, deleting, or acting on the user's behalf happens only
   after they say yes.
3. **Never write under `.thock/`, `.claude/`, or `.gemini/`.** Thock owns
   that machinery. The one exception: state markers a skill explicitly
   documents (ready and done markers under `.thock/state/`).
4. **Create-if-missing.** Daily and weekly notes may not exist yet —
   creating them from `templates/` is normal. Never treat a missing file or
   folder as an error.

## The map

- `daily/YYYY-MM-DD.md` — daily notes; `weekly/YYYY-Www.md` — weekly notes
  (new ones come from `templates/daily.md` and `templates/weekly.md`).
- `backlog.md` — the Soon / Someday / Completed task lists.
- `inbox/` — captured items awaiting triage (when the Inbox Routine is
  installed).
- `routines/<id>/` — installed Routines: each has an explainer doc, its
  skills under `routines/<id>/skills/`, and a `routine.toml` definition.
  `routines/ROUTINES.md` documents the format.
- `skills/` — core rituals (for example `skills/thock/new-routine.md`).

## Rituals

A skill is a Markdown file of instructions. Thock launches you with
"Read and execute <path>" — read that file, follow it, and honor the ground
rules above. Skills are also available as slash commands (`/wrap-today`,
`/triage-inbox`, …) through bridges Thock generates for each CLI.

## Beyond the vault

Keyboard shortcuts live outside the vault, in `~/.config/thock/keymap.json`
(Zed keymap format — bindings like `"cmd-alt-u": "thock::ToggleBacklogFocus"`).
If the user asks to change a shortcut, edit that file, keeping its existing
structure; changes apply live. `guide/customize.md` lists the defaults.

The user may edit any of these files, including this one. The file on disk
is always the source of truth.
