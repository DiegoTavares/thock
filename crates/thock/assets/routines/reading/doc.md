# Reading

Readwise collects the passages you highlight on a Kindle, in articles, in
podcasts. This Routine brings them home: Thock polls Readwise in the
background and keeps **one plain Markdown note per book** in your vault,
appending each new highlight as it appears. The notes are yours; Thock only
ever adds to them.

## Connecting

Run **Connect Readwise** (`thock: connect readwise` from the command palette,
or the Readwise row behind the sync icon in the bottom bar). Thock opens `readwise.io/access_token`
in your browser; copy the token, paste it into the prompt, press `enter`.
The token is checked, then kept in your system keychain — it never touches
the vault. Within a minute `reference/readwise/books/` fills with one note
per book you have highlights for.

## What a note looks like

```markdown
---
source: readwise
readwise_id: 28374651
category: books
---
# A Fé Na Era Do Ceticismo

## Metadata
- Author: [[Timothy Keller]]
- Full Title: A Fé Na Era Do Ceticismo
- Category: #books

## Highlights
- “Qual é seu maior problema…” ([Location 426](https://readwise.io/to_kindle?…)) <!--rw:512340987@2026-09-28-->
    - Note: use
```

The body follows the Readwise Obsidian plugin's familiar template. The
author is a `[[wikilink]]`, so authors become hubs in your graph. The
invisible comment at the end of each highlight is its identity and the day
you highlighted it; the editor hides it, and it is what lets the Reading Week
ritual ask "what did I highlight this week?" from the note alone. Leave it
in place; everything else on the line is yours.

## The rules Thock follows

- **Append-only.** The first time a book is seen, its note is created with
  every highlight. After that, the only thing Thock writes is new highlight
  lines at the end of `## Highlights`. Reword a highlight, add your own
  sections, move things around — nothing is corrected back.
- **Deleted stays deleted.** Remove a highlight line and it never comes back.
  Delete a whole note and only highlights made *after* that land in a fresh
  one.
- **Read-only toward Readwise.** Nothing is created, tagged, or changed on
  their side. Edits and deletions you make in Readwise afterwards don't reach
  lines already in the vault either.
- **A late highlight lands at the end,** even when it sits earlier in the
  book: inserting it in the middle would mean editing among your lines.

## Make it yours

`.thock/readwise.toml` is the map: one `[[sync]]` entry per Readwise
category (`books`, `articles`, `tweets`, `podcasts`, `supplementals`) and the
folder its notes land in. The shipped file syncs books only; adding podcasts
is two lines:

```toml
[[sync]]
category = "podcasts"
path     = "reference/readwise/podcasts"
```

`poll_minutes` sets the cadence (hourly by default). Deleting the file turns
the sync off and leaves every note where it is; `thock: disconnect readwise`
also forgets the token.

## Reading Week

**Reading Week** (`/reading-week`) looks at the week's highlights, says which
books were active and how far into them you got, and keeps up to three
passages worth remembering — preferring the ones you annotated. Week Review
runs it for you when this Routine is installed, so the weekly note gains a
`### Reading` section; run it by hand for a week that had no review. What it
learns goes through `memory/inbox.md` and the Reflect ritual, like everything
else your agent remembers.
