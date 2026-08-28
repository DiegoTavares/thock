# Thock V10 — Concealed markup in the Markdown editor

**Status:** Shipped (2026-08-19) — spike forced the §11.1 fallback; see §14
**Owner:** Diego · **Date:** 2026-08-19
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.8 Everything is editable, §5 The product experience), `v4-day-planner-panel.md` (the theme-derived colour precedent), `v6-backlog.md` (inline Markdown in panels, `markdown_text.rs`)

---

## 1. Summary

Thock's core surface is a Markdown file in Zed's editor. That editor is excellent for *writing*
Markdown and mediocre for *reading* it: every `#`, `[[`, `](https://…)` stays on screen forever, so
a note that is 90% prose reads as 100% syntax. The existing viewing mode (`markdown_preview`) fixes
reading by giving up editing, which is the wrong trade for a vault you live in.

V10 adds **conceal**: while the cursor is elsewhere, a line renders the way it would in preview —
markers hidden, links and headings coloured, `___` drawn as a rule. Put the cursor on the line and
the raw source comes back, unchanged and fully editable. No preview pane, no mode switch, no second
editor.

Two things make this cheap enough to be worth doing:

1. **It is display-only.** The buffer is never touched, so the file on disk, saving, git history, and
   every skill that reads the vault are all unaffected by construction.
2. **Font size never changes.** Headings differ by *colour*, not size. Zed's editor lays out on a
   single line height per editor; varying it per row would be deep surgery in `element.rs`. Colour
   costs nothing.

The whole feature lives in `crates/thock/`, using editor APIs that are already `pub`. The expected
upstream diff is **zero lines** (§10.1 is the one place that might change that).

## 2. Goals & success criteria

- **G1** — A daily note read at a glance shows prose, coloured headings, coloured link labels and a
  drawn rule; no `#`, no brackets, no URLs.
- **G2** — Putting the cursor on any line restores that line's exact source, and editing it behaves
  identically to editing Markdown today. Nothing is ever "sort of" editable.
- **G3** — The bytes on disk are identical whether conceal is on or off. This is the acceptance test
  that matters most: `git diff` after a session of reading must be empty.
- **G4** — Nothing inside a fenced code block or an inline code span is ever concealed. A `# heading`
  in a bash snippet stays a `#`.
- **G5** — Turning conceal off is one keystroke, and the setting persists per vault.
- **G6** — A Markdown file *outside* a vault (a README in a code repo, this spec) edits exactly as it
  does today.

**Success:** the author stops opening viewing mode to read their own notes.

## 3. Non-goals (explicitly out of V10)

- **Rendering anything with a size or a shape.** No inline images, no tables laid out as tables, no
  syntax-highlighted code-block chrome, no font-size changes. Colour and hidden markers only.
- **Bold / italic / strikethrough.** `**bold**` markers stay visible in V10. They are cheap to add
  later on the same machinery, but they interact with a `.md` writer's muscle memory more than
  headings do, and the four requested constructs are the ones that hurt.
- **Images (`![alt](src)`).** Concealing the markers would imply an image is coming. Left untouched.
- **Setext headings** (`Title` over `=====`) and the `---` / `***` forms of a thematic break. `---`
  is ambiguous with YAML front matter and with a setext underline; V10 conceals `___` only, as
  specified.
- **Clicking a `[[wikilink]]` to open the file.** Colour is not a click target (§10.6). Real
  cmd-click support belongs in `hover_links.rs` and is deferred to its own change.
- **Concealing in multi-buffers, minimaps, single-line editors, or read-only editors.**
- **A new Markdown crate.** `crates/markdown` is a read-only render element with no cursor and no
  buffer, and `markdown_preview` is a pane built on it. Neither is an editing surface; forking either
  produces a nicer preview, not a nicer editor. V10 decorates `crates/editor` instead.

## 4. Core concepts

### 4.1 Two primitives, both already public

Everything in V10 reduces to *hide these characters* and *recolour these characters*.

