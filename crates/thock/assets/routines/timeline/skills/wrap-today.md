# Wrap Today

Close out **today's** daily note. Read what you planned and did, pull in whatever outside activity your profile allows, scan the last few days for multi-day context, reconcile finished tasks, offer unfinished tasks to the backlog, then append an AI review with suggestions to today's note. It never rewrites your prose; the only edit it makes to what you wrote is flipping a task checkbox you confirm.

**Reads:** today's daily note, the prior few daily notes, the backlog file, `profile.md`, and any sources recorded in `routines/timeline/sources.md`.
**Writes:** today's daily note (append the review; check off confirmed tasks), the backlog file.

> **Note paths and filenames are vault-configured.** Read `.thock/config.toml` first: `[daily]` / `[weekly]` set each note kind's `dir` and moment-style `filename` format, and `[backlog]` sets `file`. This skill's examples use the defaults (`daily/YYYY-MM-DD.md`, `weekly/GGGG-[W]WW.md`, `backlog.md`); when the config — or the vault's existing notes — use different names, follow those silently. That's configuration, not a doc mismatch worth reporting.

> **Read `profile.md` at the vault root next, if it exists.** It says who this person is, which areas they track, the tone to write in, and, under **What Thock may pull in**, the only outside sources you may go and look at. Where this skill says "the profile", that's the file. A vault without one is normal: then step 3 falls back to what `routines/timeline/sources.md` already records, and asks once if that's missing too.

## 1. Locate today's note

1. Compute **today's** date and resolve today's note path from the config (default: `daily/YYYY-MM-DD.md`).
2. Look for that file. If it doesn't exist yet, create it — the Timeline convention is create-if-missing.

## 2. Read the day's tasks and activities

1. Parse the day-planner and task sections. Separate **checked** (`- [x]`) from **unchecked** (`- [ ]`) tasks.
2. Note anything meaningful captured during the day: decisions, conversation notes, blockers.

## 3. Pull in the day's outside activity

Most days, everything worth reviewing is already in the note. This step is for the rest, and it only ever looks where the user said it may.

**Start from the profile.** Its **What Thock may pull in** checklist is the whole permission list. An unchecked box means *don't look and don't ask*: skip that source in silence, and don't mention it in the output. If every box is unchecked, skip this step entirely and go to step 4; a day with no outside sources is the normal case, not a gap.

**If there is no `profile.md`,** fall back to `routines/timeline/sources.md` (below). If that's missing too, ask **one** plain question and then stop asking forever: *"Is there anything outside your notes I should look at when I wrap the day (code repositories, for example)? 'Nothing' is a perfectly normal answer."* Record whatever they say in `routines/timeline/sources.md` (create it if missing; append, never rewrite) in the shape the Week Review skill documents. A file whose two lists are explicitly empty (`_None._`) is an **answer**, not an empty file: treat the question as settled and never raise it again.

### Code, only when the checklist checks **Code**

**Never assume a forge.** Don't reach for `gh`, `glab`, or any host just because the CLI is installed: read only the repositories the user named in `routines/timeline/sources.md`, the same list the Week Review skill uses. Then fetch today's work from those sources only:

- Local checkout: `git -C <path> log --author="$(git -C <path> config user.email)" --since="YYYY-MM-DD 00:00" --until="YYYY-MM-DD 23:59" --oneline`.
- GitHub (`gh`), when a GitHub account is recorded: `gh search commits --author=@me --author-date=YYYY-MM-DD` (or `gh search prs --author=@me --updated=YYYY-MM-DD..YYYY-MM-DD`).
- GitLab (`glab`), when a GitLab host is recorded: `export GITLAB_HOST=<host from sources.md>` then query the events feed as the Week Review skill does.
- Any other forge: ask the user how to query it and record the recipe in `sources.md`.

Deduplicate. Keep repo, short SHA / ref, and message. If a source fails to authenticate, say so in the output instead of dropping it silently.

