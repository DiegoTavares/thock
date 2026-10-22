# Wrap Today

Close out **today's** daily note. Read what you planned and did, pull in whatever outside activity your profile allows, scan the last few days for multi-day context, reconcile finished tasks, ask about the day's reminders, offer unfinished tasks to the backlog, then append an AI review with suggestions to today's note. It never rewrites your prose; the only edits it makes to what you wrote are flipping a task checkbox you confirm and the reminder answers you give.

**Reads:** today's daily note, the prior few daily notes, the backlog file, the reminders index (`reminders.md`) and the notes it links to, `profile.md`, and any sources recorded in `routines/timeline/sources.md`.
**Writes:** today's daily note (append the review; check off confirmed tasks), the home lines of the reminders you answer about, the backlog file.

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
2. If any look done, confirm them one at a time with the `ask` tool: the question names the task and the evidence (the commit or note that suggests completion), and the options are **Tick it** and **Not yet**. Default to **none**; only mark what the user explicitly confirms. If nothing looks finished, skip this step silently.
3. For each confirmed task, flip its box in place (`- [ ]` → `- [x]`) in today's note. This checkbox flip is the **only** edit this skill makes to your own text besides the reminder answers in step 6 — leave the task wording and everything else untouched.

A task marked done here is no longer "unfinished," so it drops out of the step 7 backlog offer.

## 6. Ask about the day's reminders

A reminder is a token on a line (Thock writes it at the end), `(@YYYY-MM-DD HH:MM)`, `(@YYYY-MM-DD)`, or `(@HH:MM)` in a daily note (that note's day), followed by a `<!--rem:…-->` id Thock adds. Find them as the Week Review skill's step 6 does: read the index (`reminders.md`, unless `.thock/reminders.toml` names another), and through each link the line carrying the same id in the linked note, its **home**. Most of today's also show under `### Reminders` in today's note, but those lines are copies; the index is the full list.

1. Collect the open reminders due **today**: an unticked task or a sentence whose token is not struck. If there are none, skip this step silently.
2. Ask **one** question with the `ask` tool, naming them: *N reminders are still open from today. Move them to tomorrow at 09:00?*, with **Move to tomorrow 09:00**, **Let me pick** and **Leave them**. The hour in the question and the options is `tomorrow_at` (09:00 by default): `[ring] tomorrow_at` in `.thock/reminders.toml` names another.
3. On **Let me pick**, ask once per reminder, as the Week Review does: **Tomorrow 09:00**, **Next week** (Monday 09:00), **Pick a date**, **Close** and **Drop the reminder**, with `tomorrow_at` in place of 09:00 here too.
4. Write each answer on the home line only, found by its id, exactly as the Week Review's step 6 shows: rewrite the token in its full form to move it, tick the task (or strike the token in a sentence) to close it, delete the token and keep the words and the id to drop it. Never write or change an id, and never edit the index or the copies under `### Reminders`.

A reminder moved here, or closed in step 5, is no longer unfinished, so it drops out of step 7. A reminder you leave stays open and Thock keeps listing it, each morning, until it is closed, moved or dropped.

## 7. Offer unfinished tasks to the backlog

The vault keeps a holding pen — the configured backlog file, `backlog.md` by default — with the sections `## Soon`, `## Someday`, and `## Completed` (the Desk panel's Backlog tab in the right dock renders it). Unfinished tasks shouldn't die in today's note:

1. List the unfinished (`- [ ]`) tasks you found in the note. Leave out the lines under `### Reminders`: each is a copy of a reminder that lives in another note, and step 6 already asked about it.
2. Ask the user **one** question with the `ask` tool: move them to the backlog? The options are **All of them**, **None** (they stay only in the note) and **Let me pick**. If they pick, ask once per task with the options **Soon**, **Someday** and **Leave it**. Otherwise the destination is **Soon**. If there are no unfinished tasks, skip this step silently.
3. **Deduplicate before appending:** skip any chosen task whose text already appears in `backlog.md` — compare whitespace-insensitively and ignore any ` ✅ YYYY-MM-DD` completion suffix, any `(@…)` reminder token and any `<!--rem:…-->` id — in **any** section, `## Completed` included. Report skips as "already in backlog: …".
4. Append the remaining tasks as `- [ ] <task text>` at the end of the matching section. A task with a reminder keeps its token and its id, which keeps it the same reminder; only a `(@HH:MM)` token changes, to the full `(@YYYY-MM-DD HH:MM)` of the note's day, since the backlog has no day of its own. Create `backlog.md` or a missing section heading if needed — never rewrite, reorder, or delete anything already in the file.

   A task in today's note (`daily/2026-10-21.md`), and the line the backlog gets:

   ```markdown
   - [ ] Call the plumber (@14:30) <!--rem:7d3e55a1c0f2-->
   - [ ] Renew the passport (@2026-12-02) <!--rem:2f6b08e19a5c-->
   ```

   ```markdown
   - [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->
   - [ ] Renew the passport (@2026-12-02) <!--rem:2f6b08e19a5c-->
   ```

Never move a task without the user's answer. Apart from the confirmed checkbox flips in step 5 and the reminder answers in step 6, never edit the tasks inside the daily note — don't rewrite their wording, and don't touch anything else you already wrote.

## 8. Append the review

Append a `# Daily Closure` section at the end of today's note with the `append` tool, in one call, after everything already there. Never re-create the note and never paste the review into the chat in place of writing it:

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

When step 3 actually pulled something in, add one section for it above `## Suggestions`: `## Commits` for code (`- repo@abc123 · commit message`), `## Calendar` for meetings. **Omit the heading entirely when there is nothing to put under it**; never write `N/A` or "not configured" under it. An empty `## Commits` on a day with no code is noise, and on a vault that doesn't track code it's someone else's life. Under `## Open (carried forward)` copy each task line exactly as `- [ ] text`, one dash, not a dash in front of another dash, and without its `(@…)` reminder token and its `<!--rem:…-->` id: the closure is a record of the day, not a second reminder. The two tasks above are quoted as `- [ ] Call the plumber → backlog (Soon)` and `- [ ] Renew the passport → backlog (Soon)`.

Write it in the tone the profile names, and use the profile's area names when you group anything. Keep it short and factual. Base "carried forward" on the still-unchecked tasks (those you marked done in step 5 belong under `## Done`, not here) plus anything the recent-days scan shows lingering. Suffix each task that moved in step 7 with `→ backlog (Soon)` (or `(Someday)`) so the note records where it went; tasks that were skipped as duplicates get `→ already in backlog`.

## 9. Reflect

Read `skills/thock/reflect.md` at the vault root and run it in **daily** mode over today's note, which you have already read. It files whatever was noted in `memory/inbox.md` and anything durable today held into `memory/`, so the next session starts knowing it. Skip this step in silence if that file is missing.

## Output

After the appends have happened, tell the user in two or three sentences what the review says, that it is at the end of today's note, which tasks (if any) were marked done in place, and which moved to the backlog, plus Reflect's one line on what it learned when there was something. The review lives in the note; don't repeat it whole in the chat.
