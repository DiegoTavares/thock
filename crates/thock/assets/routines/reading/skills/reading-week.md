# Reading Week

What the user read this week, and the passages worth keeping. Works only from the vault — the synced Readwise notes — and makes no API calls.

**Reads:** every note with `source: readwise` frontmatter under the folders `.thock/readwise.toml` maps (read the file to find them; `reference/readwise/books/` by default — never hard-code it), `profile.md`, `memory/index.md`, `memory/reading.md`.
**Writes (append-only):** the weekly note (`# Reading Week` section, standalone mode only), `memory/inbox.md`.

> **Read `profile.md` first, if it exists.** Its **What Thock should not keep** list wins here too: a book or topic named there is skipped in silence — not in the review block, not in the memory lines, not mentioned.

## Two modes

- **From Week Review.** Week Review hands you the review's Monday–Sunday window and expects back the `### Reading` block from §4 as text to include in its own review. Write the memory lines (§5) yourself; do **not** touch the weekly note — the review appends once, for everything.
- **Standalone.** Someone ran Reading Week on its own. The window is **last week**, Monday–Sunday. Append your own `# Reading Week` section (create the weekly note from `templates/weekly.md` if it's missing; `[weekly]` in `.thock/config.toml` says where it lives and how it's named), then write the memory lines and tell the user to run Reflect, or that the next Wrap Today will pick them up.

**A week with no highlights produces nothing:** no heading, no inbox lines, no comment. In Week Review mode, hand back an empty block.

## 1. Collect the week's highlights

Every highlight line ends with an invisible marker, `<!--rw:<id>@<YYYY-MM-DD>-->`; the date is the day the highlight was made. Scan every synced note and keep the lines whose marker date falls inside the window. A `- Note:` sub-bullet right under a highlight is the user's own annotation of it; keep it with its highlight. A line without a date in its marker has no day and is skipped.

## 2. Derive progress per book

For each book with at least one highlight in the window:

- the number of new highlights,
- the location or page range they span (`locations 1074–1787`, `pages 12–40`; podcasts and articles may have neither — then just the count),
- **started**, when the book's earliest marker date of *all* its highlights falls inside the window.

Books with no activity this week are not mentioned. There is no "finished": Readwise doesn't know, so neither do you.

## 3. Pick the quotes worth keeping

Up to **three** across all books, in this order of preference:

1. highlights the user annotated with a `- Note:` (their own words make it theirs),
2. then the longest unannotated ones.

Each keeps its book, its author (from `- Author:` in the note's Metadata), and a `[[wikilink]]` to the note by its file name. Quote verbatim — these are the author's words, not yours to tidy.

## 4. Write the review block

```markdown
### Reading
- **A Fé Na Era Do Ceticismo** (Timothy Keller): 6 highlights, locations 1074–1787 [[A Fé Na Era Do Ceticismo]]
- Started **The Psychology of Money** (Morgan Housel): 2 highlights [[The Psychology of Money]]

> “Toda boa dádiva e todo dom perfeito vêm do alto…” (Keller). Your note: *graça comum*
```

One line per active book, then one block quote per kept passage, with `Your note: *…*` only when the user annotated it. Use the person's language (the vault's `## Language`, if set) for the prose; the quotes stay as written.

## 5. Note what memory should keep

Append dated lines to `memory/inbox.md` — the only memory write allowed outside Reflect (AGENTS.md rule 6). Reflect files them onto `memory/reading.md`:

```markdown
- 2026-10-05 · Reading *A Fé Na Era Do Ceticismo* (Timothy Keller), active this week
- 2026-10-05 · Kept a quote from *A Fé…* (Keller): “Toda boa dádiva…”; their note: graça comum
```

One line per active book, one per kept quote. Record *that* they read something and *which* passage they kept, attributed to the author — never restate a quote as something true about the person or the world.

## Output

In Week Review mode, your output is the block; the review does the talking. Standalone, one or two sentences: which books were active and that the section is at the end of the weekly note, plus whether there are memory lines waiting for Reflect. Never paste the block into the chat.