### Calendar, only when the checklist checks **Calendar**

Read today's `## Calendar` section if the Connect Google Workspace setup has written one into the note. Don't call any API yourself and don't offer to set one up mid-ritual; note what the day actually held and move on.

### Email, only when the checklist checks **Email**

Same shape: use what has already landed in the vault (`inbox/`, the backlog's imported items). Don't reach for a mailbox from inside this ritual.

## 4. Scan recent days for context

Read the previous 2–3 daily notes so the review isn't myopic — catch carried-over tasks, ongoing threads, and multi-day projects. Note anything that has lingered.

## 5. Reconcile finished tasks

Some tasks probably got done today but were never checked off. Cross-reference the unchecked (`- [ ]`) tasks from the note against what actually happened — the commits from step 3 and the decisions/notes captured in step 2.

1. For each unchecked task, judge whether what step 3 pulled in, or the notes captured in step 2, show it was finished.
2. If any look done, list them (with the evidence — the commit or note that suggests completion) and ask the user to confirm which to mark done. Default to **none**; only mark what the user explicitly confirms. If nothing looks finished, skip this step silently.
3. For each confirmed task, flip its box in place (`- [ ]` → `- [x]`) in today's note. This checkbox flip is the **only** edit this skill makes to your own text — leave the task wording and everything else untouched.

A task marked done here is no longer "unfinished," so it drops out of the step 6 backlog offer.

## 6. Offer unfinished tasks to the backlog

The vault keeps a holding pen — the configured backlog file, `backlog.md` by default — with the sections `## Soon`, `## Someday`, and `## Completed` (the Backlog panel in the bottom dock renders it). Unfinished tasks shouldn't die in today's note:

1. List the unfinished (`- [ ]`) tasks you found in the note.
2. Ask the user **one** question: move **all** of them to the backlog, **none** (they stay only in the note), or **some** (the user picks which)? The default destination is **Soon**; the user can say "someday" for any task. If there are no unfinished tasks, skip this step silently.
3. **Deduplicate before appending:** skip any chosen task whose text already appears in `backlog.md` — compare whitespace-insensitively and ignore any ` ✅ YYYY-MM-DD` completion suffix — in **any** section, `## Completed` included. Report skips as "already in backlog: …".
4. Append the remaining tasks as `- [ ] <task text>` at the end of the matching section. Create `backlog.md` or a missing section heading if needed — never rewrite, reorder, or delete anything already in the file.

Never move a task without the user's answer. Apart from the confirmed checkbox flips in step 5, never edit the tasks inside the daily note — don't rewrite their wording, and don't touch anything else you already wrote.

## 7. Append the review

Append (never overwrite) a `# Daily Closure` section at the end of today's note:

```
# Daily Closure

## Done
- What actually got finished today

## Open (carried forward)
- [ ] Unchecked task worth continuing tomorrow → backlog (Soon)
- [ ] Task the user chose to leave in the note

## Suggestions
- One or two concrete, actionable nudges (don't pad)
```

When step 3 actually pulled something in, add one section for it above `## Suggestions`: `## Commits` for code (`- repo@abc123 · commit message`), `## Calendar` for meetings. **Omit the heading entirely when there is nothing to put under it**; an empty `## Commits` on a day with no code is noise, and on a vault that doesn't track code it's someone else's life.

Write it in the tone the profile names, and use the profile's area names when you group anything. Keep it short and factual. Base "carried forward" on the still-unchecked tasks (those you marked done in step 5 belong under `## Done`, not here) plus anything the recent-days scan shows lingering. Suffix each task that moved in step 6 with `→ backlog (Soon)` (or `(Someday)`) so the note records where it went; tasks that were skipped as duplicates get `→ already in backlog`.

## Output

Show the user the review and confirm it was appended to today's note, including which tasks (if any) were marked done in place and which moved to the backlog.