**Hide** is a fold with a custom placeholder. `FoldPlaceholder`
(`crates/editor/src/display_map/fold_map.rs:27`) carries a `render` closure and a `collapsed_text`;
with `constrain_width: false` the element lays out at min-content, so an `Empty` element occupies
zero width and the markers disappear. There is a working precedent for the entire pattern in-tree:
`Editor::refresh_single_line_folds` (`crates/editor/src/fold.rs:900`) folds every newline in
single-line editors — background parse, diff against existing folds, apply through
`display_map.fold()`, keep its own folds separable via `type_tag`. V10's module is that function with
a different range producer.

**Recolour** is `Editor::highlight_text(key, ranges, style, cx)` (`crates/editor/src/editor.rs:9353`),
keyed so we only ever clear our own highlights.

### 4.2 The line is the unit of reveal

A *concealed span* is a buffer range whose display is suppressed. A *revealed line* is a line the
user is currently working on, where every concealed span is temporarily dropped.

V10 reveals **whole lines**, not individual constructs. Cursor anywhere on a line ⇒ that line's full
source is visible. This is one predicate instead of per-construct state, it makes horizontal motion
stable (constructs don't expand and collapse *underneath* a moving cursor), and it means the user
never has to wonder which of two links on a line is "active".

### 4.3 Display-only, always

No code path in V10 may write to a buffer. Folds and highlights live in the `DisplayMap`; copy,
save, search-and-replace, skills and git all operate on buffer text and are unaffected. If a bug in
this feature can lose a character, the design is wrong.

### 4.4 Vault-gated

Conceal is a note-taking behaviour, not an editor-wide opinion. It applies to `.md` buffers whose
file lives under an open Thock vault root. A README in a code project, and this spec, edit exactly as
they do today.

## 5. The reveal rule (this is the contract)

Given the set of the editor's selections:

- **R1** — A line is **revealed** if any selection's head or tail lies on it, or if any selection
  covers any part of it. A cursor at column 0 of line *n* reveals line *n* only.
- **R2** — With multiple cursors, the revealed set is the union. There is no "primary" cursor.
- **R3** — A selection spanning lines 4–9 reveals lines 4–9 inclusive. The user is about to copy or
  delete that text and must see what it is.
- **R4** — Reveal follows the cursor regardless of window focus. Clicking into a panel does not
  re-conceal the line the user was editing; coming back must find the editor exactly as it was left.
- **R5** — A concealed span that straddles lines (none do in V10, but the model allows it) is
  revealed if any of its lines is revealed.

Corollary of R1 that must be tested: moving down through a note re-conceals the line behind you on
the same frame as it reveals the line you arrived at, so total document width never oscillates by
more than one line's worth.

## 6. What gets concealed

Ranges come from a **line-based scanner** in Thock, not from tree-sitter. Wikilinks are not in the
Markdown grammar at all (`crates/grammars/src/markdown/`), the fence-tracking needed for G4 is a
three-state machine, and a pure `&str → Vec<ConcealSpan>` function is unit-testable with no GPUI and
no async syntax-tree availability. `markdown_text.rs` is the precedent for this shape of parser.

| Construct | Source | Concealed | Coloured |
|---|---|---|---|
| ATX heading | `^ {0,3}(#{1,6})[ \t]+(.*)$` | the `#…` run **and** the whitespace that follows it | the remaining text, by level |
| Wikilink | `[[target]]` | `[[` and `]]` | `target` |
| Wikilink with alias | `[[target\|alias]]` | `[[`, `target\|`, and `]]` | `alias` |
| Inline link | `[text](dest)` | `[` and `](dest)` | `text` |
| Thematic break | `^ {0,3}_{3,}[ \t]*$` | the entire line's text | — (drawn, see §7.3) |
| Task checkbox | `^[ \t]*[-*+][ \t]+\[( \|x\|X)\]([ \t]\|$)` | the `[ ]` / `[x]` brackets | — (drawn, see §7.4) |
| HTML comment | `<!-- … -->`, opened and closed on one line | the whole comment, delimiters included | — |
| Strikethrough | `~~text~~` | both `~~` delimiters | — (struck, see §7.5) |

Rules that apply to all of them:

- **C1** — Nothing inside a fenced code block (` ``` ` / `~~~`) or an inline code span (`` ` ``) is
  ever concealed or coloured. The scanner tracks fences across lines and spans within a line.
- **C2** — Nothing inside a YAML front-matter block (a `---` fence in the first line position) is
  concealed.
- **C3** — A malformed construct is left entirely alone. `[text](` with no closing paren, `[[` with
  no `]]`, a heading with no space after the `#` — all render as typed. Half-typed text must never
  flicker.
- **C4** — `![alt](src)` is not a link for our purposes; the `!` prefix disqualifies the match.
- **C5** — An inline link whose `text` is empty conceals nothing (there would be nothing left to
  show).
- **C6** — A checkbox needs a list bullet before it and a space or end of line after, so a `[x]`
  written in prose is not a task. Indentation is unrestricted: a nested task is still a task.
- **C7** — A comment that does not close on its own line is left visible, like any other malformed
  construct. Hiding a multi-line comment would have to fold the newlines between the prose around
  it, which is a different (and lossier) gesture than hiding markup.
- **C8** — Markup inside a concealed comment is not scanned separately: a `[[wikilink]]` in a
  comment neither colours nor resolves. Concealed comments join inline code as an exclusion zone.
- **C9** — A `~~` delimiter must sit outside every link construct: a `~~` in a URL belongs to the
  URL, and folding it would overlap the link's own folds. A strikethrough may still *span* a link
  (`~~see [docs](url)~~`), which keeps its link colour and gains the line. Delimiter runs are
  exactly two tildes (`~~~` opens a fence), and the struck text may not be empty or start or end
  with whitespace — anything else is malformed and stays literal (C3).

## 7. Colour and drawing

All colours come from the active theme; V10 introduces no new theme keys and must look deliberate in
any theme the user installs.

### 7.1 Headings

Levels 1–3 take three distinct, stable slots from the theme's player palette, the same mechanism the
Day Planner uses for its subsection colours (`day_plan::section_palette_slot` + `theme.players()`).
Levels 4–6 reuse level 3 — deeper nesting is rare in notes and three signals are enough to read
structure at a glance. Font weight and size are untouched.

### 7.2 Links

- **Internal** (`[[wikilink]]`) → `theme.colors().text_accent`.
- **External** (`[text](url)`) → the syntax theme's `link_uri` colour.

Internal and external must be visibly different: knowing whether a click would leave the vault is
the point of colouring them at all.

### 7.3 Thematic break

The whole line's text is folded; the placeholder renders a 1px full-width div. The renderer receives
`max_width` in its `ChunkRendererContext` (`crates/editor/src/element.rs:7153`), so a real
edge-to-edge rule is drawable rather than a fixed-width stub.

### 7.4 Task checkboxes

The `[ ]` / `[x]` folds to a drawn box: a 13px bordered square, filled with the app's `Check` icon
in the accent colour when the task is done, so a note's tasks read the way they do in every other
Thock surface. The list bullet is left alone — it is the user's text, and hiding it would change
what the line *is*, not just how its markup looks. The box is display-only; ticking a checkbox by
clicking it would be a buffer write and belongs with the Day Planner's task actions, not here.

### 7.5 Strikethrough

The delimiters fold and the text between them takes a strikethrough highlight with no colour of its
own, so a struck link keeps its link colour and a struck heading its level colour. The panels
(Backlog, Day Planner) render the same treatment through `markdown_text::render_markdown_row`, and
the Day Planner goes one step further: a task whose whole text is crossed out
(`- [ ] ~~09:00 Meeting~~`, or `- [ ] 09:00 ~~Meeting~~`) reads as finished — the completed icon,
the muted block fill — in `text_disabled` rather than `text_muted`, so a dropped task stays
distinguishable from a done one. The strikethrough may wrap the time token without knocking the
task off the grid. A partly struck task (`Call ~~Bob~~ Alice`) is still open.

## 8. Architecture

### 8.1 Shape

New module `crates/thock/src/markdown_conceal.rs`, wired from `thock::init` — no upstream file is
touched, following the pattern `git_ui` uses to attach to every editor from its own crate
(`crates/git_ui/src/git_ui.rs:88`):

```rust
pub fn init(cx: &mut App) {
    cx.observe_new(|editor: &mut Editor, _, cx| register(editor, cx))
        .detach();
}
```

`register` bails unless the editor is full-mode, singleton, not read-only, not a minimap, and its
buffer is a `.md` file under a vault root. Otherwise it installs a `MarkdownConcealAddon`
(`editor::Addon`) holding the parsed plan, the per-editor enabled flag, and the refresh task, and
subscribes to the editor.

### 8.2 The refresh loop

Two triggers, deliberately asymmetric in cost:

- **`EditorEvent::Edited`** → reparse. Debounced (~50 ms) and run on a background task over a
  snapshot, exactly as `refresh_single_line_folds` does. Produces the full span plan as anchors.
- **`EditorEvent::SelectionsChanged`** → recompute the revealed line set only. No reparse; this runs
  on every arrow key and must stay allocation-light.

Both end in the same apply step:

1. Compute the desired fold set = all spans minus those on revealed lines.
2. Diff against the folds currently present carrying our `type_tag`.
3. `display_map.remove_folds_with_type(stale, TYPE_ID, cx)` then `display_map.fold(new, cx)`.

**Never call `Editor::fold_creases`.** It routes through `folds_did_change`, which persists folds
(§10.1). Going through the `DisplayMap` directly is not an optimisation, it is the correctness
requirement — and it is why the newline-fold precedent does the same.

Highlights are cheaper: clear our keys and reapply the full anchor set on reparse only. Revealing a
line does not need to drop its colours, only its folds — a heading on the cursor's line stays
coloured while showing its `#`.

### 8.3 Files

| File | What |
|---|---|
| `crates/thock/src/markdown_conceal.rs` | addon, registration, subscriptions, fold/highlight apply |
| `crates/thock/src/markdown_syntax.rs` | pure scanner: `&str → Vec<ConcealSpan>`, fences, all of §6 |
| `crates/thock/src/thock.rs` | one line in `init` |
| `crates/thock/src/vault.rs` | `[markdown]` config section |
| `assets/keymaps/default-{macos,linux}.json` | one binding for the toggle |

Estimated ~700 lines including tests. The scanner carries the bulk of the test coverage and needs no
GPUI.

## 9. Configuration and actions

`.thock/config.toml`:

```toml
[markdown]
conceal = true   # default when inside a vault
```

Action `thock::ToggleMarkdownSource` — palette entry **"Thock: Show Markdown source"**. It toggles
the addon's flag for the focused editor only; new editors start from the config default. The addon
contributes a `ThockMarkdownConceal` key context so the binding is scoped to editors where the
feature is actually live.

An unparseable or absent `[markdown]` section means defaults, consistent with the rest of
`config.toml`.

## 10. Interaction with the rest of the editor

### 10.1 Fold persistence — the one real hazard

`folds_did_change` (`crates/editor/src/fold.rs:806`) snapshots **every** fold in the editor into
workspace restoration data, and `load_folds_from_db` restores them on reopen — as plain folds,
without our placeholder, which would come back as literal `⋯` ellipses in the middle of sentences.

Applying through `display_map.fold()` (§8.2) means *we* never trigger a snapshot. But any unrelated
fold the user makes triggers one that sweeps ours in. Mitigation, in order of preference:

1. On register, drop restored folds that exactly match a concealed span — a Thock-side purge, zero
   upstream diff.
2. If (1) proves unreliable, teach `folds_did_change` to skip folds carrying a `type_tag`. That is a
   one-line upstream change, mechanical across rebases, and **must be called out in the PR body**
   per the fork-discipline rules.

### 10.2 User fold commands

`unfold_all`, the gutter chevrons and vim's `zR` / `zM` operate on all folds and will wipe ours.
`type_tag` + `remove_folds_with_type` keep our removals from touching the user's; for the reverse we
re-apply after any fold change rather than assuming our state survived. Conceal folds must never
appear in the fold gutter as toggleable creases.

### 10.3 Motions and vim

Motions operate on display points, so a concealed construct is traversed as though its markers were
not there — which is the desired reading behaviour, and matches Neovim's `conceallevel`. If the spike
forces the one-character-placeholder fallback (§11.1), each concealed span costs one display column
that `l`, `$` and click-to-position will see; that is the main reason the spike comes first.

### 10.4 Search and selection

`find` matches buffer text, including concealed markers, and a selection dragged across a concealed
`](url)` silently includes it. This is standard for the class of feature (Obsidian and Neovim behave
the same way) and is accepted rather than solved. R3 exists so that the moment a selection exists,
the user can see what it holds.

### 10.5 Soft wrap

Wrapping measures shaped widths, so zero-width folds shorten lines and the full-width rule element
occupies a line by itself. Both need a look during the spike; neither is expected to be interesting.

### 10.6 Clicking links

Deferred (§3). `hover_links.rs` already resolves cmd-click on paths and URLs; teaching it `[[…]]` is
the natural home, and it is the one genuinely upstream-shaped piece of this feature. Keeping it out
of V10 is what keeps V10's diff at zero upstream lines.

## 11. Implementation plan

### 11.1 Phase 0 — spike the primitive (half a day, blocking)

One hard-coded fold over a fixed range in a scratch Markdown buffer, with
`collapsed_text: Some("".into())`, `constrain_width: false`, `render: Empty`.

Answer three questions:

- Does a zero-length placeholder survive the fold tree, or does it desync `tab_map` / `wrap_map` /
  display-point mapping? A zero-length transform output is an unusual state and nothing in-tree
  currently produces one.
- Does click-to-position still land on the right buffer offset either side of the fold?
- Does soft wrap behave?

If any answer is no, fall back to a one-character `collapsed_text` rendered by a zero-width element:
safe for the tree, but each concealed span costs a display column (§10.3), and the fallback adds
roughly a day of cursor-arithmetic fixes. **Everything below is downstream of this answer.**

### 11.2 Phase 1 — headings end to end (one day)

Scanner (headings + fences only), addon, both subscriptions, the fold diff, per-level colours, the
toggle action, the config key. The simplest construct proves the whole loop.

### 11.3 Phase 2 — links and wikilinks (one day)

Two more range producers on the same machinery. Mostly scanner work and tests: aliases, malformed
constructs, links inside code spans, `![alt]` exclusion.

### 11.4 Phase 3 — thematic break and polish (half a day)

The full-width rule element, the vault gate, the keymap entries, and the restored-fold purge
(§10.1).

Total: **3–4 days** if Phase 0 comes back clean, 4–5 if it forces the fallback.

## 12. Open assumptions to confirm on review

- **A1** — Zero-length fold placeholders work. Unverified; §11.1 exists to settle it.
- **A2** — `SelectionsChanged` fires often enough and cheaply enough to drive reveal without a
  debounce. If it needs one, the debounce must be short enough that the markers appear within a
  frame or two of the cursor arriving, or the feature feels laggy.
- **A3** — Three heading colours read as a hierarchy in the default theme without size differences.
  If they don't, the fallback is weight (600 on level 1), not size.
- **A4** — The player palette is a defensible source for heading colours in third-party themes. The
  Day Planner already bets on this; V10 inherits the bet.
- **A5** — Re-applying all highlights on every reparse is cheap enough for a 2,000-line note. If not,
  diff them the way folds are diffed.
- **A6** — Nothing in the agent, backlog, or day-planner paths reads display text rather than buffer
  text. Expected, worth a grep before shipping.

## 13. Decision log (from design discussion, 2026-08-19)

- **Reveal is line-scoped, not construct-scoped.** The original sketch had links reveal per construct
  and headings per line. Unified to per line: one predicate, stable horizontal motion, and no
  ambiguity about which of two links on a line is active. (§4.2, §5)
- **Vault-gated, not editor-wide.** Markdown outside a vault is unaffected, so editing a README or a
  spec in a code repo does not change behaviour. The gate costs nothing and answers "why is this file
  different" for free. (§4.4)
- **Escape hatch is an action *and* a setting.** `thock: show markdown source` for the moment it gets
  in the way, `[markdown] conceal` for the default. (§9)
- **Font size never changes for headings.** Colour only. Varying line height per row is deep
  `element.rs` surgery and is what makes this feature affordable to skip. (§1, §7.1)
- **No new Markdown crate.** `crates/markdown` and `markdown_preview` are read-only rendering; the
  editing surface is `crates/editor`, so V10 is a decoration layer over it. (§3)
- **`___` only for thematic breaks.** `---` is ambiguous with front matter and setext headings;
  `***` is rare. Conceal what was asked for and nothing more. (§6)

## 14. Post-implementation notes (2026-08-19)

The feature shipped as specified, with two deviations the spec left room for:

- **A1 answered: no.** A zero-length fold placeholder produces a zero-output transform that
  underflows the tab map's `TabStops` iterator (`chunk_len - 1` on an empty chunk,
  `tab_map.rs:859`), found by the GPUI tests that stand in for the Phase 0 spike. V10 uses the
  §11.1 fallback exactly as written: a one-character `collapsed_text` rendered by a zero-width
  `Empty` element. Nothing draws where the markers were, but each concealed span costs one
  invisible display column that motions and click-to-position see (§10.3).
