# Thock V56 - An email reads as a conversation anywhere, keeps up with its thread, and can be answered

**Status:** Shipped (2026-11-11): merged as #199, by-hand checks passed
**Owner:** Diego · **Date:** 2026-11-10
**Companion docs:** `v41-inbox-desk-view.md` §4.4 and §5 (the conversation reader this lifts out of
the Inbox, and the keys it shares), `v15-unified-gmail-sync.md` §4.4 and §7 (capture, the state
file, the transport), `v53-gmail-archive-on-triage.md` §4.3 and §7.5 (the `gmail.modify` consent
this rides on), `v13-inbox-routine.md` §6 and §9.5 (the note format and the triage log),
`v54-anchor-and-tides.md` (the page's measure)

---

## 1. Summary

V41 rev 3 taught the Inbox to read an email as a conversation: one card per message, the newest
open, repeats and signatures folded. That reading stops at the Inbox's edge. Once a note is
triaged, or opened from anywhere else, it is raw Markdown again: 88 lines of a resent ticket to
find one new sentence.

V56 does three things, in one change because each needs the one before it:

1. **The reader is its own tab.** Opening a note with `source: gmail` from anywhere (the file
   finder, the project panel, a wikilink, the Inbox's `o`) shows the conversation. `o` flips the
   tab to the Markdown source and back.
2. **A note keeps up with its thread.** Gmail sync appends messages that arrive after a thread was
   captured, where today it skips every thread it has seen. A reply on a triaged note brings it
   back to the Inbox, unless it was archived.
3. **Reply from Thock.** `r` answers the author of the current card, `a` answers everyone, and
   `alt-r` picks one address of the thread. The reply is sent through Gmail in the same thread,
   after a second keystroke on a line that names every recipient.

## 2. Goals & success criteria

- **G1: One reading.** The Inbox's right pane and the standalone tab are the same component, so
  every rule of V41 §4.4 holds in both and a fix lands in both.
- **G2: Reader first, source one key away.** An email note never opens as raw Markdown unless the
  person asked for the source. The file is unchanged by being read.
- **G3: The note is the whole conversation.** A message that reaches a captured thread is in its
  note within a poll, as a section in the shape capture writes. Thock's own replies arrive the same
  way, so there is one writer of sections.
- **G4: Nothing is appended twice and nothing the person wrote is touched.** A crash between the
  note and the state file re-plans into a no-op.
- **G5: A reply only goes to addresses already on the thread**, only after a confirming keystroke
  that shows them, and only when the grant allows it.
- **G6: Keyboard-complete.** Every step, from opening the note to sending, has a named `thock::`
  action and a binding.

## 3. Non-goals

- **Composing a new email, forwarding, attachments, or adding a recipient who isn't on the thread.**
  G5 is the safety property; a new address is Gmail's job (`b` opens the thread there).
- **Gmail drafts.** Thock sends or it doesn't (decision 2). An unsent reply lives in the tab and is
  gone when the tab closes.
- **Rich text.** A reply is `text/plain`, with the answered message quoted under it.
- **Reopening a reader tab after a restart.** Like the Inbox tab (V41 §3). The note is one open away.
- **A draft written by the agent.** The composer is an editor; nothing fills it.
- **The phone.** It shows the note as Markdown, including appended sections. Replying there is a
  later question.
- **Landing on a search match inside an email note.** Reader first means a project-search hit opens
  the conversation; `o` shows the source.

## 4. The reader as a component

### 4.1 `EmailReader`

`crates/thock/src/email_reader.rs` holds an `EmailReader` entity: what V41 §4.4 describes and
nothing of the Inbox. It owns the reader's focus handle, its scroll, the parsed thread, the current
message, the folds, the earlier cue and the composer (§7). It renders the scrolling body; its host
renders whatever sits above (the Inbox its title block and destination bar, the tab its header).

The host tells it three things: the note (`set_note(key, InboxItem)`, where a changed `key` means
re-parse), the key context its hints are read from, and whether its keys are live. It tells the
host nothing; the host forwards the reader's actions to it (`next_message`, `toggle_folds`,
`scroll`, `reply`), because in the Inbox focus stays in the list.

A note that isn't an email still renders as one Markdown body, as today.

`InboxView` keeps the list, the bar, marks, filing and the keys sheet. Nothing a person sees in the
Inbox changes except §8's keys.

### 4.2 `EmailNoteView`, the tab

A `workspace::Item` in `crates/thock/src/email_note_view.rs`: the note's title and envelope icon on
the tab, a header row, the title block and an `EmailReader`.

```
 Jordan Reyes · jordan@studio.example        [o] Edit  [r] Reply  [a] Reply all  [b] Open in Gmail ↗
 ──────────────────────────────────────────────────────────────────────────────────────────────
 HD 42: Backups not restarted when the farm reboots | #42

  (AR) Ana Reyes   "The job is stuck since…"                       Oct 11, 10:48   1 of 3  ›
  (JR) Jordan Reyes  "can you check the…"                          Oct 11, 11:02   2 of 3  ›
 ┌───────────────────────────────────────────────────────────────────────────────────────────┐
 │ (AR) Ana Reyes  via helpdesk@studio.example   ⇧n previous      Oct 11, 11:30   3 of 3  ⌄ │
 │ Restarted the job, it's green now.                                                        │
 │ Status  CLOSED · was OPEN                                                                 │
 │ [z] Show quoted replies and ticket details                                                │
 └───────────────────────────────────────────────────────────────────────────────────────────┘
```

It holds the note's buffer, so it follows the file: a section appended by sync (§6), or an edit
saved from another editor, re-parses and keeps the current message unless a newer one arrived, in
which case it opens on that. It keeps the page's measure (V54) rather than filling the centre: it
is a note.

Key context `ThockEmailView` plus `menu`. Focus is the reader's, so the scroll keys always answer.

### 4.3 Which notes

A Markdown file inside the vault whose frontmatter says `source: gmail`. Where it sits doesn't
matter: `inbox/`, `archives/inbox/`, a project folder.

### 4.4 Routing an open

The editor still opens the file; Thock then puts the reader in its place. When an `Editor` over a
single buffer is added to a pane and that buffer is an email note, the view is added at the same
position and the editor's tab is closed, unless

- the source was asked for (`o` in the reader, `thock::EditEmailNote`), or
- the buffer has unsaved changes, or
- the file isn't in the project.

So every way of opening a note is covered without each one knowing about email. An editor
already open on the note is left alone: opening the note
again activates it, which is what *I'm editing this* means.

`thock::ReadEmailNote` (`o` has no room in an editor; `ctrl-alt-shift-m`, beside
`thock::ToggleMarkdownSource` on `ctrl-alt-m`) swaps an editor on an email note back to the
reader, saving unsaved changes first since the reader can't. It does nothing on any other note.

A pane holds one tab per file, so the swap removes the editor before it adds the reader, and
opening the note again finds whichever of the two is there.

Nothing outside `crates/thock/` changes for this.

## 5. Keys

One table for both hosts. In the Inbox these join V41 §5; `r` there moves (§8).

| Key | Action | Does |
| --- | --- | --- |
| `j` `k` `ctrl-d` `ctrl-u` | `thock::ScrollInboxReaderDown` … | Scroll, as in the Inbox's reader |
| `n` `shift-n` | `thock::NextInboxMessage`, `thock::PreviousInboxMessage` | Next or previous card |
| `z` | `thock::ToggleInboxFolds` | Opens or closes every fold and card |
| `b` | `thock::OpenInboxLink` | The thread in Gmail |
| `o` | `thock::EditEmailNote` | The tab: the Markdown source in its place. (The Inbox keeps `thock::OpenInboxItem`, which now opens the reader tab.) |
| `ctrl-alt-shift-m` | `thock::ReadEmailNote` | From an editor on an email note: the reader in its place |
| `r` | `thock::ReplyToEmail` | Reply to the author of the current card |
| `a` | `thock::ReplyAllToEmail` | Reply to everyone on the current card's message |
| `alt-r` | `thock::ReplyToEmailRecipient` | A menu of the thread's addresses; the chosen one is the only recipient. Also from inside the composer |
| `cmd-enter` / `ctrl-enter` | `thock::SendEmailReply` | In the composer: once to check, again to send (§7.4) |
| `escape` | `menu::Cancel` | In the composer: back to the reader, the text kept |

The existing reader actions keep their names; renaming them would break keymaps for a word.

Bindings go in the Thock blocks of both default keymaps under `ThockEmailView && !composing`,
`ThockInboxView && !filtering && !composing` and `ThockEmailReply > Editor`. The reader's letters
are bound for everyone, as in the Inbox's reader, so `vim.json` gains no block; its Inbox block
takes `!composing` like the others. The Inbox's `enter` in an editor is narrowed to
`ThockInboxView && filtering > Editor`, and `ThockEmailReply > Editor` binds `enter` to
`editor::Newline`: both hosts bind `enter` to `menu::Confirm` above the composer, which would
otherwise open the selected note.

## 6. A note keeps up with its thread

### 6.1 What changes

Capture skips every thread whose digest the vault knows (V15 §4.4). That stays: a thread is
captured once. A second pass in the same poll, **refresh**, looks at the threads already captured.

### 6.2 Noticing

`threads.list` by mapped label returns each thread with its `historyId`, which Gmail moves on any
change to the thread. One request per hundred threads per mapping per poll. A thread is fetched
(`threads.get`) only when its `historyId` differs from the one recorded, or none is recorded. A
poll fetches at most `REFRESH_BUDGET` (10) threads, so the first poll after the upgrade doesn't
fetch a whole mailbox; the rest wait their turn.

### 6.3 State: `.thock/state/gmail/threads.jsonl`

The last record for a thread wins, and a write keeps only that one, so the file grows with the
threads captured and not with the polls:

```json
{"thread":"18c2f…","history":"918273","through":1762790000000,"at":"2026-11-10T09:12:00-03:00"}
```

`through` is the newest message the note holds, as Gmail's `internalDate` in milliseconds.
Capture writes a thread's first record from the thread as it was fetched (its `historyId` and its
newest message), so a reply that arrives while a capture runs is neither missed nor written
twice. A thread with no record (captured before this, or one Gmail gave no history for) takes
`through` from when `imported.jsonl` says it was captured; a record there with no thread or no
time (written before V15) is never refreshed.

