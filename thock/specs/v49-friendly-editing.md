# Thock V49 - Tick a box with a click, format with a slash

**Status:** Draft (2026-10-28).
**Owner:** Diego · **Date:** 2026-10-28
**Companion docs:** `v10-markdown-conceal.md`, `v24-inline-markdown-conceal.md` (conceal and the reveal rule),
`v43-almanac.md` §7.3 (the task checkbox and bullet), `crates/thock/src/wikilink_completion.rs`
(the `[[`, `![` and `(@` completions)

---

## 1. Summary

The desk draws a task's `- [ ] ` as a checkbox, but the box is only a picture. To tick it a user
has to put the cursor on the line, which reveals the raw `- [ ]`, then delete the space and type
`x`. A vim user has `r x`; everyone else finds it a chore, and it is the main complaint from the
first non-programmer using Thock daily.

Revealing the line also moves its text. Six characters of source replace one 13px box, so the
item's words jump right the moment the cursor lands on them, and jump back when it leaves.

The wider problem is the same: Markdown is a syntax people have to know. Someone who has never
written `## ` or `> ` can't find headings or quotes in Thock at all.

V49 does three things:

1. **The checkbox is a control.** A click ticks it and leaves the cursor where it was; `cmd-enter`
   ticks the tasks under the cursors.
2. **List prefixes stay drawn while you type.** On the cursor's line, a task's box and a bullet's
   `•` stay concealed until the cursor goes into the prefix itself, so the text never shifts.
3. **`/` opens a formatting menu.** Typing `/` at the start of a line or after a space offers
   headings, tasks, quotes, emphasis, links and the rest, by name, with no syntax to remember.

## 2. Goals & success criteria

- **G1: One click ticks a task.** Clicking a concealed checkbox flips `[ ]`↔`[x]` in the file as
  a single undoable edit. The selection doesn't move and the line stays concealed.
- **G2: Keyboard-complete.** `thock::ToggleTask` does the same from the keyboard (CLAUDE.md,
  "Panels"), and every `/` entry is reachable without the mouse.
- **G3: Text never jumps while typing in a list item.** Moving onto a task or bullet line, and
  typing in it, leaves the item's text exactly where it was drawn.
- **G4: No syntax needed to format.** Every block and inline style the conceal renders can be
  produced from the `/` menu by name.
- **G5: The menu stays out of the way.** `/` in prose (`and/or`, `1 / 2`, paths, URLs) never
  leaves a menu open, and nothing triggers inside code.
- **G6: The hint teaches, then leaves.** New users see where the menu is; once they use it or
  ignore it, the hint is gone.

