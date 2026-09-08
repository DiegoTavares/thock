# Thock V24: Inline markup in the concealed editor

**Status:** Implemented (2026-09-07)
**Owner:** Diego · **Date:** 2026-09-07
**Companion docs:** `v10-markdown-conceal.md` (the machinery this extends: §5 reveal rule, §6 scanner, §7 colour), `v16-email-view.md` (the other consumer of the same span plan)

---

## 1. Summary

V10 shipped conceal for the constructs that hurt most (headings, links, comments, checkboxes, the
`___` rule) and explicitly deferred emphasis: *"`**bold**` markers stay visible in V10. They are
cheap to add later on the same machinery"* (v10 §3). This is later.

V24 adds the everyday inline markup a note is actually made of: `**bold**`, `*italic*`,
`` `code` ``, list bullets, and blockquote lines. Every one of them rides the two primitives V10
already built, a fold with an invisible (or one-glyph) placeholder and a keyed text highlight, so
the change is a new set of span kinds in the scanner plus their styles. Nothing about the reveal
rule, the fold diff, or the display-only contract moves.

## 2. Goals & success criteria

- **G1**: A line the cursor is not on shows bold text bold, italic text slanted, inline code in the
  theme's code colour, a list item behind a `•`, and a quoted line receding into `text_muted`, with
  no `*`, `_` or `` ` `` on screen.
- **G2**: The V10 contract is untouched: the cursor's line shows its exact source, the buffer is
  never written, and `git diff` after a reading session is empty.
- **G3**: Nothing changes width in a way that shifts the page. Emphasis and code delimiters fold to
  the same invisible placeholder every other marker uses; a bullet is one column of source for one
  glyph.
- **G4**: The shapes a real note is full of are *not* emphasis: `snake_case_name`,
  `https://a.example/a_b_c`, `2*3*4`, a `*` that opens a list item, and any `*` inside a link
  destination or a code span all stay literal.
- **G5**: Styles stack rather than fight. `***both***` is bold *and* italic; emphasis over a link
  keeps the link's colour and gains the weight; a link inside a heading reads as a link.

## 3. Non-goals

- **Ordered lists.** `1.` is already legible and already means what it looks like. Concealing it
  would have to renumber, which is a rewrite, not a display.
- **Blockquote indentation or a drawn quote bar.** The `>` stays on screen and takes the muted
  colour with the rest of its line. Folding it would move the line sideways for no gain, and a
  drawn bar is a block-level shape V10 §3 rules out.
- **Setext emphasis oddities, reference links, footnotes, tables, images.** Unchanged from V10.
- **Emphasis in the panels.** `markdown_text::render_markdown_row` (Backlog, Day Planner) still
  renders links and strikethrough only. Extending it is a separate, self-contained change.
- **Font size.** Still never changes (v10 §1).

## 4. What gets concealed

Extending v10 §6's table. Every rule there (C1–C9) still holds: nothing inside a fence, front
matter, an inline code span or a concealed comment is scanned, and a malformed construct produces no
spans at all.

| Construct | Source | Concealed | Styled |
|---|---|---|---|
| Bold | `**text**`, `__text__` | both delimiter runs | the text, `FontWeight::BOLD` |
| Italic | `*text*`, `_text_` | both delimiter runs | the text, `FontStyle::Italic` |
| Bold italic | `***text***`, `___text___` | both delimiter runs | the text, both of the above |
| Inline code | `` `code` `` | both backtick runs | the text, the theme's `text.literal` colour |
| List bullet | `^[ \t]*[-*+][ \t]` | the marker character | (drawn as `•`, see §5.2) |
| Blockquote | `^ {0,3}>…` | nothing | the whole line, marker included, `text_muted` |

Rules of its own:

- **C10: A delimiter run is one, two or three characters, and the same length at both ends.**
  `****four****` is literal text. The opener must be followed by non-whitespace and the closer
  preceded by it.
- **C11: A delimiter may not touch a word character.** This is the whole of G4: `snake_case`,
  `a_b_c` in a URL and `2*3*4` are punctuation inside a word, not emphasis. Every non-ASCII byte
  counts as a word character, so `café*` opens nothing either. A backslash escapes the construct.
- **C12: Emphasis nests, up to three levels.** `**bold *and italic* here**` folds four delimiters
  and stacks two styles; the cap keeps a line of nothing but delimiters from recursing per
  character.
- **C13: A delimiter inside another construct belongs to that construct.** A `*` in a link
  destination, a code span or a comment is not a delimiter. This is the same rule V10 gives `~~`
  (C9), for the same reason: folding it would overlap that construct's own folds. Emphasis may
  still *span* a link.
- **C14: A bullet needs whitespace after it**, so a `*` that opens emphasis is never a bullet; and a
  line whose every non-blank character is the marker (`* * *`, `- - -`) is a thematic break, not a
  list.
- **C15: A task line keeps both.** `- [ ] task` draws as `• ☐ task`: the bullet is the list's, the
  box is the task's. (V10 §7.4 left the bullet alone because there was nothing to draw it as.)

## 5. Colour and drawing

As in v10 §7, every colour comes from the active theme and V24 introduces no theme keys.

### 5.1 Emphasis

Bold and italic take **weight and slant only, with no colour of their own**, the same trick V10 §7.5
uses for strikethrough. Whatever the theme's syntax highlighting already paints `emphasis` /
`emphasis.strong` survives underneath (One paints them blue and orange; the fallback theme paints
nothing), and emphasis over a link keeps the link's colour and gains the weight. It also means
italic reads as italic in a theme that only recolours it, which is the point of the feature.

### 5.2 Bullets

The marker folds to a `•` in `text_muted`, drawn by the placeholder the way the checkbox is. One
source column becomes one glyph, so the item's text does not move. All nesting depths use the same
glyph, since depth is already carried by the indentation.

### 5.3 Inline code

The backticks fold and the text takes `text.literal` from the syntax theme, the name the Markdown
grammar gives a code span, so a code span in a note and a fenced block in the same theme agree on
what code looks like. Falls back to `text_muted` in a theme that does not define it.

### 5.4 Blockquotes

The whole line, `>` included, takes `text_muted` and nothing folds. A quote should recede, not
disappear, and the marker is what says the line is a quote.

## 6. Highlight slot order

The slot table gained six entries and, with them, an order that matters: overlapping highlights
merge with the **later slot winning**, so the table is now written as an explicit priority list.
Quote tint, heading levels, bold, italic, code, wikilink, link, strikethrough, then the email view's
trio.

Two consequences worth stating:

- The quote tint sits under everything a quoted line may also contain, so a link inside a blockquote
  is still link-coloured.
- The link colours now sit **above** the heading levels, which makes real what v10 §7.2 always
  claimed: a `[[wikilink]]` inside a heading reads as a link rather than taking the heading's colour.

Heading levels keep the exact palette entries they had. The slot arithmetic moved, the colours did
not.

## 7. Files

| File | What |
|---|---|
| `crates/thock/src/markdown_syntax.rs` | `Bold`, `Italic`, `Code`, `Bullet`, `Quote` span kinds; the emphasis scanner (`each_emphasis` / `parse_emphasis`), `bullet_marker`, `blockquote_range`, and code spans that now carry their inner text |
| `crates/thock/src/markdown_conceal.rs` | the bullet placeholder, the named slot table and its styles |

No upstream file is touched, and the expected upstream diff stays zero lines.
