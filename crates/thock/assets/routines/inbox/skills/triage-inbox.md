# Triage Inbox

Empty the vault's front door. Every file in `inbox/` is an untriaged item — that is the whole
data model. This ritual proposes a destination for each one and files **only what the user
confirms**: it appends tasks, appends to notes, or moves whole files — nothing else, and it
never rewrites what the user wrote.

**Reads:** `inbox/`, `routines/inbox/triage-policy.md`, `.thock/state/inbox/last-triage`,
`backlog.md`, today's daily note.
**Writes:** `backlog.md` (append), today's note (append), destination notes/folders,
`archives/inbox/triage-log.md` (append), `.thock/state/inbox/last-triage`, and — only after a
destination write succeeded and only for confirmed items — moving or deleting files in `inbox/`.

> **The confirm gate is a guarantee, not a setting.** Propose and wait, even at high
> confidence. There is no rule, threshold, or flag that skips step 4 — accepting the whole
> batch is one word, and that is as fast as it gets.

> **Paths are vault-configured.** `.thock/inbox.toml` may set `dir` to something other than
> `inbox`; `.thock/config.toml` sets the daily-note and backlog paths. The defaults are used
> below — follow the config silently when it differs.

## 1. Read the policy and the watermark

1. Read `routines/inbox/triage-policy.md`. It is the judgment you apply in step 3. If it's
   missing, use sensible defaults (links and ideas → Backlog · Someday; clearly-urgent items →
   Soon) and, once, nudge the user to run **Set Up Inbox**.
2. Read `.thock/state/inbox/last-triage` if it exists — one ISO timestamp, the end of the last
   run. It only sorts the presentation; dedup never depends on it.

## 2. List the queue

List `inbox/*.md`, newest first. Split into **New** (file's own `captured:` frontmatter — or
file mtime for hand-written notes — after the watermark) and **Carried over** (everything
else, including anything stamped `deferred:`). An empty folder means a short "inbox is empty"
and you're done — skip to step 6 only if you have nothing at all to write.

For each item read its frontmatter (`title`, `url`, `link`, `due`, `source`, `capture`) when
present; a hand-written file with no frontmatter is a valid item whose title is its first
heading or first line.

## 3. Propose

For each item choose **exactly one** destination from the policy, with a one-line reason.
A `due:` date is metadata for your judgment (soon vs someday), never an instruction. Present
the whole batch as one compact numbered list — **one screen, not a conversation per item**:

```
New
1. Ship it — a practical guide (link) → Backlog · Someday — article to read
2. Call the notary (due 2026-08-27)  → Backlog · Soon — dated and near

Carried over
3. Sketch: vault sync ideas          → notes/ideas/ (kept as note) — deferred twice
```

## 4. Wait

Do not touch anything before the user answers. Accept `all`, a selection (`1,3`), a
re-assignment (`2 → soon`, `3 → today`), `leave 4` (defer), `drop 6` (discard), or plain
prose. Anything not confirmed is left exactly where it is.

## 5. Apply the confirmed batch

Destination writes come **first**; an item's inbox file is removed only after its destination
write succeeded. A failed write leaves that item's file in place — report it, don't retry
silently.

| Destination | What happens |
| --- | --- |
| Backlog · Soon / Someday | Append `- [ ] <title>` under the heading (create it if missing); carry the item's `url` as a Markdown link on the task. Then **delete** the file. |
| Today's note, a named section | Append the line under that section, creating today's note from its template if missing. Then **delete** the file. |
| Append to an existing note | Append the item's content under a dated heading. Then **delete** the file. |
| Kept as a note | **Move** the file into the destination folder (rename to fit its naming), content preserved verbatim. |
| Left | Leave the file, stamp `deferred: <today>` in its frontmatter (add frontmatter if it has none), optionally with a one-line reason on the next line. |
| Discarded | **Delete** the file. |

A task line carries the item's **title**, not its whole body — the body either moved with the
file or was the URL. Deletion is deliberate: the content has been absorbed or rejected,
invisible history checkpoints the batch, and the log line below says where it went. An item
deferred three times is information, not a failure — say so once, quietly, and move on.

## 6. Log and stamp

Append one line per handled item (filed, moved, or discarded — not deferred) to
`archives/inbox/triage-log.md`, creating it if missing, newest at the bottom. The format is
exact — the trailing marker is machinery (it rebuilds capture state after a crash), so copy
the item's `capture:` digest into it verbatim, and omit the marker only when the item had no
digest (hand-written):

```markdown
- 2026-08-23 · Ship it — a practical guide to shipping → Backlog · Someday <!--inbox:4d1f9a02c7b3-->
```

Then write the current ISO timestamp to `.thock/state/inbox/last-triage` (overwrite; create
the directory if needed).

## Output

Report counts by destination, what was left (and why), and anything that failed. The measure
of success is simple: `inbox/` holds only what the user chose to leave.
