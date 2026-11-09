# Inbox

The vault's **front door**. Away from the keyboard, a thought either survives until you sit
down or it doesn't — the Inbox fixes that with one rule:

> **Capture is dumb, instant, and thumb-sized; triage is deliberate, assisted, and at the desk.**
> Nothing decides anything on the phone.

## The landing zone

`inbox/` is a plain folder, and **a file in it is an untriaged item** — that is the entire data
model. There is no index, no database, no "unread" flag. Anything can write a note into it:

- **Share to Google Tasks** on your phone — the task (title, link, notes, due date) becomes an
  inbox note within a few minutes.
- **Label an email `thock/inbox`** in Gmail — the email's text lands as a note with a link back
  to the thread.
- **Drag a file in from Finder**, or just write one by hand. Frontmatter is optional — the
  first heading or line is the title.

The two Google transports are read-only: Thock never completes a task, never removes a label,
never touches your phone-side lists. Completing a task on the phone *before* Thock polls means
"never mind" — a genuinely useful gesture.

## The Inbox view

The fast path. Open the **Inbox** (`ctrl-alt-i`, the *Inbox* link in this Routine, or **Open** on
the Inbox row in the sync icon) and the queue takes the centre pane: what is waiting on the left,
in *New* and *Carried over*, the selected note rendered on the right, pictures included. One key
per decision along the bottom: `t` for today's Day planner, `s` for Soon, `shift-s` for Someday,
`e` to archive, `o` to open the note, `u` to take the last decision back. `x` marks rows so one
key files them all, `/` filters, `tab` moves into the note to scroll it, `?` shows every key.

Each decision leaves exactly what the phone's swipe leaves: a task line, the note moved into
`archives/inbox/`, and a line in the triage log. With Thock Plus, every row carries a **suggested
destination** with its confidence, and `space` takes it; a suggestion only ever pre-selects a key,
nothing is filed without a keystroke. `shift-r` hands what is left to the ritual below.

## Triage

For the decisions that need the vault's folders and some judgement, run **Triage Inbox**
(`/triage-inbox`, or `shift-r` from the Inbox view). It reads your policy
(`routines/inbox/triage-policy.md`), shows one line per item with a
proposed destination and a one-line reason, and waits. You confirm, re-assign, defer, or drop —
**nothing is filed without your say-so**, and accepting the whole batch is one word.

Destinations are ordinary file operations: a task appended to the Backlog, a line appended to
today's note, the note itself moved into a folder, or an append to a reading list. Once an item
is filed, its inbox file is deleted — the content has been absorbed, `inbox/` is a worklist
that must empty to work, and invisible history checkpoints every batch so nothing is actually
unrecoverable. Every handled item gets a line in `archives/inbox/triage-log.md`, so the vault
can always answer "what happened to that thing I sent myself" — even with Thock closed.

## Auto-archive

Some of the Inbox needs nothing from you: notifications, tickets filed against the wrong team,
things another group owns. Run `thock: turn on inbox auto archive` and, once a day after the first
Gmail sync, Thock asks about each item that arrived since the last run and archives the ones it
is sure about (0.95 or more), exactly as `e` would, in Gmail too when Gmail archive is on. A
thread you wrote in is never touched. What it filed waits in a collapsed **Auto-archived** group
at the bottom of the Inbox list until the next run: `u` sends an item back, `t`, `s` and
`shift-s` re-file it, and either way Gmail puts the thread back.

Turning it on first runs **Refresh Inbox Policy**, a weekly ritual that reads your notes and your
past decisions (corrections first), keeps a map of your work in `memory/work.md`, and rewrites the
policy's Proposals and Criteria so a miscategorised ticket is judged by what it's about. The run
never works from the shipped defaults: until the refresh has stamped the policy, the group says
it is waiting. `[inbox] auto_archive_threshold` in `.thock/config.toml` sets the bar (never below
0.85), `[inbox] names` lists how ticket systems spell you, and
`thock: turn off inbox auto archive` stops it.

## Make it yours

- `routines/inbox/triage-policy.md` — the judgment triage follows. **Set Up Inbox** writes it
  from six questions; editing the file by hand changes the ritual with no re-setup. Its
  `## Criteria` says what each of the suggestion's five answers means.
- `.thock/inbox.toml` — the landing folder, poll interval, and the Google Tasks list. The
  Gmail label lives in `.thock/gmail.toml`'s label → folder map instead. Delete either file and
  that transport goes away; `inbox/` keeps working as a plain folder.
- **Archive in Gmail** — run `thock: enable gmail archive` and an email you file here also
  leaves your Gmail inbox, so you triage it once. Google asks for the permission first; until
  then Thock only reads your mail. `[archive] on` in `.thock/gmail.toml` lists which decisions
  count (`today`, `soon`, `someday`, `archive`, `discard`, `other`; just `archive` to start).
  Only the email's inbox label changes, a minute after you decide, and undoing the decision puts
  it back. Decisions made before you turned it on stay where they are in Gmail.
  `thock: disable gmail archive` stops it.
