# Wrap Today

Close out **today's** daily note. Read what you planned and did, pull in whatever outside activity your profile allows, scan the last few days for multi-day context, sort the day's loose ends (finished tasks, today's reminders, tasks for the backlog) in one question, then append an AI review with suggestions to today's note. It never rewrites your prose; the only edits it makes to what you wrote are the answers you give in that one question: a checkbox you tick, a reminder you move.

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

## 5. Sort the day's loose ends

Everything still open from today gets its answer in **one** question: the tasks that may be finished, today's reminders, and the tasks that could go to the backlog. Ask it with the `sort` tool, which shows the person every item with its own row of choices and returns all the answers together. **If you have no `sort` tool, skip this step** and go on to step 6: nothing moves, and the closure lists the open tasks as they are.

### Build the sheet

**Tasks** are the unchecked (`- [ ]`) tasks in today's note, except the lines under `### Reminders` (each is a copy of a reminder that lives in another note) and tasks carrying a reminder due today (they go under Reminders). Before listing them, **deduplicate**: a task whose text already appears in `backlog.md` gets no row. Compare whitespace-insensitively and ignore any ` ✅ YYYY-MM-DD` completion suffix, any `(@…)` reminder token and any `<!--rem:…-->` id, in **any** section, `## Completed` included. Name those tasks in the sheet's footnote instead: *Already in your backlog: Fix the bike*.

**Reminders** are the open reminders due **today**. A reminder is a token on a line (Thock writes it at the end), `(@YYYY-MM-DD HH:MM)`, `(@YYYY-MM-DD)`, or `(@HH:MM)` in a daily note (that note's day), followed by a `<!--rem:…-->` id Thock adds. Find them as the Week Review skill's step 6 does: read the index (`reminders.md`, unless `.thock/reminders.toml` names another), and through each link the line carrying the same id in the linked note, its **home**. Most of today's also show under `### Reminders` in today's note, but those lines are copies; the index is the full list. A reminder is open when it is an unticked task or a sentence whose token is not struck.

The sheet has up to two groups, and a group with no items is left out:

| Group | Choices, in this order |
| --- | --- |
| **Tasks** | **Done**, **Soon**, **Someday**, **Leave** |
| **Reminders** | **Done**, **Tomorrow 09:00**, **Next week**, **Drop**, **Leave** |

The hour in **Tomorrow 09:00** is `tomorrow_at` (09:00 by default): `[ring] tomorrow_at` in `.thock/reminders.toml` names another, and the label carries that hour. **Next week** is Monday at the same hour.

Give each item a short id (`t1`, `t2`, `r1`), its text as the person wrote it without the token and the id, and pre-select a choice in `suggested` **only when you can say why**, putting the reason in its `note`:

- **Done** only when what step 3 pulled in, or the notes captured in step 2, show it was finished. A task is unfinished unless something shows otherwise, so a **Done** suggestion always has a note citing the evidence (`commit 3f1c2a "budget v1"`).
- **Soon** or **Someday** when you can say why: a due date, how long it has lingered in the recent days (`carried 2 days`), the triage policy the profile names.
- **Tomorrow 09:00** when the reminder is still relevant.
- Otherwise leave `suggested` out. The person picks, and the sheet can't be applied until every row has an answer.

The question names the day's work, for example *Where should today's loose ends go?* With no rows at all, skip this step silently.

### Apply the answers

Write each answer exactly as below. These are the **only** edits this skill makes to your own text; leave every task's wording and everything else untouched.

- **Done** on a task flips its box in place (`- [ ]` → `- [x]`) in today's note. On a reminder, tick its task or strike the token in its sentence, on its home line.
- **Soon** and **Someday** append the task as `- [ ] <task text>` at the end of the matching section of the backlog file (the Desk panel's Backlog tab in the right dock renders it). A task with a reminder keeps its token and its id, which keeps it the same reminder; only a `(@HH:MM)` token changes, to the full `(@YYYY-MM-DD HH:MM)` of the note's day, since the backlog has no day of its own. Create `backlog.md` or a missing section heading if needed — never rewrite, reorder, or delete anything already in the file.

   A task in today's note (`daily/2026-10-21.md`), and the line the backlog gets:

   ```markdown
   - [ ] Call the plumber (@14:30) <!--rem:7d3e55a1c0f2-->
   - [ ] Renew the passport (@2026-12-02) <!--rem:2f6b08e19a5c-->
   ```

   ```markdown
   - [ ] Call the plumber (@2026-10-21 14:30) <!--rem:7d3e55a1c0f2-->
   - [ ] Renew the passport (@2026-12-02) <!--rem:2f6b08e19a5c-->
   ```

- **Tomorrow 09:00** and **Next week** rewrite the reminder's token, in its full form, on its home line only, found by its id, exactly as the Week Review's step 6 shows. **Drop** deletes the token and keeps the words and the id.
- **Leave** changes nothing: the task stays only in the note, and a reminder stays open, listed each morning until it is closed, moved or dropped. A reminder that needs a specific date is left here and edited in its note.

Never move a task without the person's answer, never write or change a `<!--rem:…-->` id, and never edit the index or the copies under `### Reminders`. If the sheet comes back unanswered, nothing moves: say so in the output, and write the closure with every task still open.

## 6. Append the review

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

Write it in the tone the profile names, and use the profile's area names when you group anything. Keep it short and factual. Base "carried forward" on the still-unchecked tasks (those answered **Done** in step 5 belong under `## Done`, not here) plus anything the recent-days scan shows lingering. Suffix each task that moved in step 5 with `→ backlog (Soon)` (or `(Someday)`) so the note records where it went; tasks the footnote named as already in the backlog get `→ already in backlog`.

## 7. Reflect

Read `skills/thock/reflect.md` at the vault root and run it in **daily** mode over today's note, which you have already read. It files whatever was noted in `memory/inbox.md` and anything durable today held into `memory/`, so the next session starts knowing it. Skip this step in silence if that file is missing.

## Output

After the appends have happened, tell the user in two or three sentences what the review says, that it is at the end of today's note, which tasks (if any) were marked done in place, and which moved to the backlog, plus Reflect's one line on what it learned when there was something. The review lives in the note; don't repeat it whole in the chat.
