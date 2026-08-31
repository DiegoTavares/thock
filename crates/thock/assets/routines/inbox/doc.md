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

## Triage

At the desk, run **Triage Inbox** (`/triage-inbox`, or press enter on the Backlog panel's Inbox
row). It reads your policy (`routines/inbox/triage-policy.md`), shows one line per item with a
proposed destination and a one-line reason, and waits. You confirm, re-assign, defer, or drop —
**nothing is filed without your say-so**, and accepting the whole batch is one word.

Destinations are ordinary file operations: a task appended to the Backlog, a line appended to
today's note, the note itself moved into a folder, or an append to a reading list. Once an item
is filed, its inbox file is deleted — the content has been absorbed, `inbox/` is a worklist
that must empty to work, and invisible history checkpoints every batch so nothing is actually
unrecoverable. Every handled item gets a line in `archives/inbox/triage-log.md`, so the vault
can always answer "what happened to that thing I sent myself" — even with Thock closed.

## Make it yours

- `routines/inbox/triage-policy.md` — the judgment triage follows. **Set Up Inbox** writes it
  from six questions; editing the file by hand changes the ritual with no re-setup.
- `.thock/inbox.toml` — the landing folder, poll interval, and the Google Tasks list. The
  Gmail label lives in `.thock/gmail.toml`'s label → folder map instead. Delete either file and
  that transport goes away; `inbox/` keeps working as a plain folder.