Non-goals: richer "complete" semantics in the editor (§4.2); user- or Routine-defined `/` entries
(§8); the iPhone (§8); running skills from `/` (that is the command palette's job).

## 3. Today

- `markdown_syntax.rs` scans a task line into a `Marker` span over `- ` and a `Checkbox(bool)`
  span over `[ ]`/`[x]`; a plain list line gets a `Bullet` span over the `-`/`*`/`+`.
- `markdown_conceal.rs` folds each span behind a placeholder. `checkbox_placeholder` draws the box
  and has no click handler. The module promises it never writes to the buffer (V10 §4.3).
- `revealed_rows` unfolds every span on a row the cursor or a selection touches (V10 §4.2), which
  is what makes the text jump.
- Enter on a task line already continues the list with `- [ ] ` (the Markdown language's
  `task_list` config).

## 4. Checkboxes

### 4.1 Click

- Clicking a concealed checkbox (the fold placeholder) replaces the one character between the
  brackets: `[ ]` becomes `[x]`, `[x]` or `[X]` becomes `[ ]`. Nothing else on the line changes.
- It is one transaction, so `cmd-z` restores it, and it does not touch the selections. The line
  stays concealed and the box flips under the pointer.
- The placeholder's hit area is the box plus its padding and shows a pointer cursor. A click
  anywhere else on the line behaves as it does today.
- Read-only editors (history previews) draw the box but ignore the click.

### 4.2 Same flip everywhere

The click and `ToggleTask` flip the box and nothing more, in every note, the Backlog and
reminders notes included. Moving a task to *Completed* and stamping ` ✅ <date>` stay with the
Backlog panel's *Complete* (V6 §6.3). A hand-checked task there already stops counting as open
work, so the panel needs no change. Toggling a parent leaves its children alone.

### 4.3 `thock::ToggleTask`

> *Tick or untick the task on this line.*

- Bound to `cmd-enter` (macOS) and `ctrl-enter` (Linux) under `Editor && ThockMarkdownConceal`,
  which every vault note's editor carries.
- For every selection, each line it covers that is a task gets flipped, all in one transaction.
  Lines that aren't tasks are skipped.
- If no line under any selection is a task, the action propagates, so `cmd-enter` stays
  `editor::NewlineBelow` everywhere else (the `go_to_wikilink` precedent).
- Works with conceal off too: it is a text edit, not a display feature.
- Vim needs no binding: `r x` already works on the revealed `[ ]`, and `cmd-enter` works in both
  modes.

### 4.4 The prefix stays drawn on the cursor's line

A **list prefix** is the run from a list line's indentation to its text: `- ` for a bullet,
`- [ ] ` for a task. On a revealed row, the prefix's spans stay folded unless the cursor is
**inside** the prefix:

```
cursor in the text          ☑ Agent should not dis|appear
cursor at the text start    ☑ |Agent should not disappear       (still concealed)
cursor inside the prefix    - [x]| Agent should not disappear   (revealed)
```

- **Inside** means strictly before the prefix's end. A cursor right after the prefix is in
  the text, so a fresh task from Enter shows its box with the cursor after it.
- Ways in: `home`, `cmd-left`, vim `0`/`^`/`I`, `left` from the text start, or a click left of
  the text. Once inside, the whole line shows its source, as it does today.
- A selection that covers any part of the prefix counts as inside.
- Only bullets and tasks. Quotes, headings and inline markers keep today's reveal rule; headings
  already keep their height (V43 §7.2).
- Ordered lists (`1. `) are not concealed today and stay as they are.

### 4.5 Backspace at the start of an item

In insert mode, with a single empty selection exactly at the end of a list prefix, `backspace`
deletes the whole prefix: `- [ ] buy milk` becomes `buy milk`, and `  - note` becomes `note`.
The next backspace joins with the line above as usual. With the cursor anywhere else, or with
any selection, `backspace` is unchanged. Vim normal mode is untouched.

## 5. The `/` menu

### 5.1 Trigger

The menu opens when `/` is typed and all of these hold:

- the editor is a vault note (`ThockMarkdownConceal`) in insert mode;
- the `/` is at the start of the line (after indentation) or right after whitespace;
- the cursor is not inside a fenced code block, inline code, a link destination or a wikilink.

It joins the existing `WikilinkCompletionProvider` as a fourth trigger, so it uses the editor's
own completion menu: arrows and `ctrl-n`/`ctrl-p` move, `enter`/`tab` pick, `escape` closes.

### 5.2 Filtering and dismissal

- The word after `/` fuzzy-filters entries by label and aliases (`/h2`, `/todo`, `/quo`).
- The menu closes, leaving the typed text as it is, on `escape`, on a space, or as soon as nothing
  matches. `and/or`, `1 / 2` and `/usr/bin` never keep it open (G5).

### 5.3 Entries

| Group | Label | Aliases | Applies |
| --- | --- | --- | --- |
| Blocks | Task | todo, checkbox | prefix `- [ ] ` |
| | Bullet | list | prefix `- ` |
| | Numbered | ol | prefix `1. ` |
| | Heading 1 / 2 / 3 | h1, h2, h3, title | prefix `# ` / `## ` / `### ` |
| | Quote | blockquote | prefix `> ` |
| | Divider | hr, rule, line | `___` line |
| | Code block | fence | ```` ``` ```` fence pair, cursor inside |
| | Table | | two-column header, rule and one empty row |
| Inline | Bold / Italic / Strikethrough / Code | b, i, s | `**\|**`, `_\|_`, `~~\|~~`, `` `\|` `` |
| | Link | url | `[\|]()` |
| Thock | Link to note | wiki, page | inserts `[[` and opens its completion |
| | Image | picture | inserts `![` and opens its completion |
| | Reminder | remind | inserts `(@` and opens its completion |
| | Today's date / Time now | date, time | `2026-10-28` / `14:05` |

`|` marks where the cursor lands. The Thock entries reuse the completions that already exist;
the menu only types their trigger.

### 5.4 Applying an entry

Picking an entry first removes the `/query` it was opened with, then:

- **Prefix entries rewrite the current line's prefix**, replacing any list, task, heading or quote
  prefix it already had and keeping its indentation. The cursor stays on the same character of
  text. `- buy milk /h2` → `## buy milk`; `## Notes /quote` → `> Notes`.
- **Divider, code block and table** replace the line when it is otherwise empty, else go on a new
  line below it.
- **Inline entries** insert their pair at the cursor.

Each pick is one transaction.

### 5.5 `thock::InsertFormatting`

> *Choose a heading, list, quote or style to insert.*

A command-palette action that types `/` at the cursor (with a space before it when needed) and
opens the menu, for people who don't know the trigger or have stopped seeing the hint. It is
unbound by default.

### 5.6 The empty-line hint

- While conceal is on and the newest cursor sits on an empty line of a vault note, the line shows
  muted ghost text: *Type / for formatting*. It is display-only and never written.
- Each new arrival of the cursor on an empty line where the hint draws is one **sighting**. Each
  pick from the menu, from `/` or `InsertFormatting`, is one **pick**.
- After **3 picks** (learned it) or **10 sightings** (ignoring it), the hint stops for good.
- Both counters live in the app's key-value store (`thock_slash_hint`, the `backlog_panel`
  precedent), per machine and never in the vault. A failed write is logged and the hint keeps
  showing, which is harmless.

## 6. Changes by file

| File | Change |
| --- | --- |
| `crates/thock/src/markdown_syntax.rs` | `list_prefix(line)`; `toggle_task_edit(line)`; `apply_block(line, entry)`; `slash_trigger(text, offset)` |
| `crates/thock/src/markdown_conceal.rs` | `ToggleTask` action (next to `ToggleMarkdownSource`); clickable `checkbox_placeholder`; prefix-aware `revealed_rows`; `ToggleTask` and backspace handlers; hint render; module doc updated for the two buffer writes |
| `crates/thock/src/wikilink_completion.rs` | `/` trigger, entry catalog and apply; `InsertFormatting` action (next to `InsertImage`) |
| `assets/keymaps/default-{macos,linux}.json` | `cmd-enter` / `ctrl-enter` → `thock::ToggleTask` under `Editor && ThockMarkdownConceal` |
| `thock/VISION.md` §12 | roadmap entry |

No inherited crate changes are expected. If the fold placeholder can't take a click without
moving the selection, the fix belongs in `editor`'s placeholder rendering and the PR says why.

## 7. Tests

- **Pure (`markdown_syntax`):**
  - `list_prefix` on bullets, tasks, nested and indented items, and look-alikes;
  - `toggle_task_edit` on `[ ]`, `[x]`, `[X]` and non-tasks;
  - `apply_block` for every prefix entry over every existing prefix, keeping indentation;
  - `slash_trigger` at line start, after a space, mid-word, in a fence, in inline code and in a URL.
- **Conceal (`VisualTestContext`):**
  - clicking the box flips the file and leaves the selections and folds as they were;
  - `cmd-enter` on a task line, on a multi-line selection, and falling through to NewlineBelow
    on a plain line;
  - the cursor in the text keeps the prefix folded, and `home` reveals it;
  - Enter on a task keeps the new line's box drawn;
  - backspace at the text start removes the prefix and is unchanged elsewhere.
- **Menu (keystrokes):**
  - `/h2` + `enter` rewrites the line;
  - `/` then space, and `and/or`, leave no menu;
  - `escape` keeps the `/`;
  - inline entries place the cursor inside the pair;
  - *Link to note* opens the wikilink completion.
- **Hint:** it draws on an empty line; it stops after 3 picks and, separately, after 10 sightings,
  with counters read back from the store.

## 8. Follow-ups

- **Vault-defined entries:** snippets and Routine templates as `/` entries, from a vault-visible
  file (Everything is editable).
- **iPhone:** tap-to-toggle and a formatting bar on the keyboard, following the same rules (§4.2,
  §4.4) so the desk and phone agree.
- Concealing ordered-list markers, so `1. ` gets the same prefix rule.

## 9. Decisions

1. **One spec, one release.** The checkbox and the menu solve the same problem, Markdown leaking
   into a non-programmer's way, and ship together.
2. **A click doesn't move the cursor.** Moving it would reveal the line and pull the box out from
   under the pointer.
3. **Reveal the prefix only when entered,** rather than padding the box to the source's width.
   Padding would leave every concealed task with a wide gutter, and the font swap on reveal would
   still shift it.
4. **A flip is just a flip.** Panels own move-and-stamp; the editor stays predictable and
   undoable.
5. **`cmd-enter` that falls through** keeps Obsidian's muscle memory without losing NewlineBelow
   outside tasks.
6. **Backspace drops the whole prefix,** so syntax never shows up one character at a time.
7. **The menu goes through the completion provider,** not a new popover, so it gets the editor's
   keyboard handling and the Thock entries hand off to the existing completions.
8. **The catalog is fixed in v1;** making it editable is §8.
9. **The hint counts in the key-value store,** not settings: it is per-machine UI state, not a
   preference.

## 10. Risks

- **R1: clicks on fold placeholders.** Today a click on a placeholder unfolds or selects. The
  click handler has to stop propagation before the editor moves the selection; if that isn't
  possible from the placeholder alone, a small `editor` change is needed (§6).
- **R2: a partly revealed row.** `revealed_rows` works per row; keeping some spans folded on a
  revealed row may disturb the impostor and repair logic in `apply_folds`. Cover it with the
  existing loop-guard tests.
- **R3: ghost text on an empty line.** An inlay at the end of an empty row must not shift the
  cursor or join the buffer text; check vim's `$` and `A` on that row.
- **R4: `/` in the completion provider.** Other providers' triggers, such as the language
  server's, must not open the menu, and the menu must not stop `[[` from working.