A fetch with nothing newer than `through` still writes a record, so a label change isn't fetched
again. A record is written only once the note's file holds the messages: a note open with unsaved
words takes the append in its buffer and is asked about again each poll until it is saved.

### 6.4 The write

Messages newer than `through`, drafts left out, oldest first, each in the section capture writes
(`## <from> — <YYYY-MM-DD HH:MM>`, then the text):

- **Where.** The note is found by its `capture:` digest: in the mapped folders, the inbox folder
  and `archives/inbox/` first; else by a walk of the vault's Markdown, which only happens with
  something to append. A note that is gone stays gone; the record is still written.
- **Append.** The sections go at the end of the file, after one blank line. Through the open
  buffer as one transaction when the note is open and the person isn't typing (the same guard the
  backlog line uses), else read-modify-write through the project `Fs`.
- **A lone message gets its heading.** Capture writes a single message bare (V13 §6). Before the
  first append, `## <from> — <date>` for that first message is inserted above the body, under the
  title, so the reader sees two messages rather than a note and a reply. This is the one insert;
  nothing is rewritten.
- **Never twice.** A section the note already contains, byte for byte, isn't appended. That is
  what makes a crash between the note and the state file harmless. Mail's CRLF line endings are
  made plain newlines first: an open buffer never holds CRLF, so a section that kept them would
  never be found there and would be appended again.
