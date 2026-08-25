# Connect Google Workspace

Link a Google account to this vault with one sign-in that powers three rituals: today's accepted meetings appear inside today's daily note (as ordinary Markdown checklist lines under the Day planner section), any email the user labels **`thock/backlog`** in Gmail becomes a task in the Backlog's Someday column, and anything flung at Google Tasks from a phone — or labeled **`thock/inbox`** in Gmail — lands as a note in the vault's `inbox/` folder for the Triage Inbox ritual. Thock itself does the syncing; this skill's job is to get the connection made and explain what the user will see.

**Reads:** Google Calendar, Gmail, and Google Tasks — all read-only.
**Writes:** `.thock/google.toml` (the account), `.thock/calendar.toml` / `.thock/gmail.toml` / `.thock/inbox.toml` (per-feature preferences), the `Calendar` subsection of today's daily note, `backlog.md` (Someday, append), `archives/emails/` (only when full import is on), `inbox/` (captured items).

> **Do not attempt OAuth yourself.** The sign-in runs inside Thock (system browser + system keychain); no token is ever written to the vault, and there is nothing for you to fetch or store. Your role is to start the flow and configure preferences.

## 1. Start the connection

1. Ask the user to run **`thock: connect google workspace`** from the command palette (or click **Connect Google Workspace** at the top of the Day Planner or Backlog panel).
2. Their browser opens a Google sign-in. One consent screen grants read-only access to Calendar, Gmail, and Tasks together. Afterwards Thock shows a calendar picker: `enter` toggles a calendar, `escape` saves the choice. The primary calendar starts selected.
3. Next, Thock asks how captured emails should land in the Backlog: **Link to Gmail** (the task links back to the thread) or **Archive into the vault** (the email's text is saved under `archives/emails/` and linked from the task). `enter` chooses; the choice can be changed any time with **`thock: choose email import mode`**.
4. For the Gmail gestures there is one more human step: **create the labels in Gmail** — `thock/backlog` (email → Backlog task, directly) and `thock/inbox` (email → inbox note, for triage). Gmail nests them under one `thock` parent. Until a label exists, the Backlog panel's status row says so and nothing else happens. A vault upgraded from the old flat `backlog` label keeps working — the row shows a rename hint until the label moves to `thock/backlog`.
5. Google Tasks needs no setup at all: sharing a link (or typing a thought) into the default **My Tasks** list on a phone is enough. Thock polls it read-only and never completes or deletes a task — completing it on the phone *before* Thock sees it simply means "never mind".
6. That's it — the account lands in `.thock/google.toml`; the sign-in itself lives in the system keychain.

## 2. Explain the calendar ritual (only if asked)

- Every few minutes Thock pulls today's events and maintains a `## Calendar` subsection inside the Day planner section of today's note. Each meeting is a normal checklist line like:

  ```
  - [ ] 10:00 - 10:30 API Leads meeting <!--gcal:9f2c1ab4e7d0-->
  ```

  The trailing HTML comment is the meeting's identity — invisible in rendered Markdown, and what lets a moved meeting keep its checkbox. Tell the user to leave it in place; everything else on the line is theirs.
- Ticking a meeting off, adding sub-bullets, or rewriting its title is always safe. A renamed line becomes the user's — Thock stops correcting it entirely.
- A cancelled meeting is struck through and marked `(cancelled)`, never deleted. Sync is read-only toward Google: editing the note never changes the calendar.
- Sync waits for the user to create the daily note and for the Day planner heading to exist — it never creates either.

## 3. Explain the email ritual (only if asked)

- The gesture lives in Gmail: label an email `thock/backlog`, and within a few minutes the thread appears once as an unchecked task at the end of the Backlog's **Someday** section, carrying its own invisible `<!--gmail:…-->` identity comment. Label it `thock/inbox` instead when it matters but isn't yet a task — it becomes an inbox note for triage. An email carrying both labels takes the backlog lane, once.
- With the default `import = "title"`, the task links back to the thread in Gmail. With `import = "full"`, the email's text is archived as a plain Markdown note under `archives/emails/` and the task carries an Obsidian-style `[[wikilink]]` to it (the link is inert text for now — navigation comes later).
- Capture is one-way and one-time: Thock never modifies labels, never marks mail read, and never touches a captured task again. Completing, editing, moving, or deleting the task is entirely the user's business; removing the label after capture changes nothing.
- Re-labeling an already-captured thread does not duplicate it, ever.

## 4. Explain the inbox ritual (only if asked)

The Inbox Routine's own doc (`routines/inbox/Inbox.md`) covers it: capture is dumb and thumb-sized (Google Tasks, `thock/inbox`, or just dropping a file into `inbox/`), and the **Triage Inbox** ritual files everything later, at the desk, with the user confirming every move.

## 5. Adjust preferences (on request)

All the config files are plain TOML the user (or you) can edit:

- `.thock/google.toml` — the connected `account`, plus an optional `[google]` OAuth client override. Shared by every Google feature.
- `.thock/calendar.toml` — `calendars = [...]` (the picker `thock: choose calendars` edits this without re-authenticating), `section = "Calendar"`, `[filters]` (`accepted_only`, `include_solo`, `all_day`, `private_busy`), `poll_seconds` (60–3600).
- `.thock/gmail.toml` — `label = "thock/backlog"` (which Gmail label captures to the Backlog), `import = "title" | "full"`, `archive_dir = "archives/emails"`, `poll_seconds`.
- `.thock/inbox.toml` — `dir = "inbox"` (the landing zone), `poll_seconds`, `[gmail] label = "thock/inbox"` / `enabled`, `[tasks] list` (defaults to the account's default list) / `enabled`.

Deleting `.thock/gmail.toml` or `.thock/inbox.toml` turns that capture path off while leaving the rest connected. To stop everything, run **`thock: disconnect google workspace`** — it forgets the sign-in and leaves every note, task, and archive untouched.