- **One upstream line, not zero — and not the one §10.1 predicted.** `HighlightKey`
  (`crates/editor/src/display_map.rs`) is a closed enum, so keyed recolouring (§4.1) needed a
  `ThockMarkdownConceal(usize)` variant added to it. Purely additive and mechanical across
  rebases. The §10.1 fold-persistence hazard was handled entirely Thock-side with mitigation 1:
  every fold apply purges untagged folds that exactly match a conceal span.

Everything else landed per spec: line-scoped reveal driven by `SelectionsChanged` with no
debounce (A2 held), reparse debounced at 50 ms on a background task, folds applied through the
`DisplayMap` (never `fold_creases`), reveal healing after `unfold_all`/`zR` on the next cursor
move, and A6 verified — no Thock panel reads display text. The scanner lives in
`markdown_syntax.rs` with the unit suite; the editor behaviour is covered by GPUI tests in
`markdown_conceal.rs`, including the G3 bytes-untouched assertion.

One addition landed post-review: **go-to-definition on wikilinks**. `g d` / `editor::GoToDefinition`
with the newest cursor anywhere on a `[[wikilink]]` opens the linked note; the addon registers its
own `GoToDefinition` handler (Thock-registered editor actions run before the built-in and propagate
when the cursor isn't on a wikilink, so code-style definitions are untouched). Targets resolve
against the worktree snapshot — an exact vault-relative path (as written or with `.md` appended)
wins, otherwise the first file the bare name reaches, Obsidian-style: a stem matches notes (`.md`)
only, while any other file needs its extension written out (`[[scan.pdf]]`) — with no disk IO and
no file creation for unresolved targets. *Click* navigation stays deferred exactly as §3/§10.6 say.

Two more constructs joined the conceal set afterwards, both on the existing machinery (2026-08-19):
**task checkboxes** (`- [ ]` / `- [x]` draw as a real checkbox, §7.4) and **single-line HTML
comments** (`<!-- … -->` disappears entirely). Comments matter because Thock itself writes them —
the Gmail capture's `<!--gmail:…-->` markers (V9) live at the end of backlog lines, and the Backlog
panel already hides them in its rows; the editor now agrees. Both honour the reveal rule and every
exclusion (C1–C3), and comments became an exclusion zone of their own (C8).

**Strikethrough** joined the set next (§7.5, C9), in the editor and in the Backlog and Day Planner
rows alike, and gave the Day Planner a third item state: a task crossed out rather than ticked
reads as finished in its own dimmer tone.