- **One note's trouble is its own.** A note that can't be read or written is logged and tried
  again next poll; the other threads are still applied. A refresh that failed is the poll's
  outcome in the sync status, after the Gmail archive pass has run: nothing here holds V53 up.
- **Linked folders aren't walked**, since one may lead back up the vault.

The sender's text is untrusted (V15): it goes through the sanitising capture uses.

### 6.5 Back to the Inbox

When a refresh appends a message someone else wrote (Gmail's `SENT` label isn't on it) and the
note sits in `archives/inbox/`, the note moves back to the inbox folder, unless the last decision
the triage log holds for it is **Archive** or **Discard**. An archived note is appended in place.

| The note is | Its last decision | A reply from someone else |
| --- | --- | --- |
| in `inbox/` | none | appended |
| in `archives/inbox/` | Today, Soon, Someday, or kept as a note | appended, moved back to `inbox/` |
| in `archives/inbox/` | Archive, Discard | appended in place |
| anywhere else | any | appended in place |

Your own reply never moves a note. The move never overwrites: if `inbox/` already has that name the
note stays where it is. The task line an earlier decision wrote stays; its link still resolves.

### 6.6 Transport

`MailTransport` gains `refresh(known, budget)`, returning per changed thread its new `historyId`,
the first message's sender and time, and the messages past `through`. The default is *nothing
changed*, so a transport that can't tell loses nothing.

