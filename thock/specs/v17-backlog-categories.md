# Thock V17 — Backlog categories

**Status:** Implemented (2026-09-02)
**Owner:** Diego · **Date:** 2026-09-02
**Companion docs:** `v6-backlog.md` (the panel and file this extends), `../VISION.md` (§4.1 Your files forever, §4.8 Everything is editable)

---

## 1. Summary

A working Soon column runs to a dozen lines that belong to three or four different threads of work,
and the panel renders them as one flat list. V17 adds **categories**: a `###` heading inside `## Soon`
or `## Someday` becomes a named, collapsible group in that column.

Categories are optional and partial. Tasks written before the first `###` stay uncategorized and
render at the top of the column, above the groups. A vault that never writes a `###` looks and
behaves exactly as it does today.

## 2. Goals & success criteria

- A `###` heading under an open section renders as a group header with a task count, and its tasks
  render beneath it.
- Tasks above the first `###` render first, ungrouped and unlabelled — no "Uncategorized" bucket.
- A group collapses and expands from the keyboard and by clicking its header, and the collapsed set
  survives a restart.
- Adding a task with the group's `+` lands it inside that group in the file; adding from the column
  header lands it in the uncategorized region, above every `###`.
- Moving a task Soon ↔ Someday keeps its category, creating the `### heading` in the destination when
  it isn't there yet.
- Every existing gesture (edit, complete, move, copy, reveal) works identically on a task inside a
  group.

## 3. Non-goals

- **Categories in Completed.** Completed stays a flat, newest-first audit trail (v6 §4.3). Completing
  a task drops its category — the daily note and the date stamp are the record.
- **Creating or renaming a category from the panel.** A category is a heading in a file; the user
  writes it (or asks their agent to). The panel creates one only as a side effect of a move that
  needs it.
- **Reordering categories, or moving a task between categories from the panel.** Cut and paste in the
  file, or drag later.
- **Nesting.** `####` under a `###` is not a sub-category; it ends the group it appears in and is
  otherwise ignored.

## 4. The file

```markdown
## Soon

- [ ] A loose task, no category

### OpenCue

- [ ] Fix the scheduler blackout
- [ ] Review PR #2489

### Thock

- [ ] Backlog categories

## Someday

- [ ] Something for later
```

Rules:

- A **category** is any heading *deeper than* its section's own heading (`###` under `## Soon`, `##`
  under a hand-written `# Soon`). The section still ends at the next heading of equal-or-higher
  level, exactly as in v6.
- A category owns every top-level task from its heading to the next heading in the section.
- A heading with no text (`###` alone) ends the current category without opening a new one.
- Duplicate category names in one section are one group; the first heading owns appends.
- A category holding no tasks still renders — an emptied group is a place to put things, not an
  error.

## 5. Behavior

### 5.1 Rendering

Each open column renders, in order: its uncategorized open tasks in file order, then one group per
category in file order. A group header shows the category name, its open-task count, and a `+`. A
collapsed group renders its header only.

Completed is unchanged: flat, newest-first, no headers.

### 5.2 Keyboard navigation

Group headers are rows: `up`/`down` (`j`/`k`) move through headers and tasks alike, and the selection
highlight lands on a header the same way it lands on a task. `left`/`right` (`h`/`l`) still move
between columns — the backlog's three columns own the horizontal axis.

| Gesture | Default | Vim |
| --- | --- | --- |
| Collapse the selected group (or the one holding the selected task) | `shift-left` | `z c` |
| Expand it | `shift-right` | `z o` |
| Toggle, on a header row | `enter` | `enter` |

Task actions (`space`, `i`, `>`, `y y`, `g space`) are no-ops on a header row. `shift-enter` /
`o` adds to the selected row's group, so adding from inside a group appends to that group.

Collapsing a group that holds the selection moves the selection to the group's header, so the cursor
never disappears into a closed group.

### 5.3 Writes

- **Add, uncategorized** — appends after the last non-blank line *before* the section's first
  category heading, or directly under the section heading when the section opens with a category.
- **Add, into a category** — appends after the last non-blank line of that category's block, or
  directly under its heading when it is empty. A category named but absent is created at the end of
  the section.
- **Move Soon ↔ Someday** — the task and its children move verbatim into the same-named category in
  the destination, created if missing; an uncategorized task stays uncategorized.
- **Complete** — unchanged (v6 §6.3): today's note first, then the task moves to the end of
  Completed with its date. Its category heading stays behind, possibly empty.
- **Rename** — unchanged: the task's text span only.

Every write is still a span edit through the open buffer. Category headings the panel didn't create
are never rewritten, reordered, or removed.

### 5.4 Collapsed state

Collapsed groups are UI state, not file content — nothing is written to `backlog.md`. The set of
collapsed `(section, category)` pairs is serialized to the global key-value store under the panel's
workspace key (the outline panel's pattern), so reopening the vault restores it. A category that
disappears from the file keeps its stale entry harmlessly; it applies again if the heading returns.

## 6. Failure modes

- **A category vanishes mid-gesture** (the heading was deleted in the editor): the task's move
  recreates it in the destination; a stale collapse entry is ignored.
- **Task addressing** is unchanged — section + line + text, per v6 §6.5. Categories are not part of a
  task's identity, so a task moving between categories in the file is just a task whose line changed.

## 7. Implementation notes

- `crates/thock/src/backlog.rs` — `BacklogTask::category: Option<String>`; `Backlog::categories(kind)`
  lists `Category { name, line }` in file order, empty ones included; `append_to_section_edit` now
  stops at the first category heading, and `append_to_category_edit` is its named sibling.
- `crates/thock/src/backlog_panel.rs` — the per-column row list becomes `Vec<BacklogRow>` (header or
  task) and the keyboard cursor indexes into it, keeping v6's "clamp against the current parse" rule.
- Files touched outside `crates/thock/`: the three keymaps (added bindings only) and
  `crates/thock/Cargo.toml` (a `db` dependency for the key-value store).