## 7. Reply

### 7.1 Permission

Sending needs a write to Gmail. It rides on V53's `gmail.modify` consent (decision 3): a vault that
allowed Gmail archive can reply with no new prompt. That widens what the grant is used for, so
V53 §2's *nothing else* is amended to say so, and the consent step's own text names both.

Permission and the archive setting stay separate (V53 §4.3). A vault without the grant sees, in
place of the composer, *Replying needs permission to send from your Gmail* and a button for
`thock::AllowGmailReplies`, which runs the incremental consent and leaves `[archive]` alone.

### 7.2 Who it goes to

The thread is asked for when a reply starts (`threads.get`), which gives each message's `From`,
`To`, `Cc`, `Reply-To`, `Message-ID` and `References`, and the text the reply quotes. The card being read is
matched to its message by position, and by sender and minute when the counts differ; failing that
the newest.

- `r`: the message's `Reply-To`, else its `From`. If that is the account itself (replying to your
  own message), its `To`.
- `a`: the same in **To**, and the message's other `To` and `Cc` in **Cc**, the account's own
  address left out.
- `alt-r`: every address the thread's headers name, the account's left out, as a menu; the one
  picked is **To**.

`Reply-To` may name several addresses; all of them are who the author asked to be answered. One
that names nobody usable leaves the author.

A recipient is always an address read from the thread. The composer has no field to type one in.
Addresses are read from the headers as they were sent, and only the names are decoded after, so
an encoded name can't put an address of its own on the thread.

A ticketing system's card is headed by the person its text names (V41 §4.4), but a reply goes to
the address that sent it. The composer's **To** line says the address, so the two can't be confused.

### 7.3 The composer

Under the last card, on the cards' ground: **To** and **Cc** as read-only lines, an auto-height
editor in the reader's type, and one line of hints. Key context `ThockEmailReply`; while it has
focus the reader's context carries `composing`, so `r`, `n` and `z` are letters again.

`escape` returns to the reader and keeps the text; `r` or `a` again reopens it with what was
written, switching recipients. The text is kept per note while the tab or the Inbox view lives.
`escape` on an empty reply closes the composer.

A note without a `link:` to its thread has nothing to reply into, and `r` does nothing there.

### 7.4 Sending

`cmd-enter` doesn't send. It replaces the hints with what is about to happen:

```
 Send to ana@studio.example, cc helpdesk@studio.example, as diego@example.com?   [⌘↵] Send   [esc] Keep writing
```

A second `cmd-enter` sends. Any edit, or a change of recipients, goes back to the first step. The
line and the **To** and **Cc** lines wrap rather than cut, so every address is on screen.

The message is `text/plain; charset=utf-8`: the reply, a blank line, `On <date>, <from> wrote:` and
the answered message quoted with `> `. Headers: `To`, `Cc`, `Subject` (the thread's, with one
`Re: `), `In-Reply-To` and `References` from the answered message, and the request names the
thread. Every header value has line breaks removed before it is written, non-ASCII is RFC 2047
encoded, and `To`, `Cc` and `References` are folded to stay inside the line length.

While it sends the composer is read-only and says *Sending…*. On success it closes, the reader
says *Reply sent. It joins the note at the next sync.* where the composer was, and a sync runs, so
the reply joins the note by §6 like any other message. On failure the composer stays with its text
and the error; when the permission is what was missing, *Allow replies* is offered there.

A reply being sent is not taken back by selecting another note or closing the tab. Back on its
note before Gmail answers, `r` shows it still sending. If it failed while another note was
showing, the note says *Your reply here wasn't sent* and `r` reopens it with its words and the
error. When the tab was closed there is nowhere to say so: the failure is logged.

If the thread can't be read from Gmail the composer says so and keeps what is typed; `r` or `a`
asks again.

### 7.5 Transport

`MailTransport` gains `thread_headers(thread_id)` and `send_reply(OutgoingReply)`. `send_reply`
refuses with `MissingPermission` when the grant is known to lack `gmail.modify`, without sending.

## 8. The Inbox

- The right pane is an `EmailReader`.
- `r` is `thock::ReplyToEmail` and `a` is `thock::ReplyAllToEmail`. On a note that isn't an email
  they do nothing. `thock::SuggestInboxDestinations` moves from `r` to `shift-j` (decision 5).
- The reader's header offers **Reply** and **Reply all** with their keys, beside *Open note* and
  *Open in Gmail*, on an email that has its thread; a tight header keeps only **Reply**.
- `o` and `enter` open the note as before; for an email that is now the reader tab (§4.4).
- The keys sheet and the row menu gain the reply keys.

## 9. Product invariants

- **Your files.** The note is still plain Markdown and holds the whole thread; the reader is a
  reading. State is under `.thock/state/gmail/`, which a vault can lose and rebuild.
- **Augmentation.** Refresh appends sections and inserts one heading. It never rewrites.
- **Human-in-the-loop.** Sending is the only thing here that leaves the vault and can't be taken
  back. It takes two deliberate keystrokes, shows every recipient, and can't address anyone the
  thread didn't already name. Email text never triggers a send.
- **Modular.** Without Gmail connected, an email note still reads as a conversation; reply says
  what it needs.

## 10. Tests

- `inbox_email` and the Inbox's reader tests keep passing against the extracted component.
- `email_note_view`: an email note opened by path lands as the reader; `o` gives the editor and
  `ctrl-alt-shift-m` the reader back; a non-email note and a dirty buffer stay editors; `n`, `z`, `j`
  move what they should (keystrokes); an append to the buffer shows as a new card.
- `gmail` (pure): the refresh plan for a bare note, a sectioned note, a section already present;
  where a note goes by its last decision; recipients for reply, reply all and one address; the
  MIME text, with a header carrying a line break and a non-ASCII subject.
- `gmail_google`: `threads.list` paging, the fetch budget, `SENT` marking, the send request's body
  and thread, a refused send without the permission.
- `gmail_service` (stub transport): a reply lands once across two polls and a crash between the
  note and the state; a triaged note returns to the inbox and an archived one doesn't; an open
  buffer takes the append.
- Composer (keystrokes): `r` opens it with the author, `a` with everyone, one `cmd-enter` sends
  nothing, the second sends exactly what the line showed, an edit in between disarms it.
- Composer, in both hosts: `enter` is a new line; a send outlives its tab and a change of note; a
  failed thread read is retried; a moved note keeps its reply.
- Keymaps: the new bindings exist on both platforms.

## 11. Changes by file

| File | Change |
| --- | --- |
| `crates/thock/src/email_reader.rs` | New. The reader lifted from `inbox_view.rs`, and the composer |
| `crates/thock/src/email_note_view.rs` | New. The tab, and the routing of §4.4 |
| `crates/thock/src/inbox_email.rs` | `is_message_heading`, for the refresh plan |
| `crates/thock/src/inbox_view.rs` | Hosts `EmailReader`; reply keys; `r` → `shift-j` |
| `crates/thock/src/gmail.rs` | Refresh plan, recipients, MIME, the transport's three new calls |
| `crates/thock/src/gmail_google.rs` | `threads.list`, metadata fetch, `messages.send` |
| `crates/thock/src/gmail_service.rs` | The refresh pass, `threads.jsonl`, reply, `AllowGmailReplies` |
| `assets/keymaps/*.json` | The bindings of §5 and §8 |
| `thock/specs/v41-…`, `v53-…`, `thock/VISION.md` | The amendments named above; the roadmap entry |

## 12. Decisions (2026-11-10)

1. **Reader first.** An email note opens as the conversation; the source is one key away. Not the
   editor with a toggle, and not a split.
2. **Send only, after a confirm.** No Gmail drafts.
3. **Ride on `gmail.modify`.** No separate send consent. V53's promise is amended rather than kept
   by a second scope.
4. **Sync learns replies**, and Thock's own reply reaches the note that way.
5. **`r` / `a` as in Gmail**, in both hosts; the Inbox's re-suggest moves to `shift-j`.
6. **A reply returns a triaged note to the Inbox, except an archived one**, which is appended in
   place. *Archived* is read as the decision the triage log holds (Archive or Discard), since every
   decision moves the file to `archives/inbox/`.
7. **One spec, one pull request.**

## 13. Open questions

- **Resolved: a second decision doesn't archive in Gmail again.** A thread Thock archived in
  Gmail (V53) comes back to the Gmail inbox on a reply by Gmail's own rule. When its note returns
  here and is decided again, V53's reconciler holds to G7 (*once Thock has archived a thread it
  never archives it again*), so the thread stays in the Gmail inbox until it is archived there.
  Marking it un-archived on the way back would archive it on the old log line, with no new
  decision, so this is left as it is. Worth a look once it has been lived with.
