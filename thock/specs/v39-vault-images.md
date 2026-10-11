# Thock V39 - Images in the vault: one folder, rendered on the desk, captured on the phone

**Status:** Implemented (2026-10-10): the desk editor, the sync, and the phone's capture, edit and
share paths. Still to come: the phone drawing pictures in its editor, and pulling binary snapshots.
**Owner:** Diego · **Date:** 2026-10-10
**Design reference:** the design notes with the editor mockups, the sync sequence and the decision
cards this spec resolves: <https://claude.ai/artifact/Y81PSwkFpv7zxHtzwi9MVE>.
**Companion docs:** `v10-markdown-conceal.md` and `v24-inline-markdown-conceal.md` (the conceal
machinery the editor half rides), `v13-inbox-routine.md` (the inbox note an image lands in),
`v33-iphone-companion.md` §14 (the write contract this extends), `v34-vault-sync.md` and
`v34-vault-sync-api.md` §3.5, §4.1, §7 and §9 (the allow-list, the write kinds and the fixture
corpus this adds to), `v40-phone-inbox-gestures.md` (ships the second new write kind on the same
sync-core change)

---

## 1. Summary

A vault holds words and nothing else. A whiteboard photo, a receipt, a screenshot of the thing the
note is about: today the person can keep the words and not the picture. V39 gives the vault one
folder for images, `images/`, a link format that survives every move a line makes between notes,
and three surfaces that honour it:

- **The desk editor** draws an image under the line that links it, on the V10 conceal machinery:
  the link's syntax folds away while the cursor is elsewhere and comes back when the cursor lands on
  the line; the picture stays either way. Typing `![` opens the same completion menu `[[` opens,
  listing the images folder.
- **Sync** carries images both ways. Down, as binary snapshots the desk publishes like any other
  file; up, as one new write kind, `put_file`, that the phone queues ahead of the note that links it.
- **The phone** attaches a photo while capturing, while editing a waiting inbox note, and from the
  share sheet, which today keeps links and nothing else.

The one real file-format commitment is the link: `![alt](/images/<name>)`, vault-root paths with a
leading slash. Thock moves lines between files all day (inbox to daily, daily to backlog, backlog to
daily); a path relative to the note breaks the moment a line changes depth, and a leading slash is
what Zed's own preview, GitHub, VS Code and Obsidian all read as "from the root".

## 2. Goals & success criteria

- A note with `![whiteboard](/images/2026-10-10-0931-whiteboard.jpg)` shows the picture under that
  line on the desk, at the editor's width or smaller, and the line reads as its alt text until the
  cursor lands on it. Bytes on disk never change (V10 G3).
- Typing `![` in a vault note lists `images/` fuzzily; accepting writes the whole link.
- A photo attached on the phone is a file in `images/` on the desk, linked from the note that was
  captured, after one sync round. Nothing else in the note changes.
- An image shared to Thock from Photos or Safari becomes one inbox note with the picture under its
  title, after one sync round.
- A `.jpg` dropped into `images/` at the desk by hand syncs like a note, as long as it is under the
  size limit; a bigger one stays local and the Phone row says so.
- A desk older than the phone holds a `put_file` write and everything behind it until updated
  (V34 API §10.4), so no tester loses an image to a version skew.
- No upstream file changes. The editor's block API is public; the rest is the Thock crate.

## 3. Non-goals

- **Pasting or dropping an image into the editor** to copy it into `images/`. Natural next slice;
  nothing here closes the door.
- **The phone drawing images** in its own editor, and **pulling binary snapshots** to do so. In V39
  the phone shows an image line as literal text and keeps only the images it captured itself.
- **Image editing** of any kind: no crop, no rotate, no annotate. The phone downsizes, nothing more.
- **PDFs, audio, video, or any attachment that is not an image.** The allow-list grows by five
  image extensions and stops there.
- **A separate attachment store or route** on the Plus backend. Images are files in the vault and
  travel as files.
- **Obsidian `![[embed]]`** stays unrecognised, as V10 decided.

## 4. The folder and the link

### 4.1 The folder

`images/` at the vault root, created when the first image lands (desk insert or phone write), never
by a Routine: nothing in the vault should assume a Routine exists. The folder is configurable:

```toml
[images]
dir = "images"
```

The desk reads it as `VaultConfig.images.dir` (default `images`); the phone reads the same key as
`VaultConfig.imagesDir`. `AGENTS.md`'s map gains one line so an agent knows where a picture goes.

Files are named like inbox notes, so a folder listing reads as a timeline:

```
images/2026-10-10-0931-whiteboard.jpg
images/2026-10-10-0931-whiteboard-2.jpg     # same minute, same slug: suffixed
images/2026-10-12-1804-screenshot.png
```

`<YYYY-MM-DD>-<HHmm>-<slug>.<ext>`: the slug is the source file's stem, lower-cased, non-alphanumerics
folded to `-`, at most 40 characters, or `photo` when there is no name (the camera, a pasted image).
A collision takes `-2`, `-3`, … in the writer's own view of the folder; `put_file` never overwrites
(§6.2), so a collision between two devices costs one skipped write, not a replaced picture.

### 4.2 The link

```markdown
![whiteboard](/images/2026-10-10-0931-whiteboard.jpg)
```

Standard Markdown image syntax, alt text first, a vault-root path with a leading slash. The alt text
defaults to the slug and is the person's to change; it is what the desk shows while the syntax is
concealed. Every writer in Thock (the desk's insert, the phone's capture, the share extension, a
skill that is told to attach a picture) writes exactly this shape.

### 4.3 Resolution on the desk

The resolver is forgiving about what it reads, strict about what it writes:

| `src` | Resolves against |
| --- | --- |
| `/images/x.jpg` (leading slash) | the vault root |
| `images/x.jpg` (bare, first segment is the configured folder) | the vault root |
| any other relative path | the note's own folder |
| `https://…` | fetched; Readwise already writes `![rw-book-cover](https://…)` |
| `data:` | not rendered; the line shows its source |

Percent-encoded paths are decoded first. A path that resolves outside the vault is not rendered.

## 5. The desk editor

### 5.1 What it draws

Two states, both on the V10 machinery:

```
cursor elsewhere                          cursor on the line
───────────────────────────────────────   ───────────────────────────────────────
Sketched the sync flow:                   Sketched the sync flow:
whiteboard                                ![whiteboard](/images/2026-10-10-0931-whiteboard.jpg)▌
┌───────────────────────────┐             ┌───────────────────────────┐
│                           │             │                           │
│        (the image)        │             │        (the image)        │
└───────────────────────────┘             └───────────────────────────┘
- [ ] Send her the spec                   - [ ] Send her the spec
```

- The image is an editor **block** placed below the line (`BlockPlacement::Below`), rendered with
  gpui's `img`, scaled to fit the block's width, at most about twenty rows tall, aspect preserved.
  Its height is whole rows; the block is inserted at a placeholder height and resized once the image
  has decoded.
- The link's syntax is a **fold** like any other V10 markup: `![` … `](…)` collapse to the alt text,
  coloured as an internal link (V10 §7.2). An image line with empty alt text shows the file's stem.
- A fold placeholder renders inline at line height and cannot grow a row (V10 §14), which is why the
  picture is a block and the syntax is a fold, two primitives on one line.

### 5.2 The reveal rule, extended

V10 §5 stands: the cursor's line shows its exact source. The image block is **not** part of that
rule: it stays while the cursor is on the line, so editing the alt text never makes the page jump.
It leaves only when the line stops being an image line (the syntax is broken by an edit, after the
reparse debounce) or when markdown source is toggled on for the editor, which hides every block
along with every fold.

### 5.3 Missing and remote images

- **The file is not there**: no block. The fold shows the alt text with a muted "not found" suffix,
  so the line reads *whiteboard · not found* and the source is one cursor move away. A vault is
  hand-editable and half-moved folders are normal; a missing picture is an empty state, not an error.
- **Remote (`https://`)**: fetched through gpui's image resource loader and cached for the editor's
  lifetime. While loading, and when the fetch fails, the line behaves as missing. Nothing is written
  to the vault; a remote image is never downloaded into `images/`.
- **Too large to decode**: as missing. The decode happens off the foreground thread; a block never
  waits on it.

### 5.4 Inserting an image

Typing `![` in a vault note opens the completion menu listing every file under the images folder
with an allowed extension, newest first, through the same `CompletionProvider` that serves `[[`
(`wikilink_completion.rs`). The editor's own fuzzy matching filters as the person types after the
bracket. Accepting an item replaces `![query` with the whole link, alt text set to the slug, and
consumes a `]` autoclose already added, as the wikilink path does. The list is read from the
worktree snapshot in memory; no index, no disk walk.

`thock: insert image` (`thock::InsertImage`, unbound by default) does the same from the command
palette or a binding: it inserts `![` at the cursor and shows completions. A note-taker finds it by
name; a keyboard user binds it.

### 5.5 Architecture

- `markdown_syntax::conceal_spans` gains `SpanKind::Image { alt: Range, src: Range }` for a line
  whose only content is one image link, optionally surrounded by whitespace. An image inside a
  paragraph or a list item stays literal in V39: the block would land under the wrong line.
  The four scanner tests that assert `![…](…)` is ignored flip to assert the new kind.
- `MarkdownConcealAddon` gains `image_blocks: HashMap<Anchor, (CustomBlockId, ImageKey)>`, diffed
  on every reparse the way crease ids are: a new image line inserts a block, a removed one removes
  it, a changed `src` replaces it. `SelectionsChanged` does not touch blocks; reveal is folds only.
- Resolution (§4.3) is a pure function `resolve_image_source(src, note_dir, vault_root, images_dir)
  -> Option<ImageSource>`, unit-tested without gpui.
- The block's render closure holds an `Arc<Path>` or a URI and the editor's `RetainAllImageCache`,
  so a scroll past the same picture never decodes twice. The decoded size comes back through the
  image cache; the addon then calls `resize_blocks`.
- Nothing upstream: `insert_blocks`, `replace_blocks`, `resize_blocks` and `remove_blocks` are
  public on `Editor`, and `HighlightKey::ThockMarkdownConceal` already exists.
- One trap, found in implementation: the block map drops any block whose row *begins inside a
  fold*, and the `![` fold would begin at column 0. So the `!` is not folded; it stays in the
  buffer's display painted in the editor's background colour (a fourteenth highlight slot), and the
  fold starts at the `[`. The line costs one more invisible column than a wikilink does. The
  alternative was a rule change in `block_map.rs`; it stays in the Thock crate instead.

### 5.6 Configuration and actions

- `[markdown] conceal = false` turns image rendering off with the rest of conceal; **thock: toggle
  markdown source** does so per editor. There is no separate switch: a picture is markup rendered.
- `thock::InsertImage` as in §5.4. `g d` and go-to-definition on an image line open the file in
  Zed's image viewer, through the same path a wikilink takes to its note.

## 6. Sync

The protocol's invariant stands: snapshots flow from the desk, writes flow from the phone (V34 API
§1). Images join both flows without a new route.

### 6.1 Binary snapshots, down

V34 drew the allow-list at text, on purpose (V34 §3, §5). V39 widens it by exactly this much: a path
whose first segment is the images folder and whose extension is one of `png`, `jpg`, `jpeg`, `gif`,
`webp` is syncable. The rule is folder-and-extension, not extension alone, so a `.png` dropped
anywhere else stays local and the boundary is still legible in one sentence.

- The desk's scan includes the folder and skips the UTF-8 check for binary paths. The 2 MB plaintext
  limit holds; a larger image is listed with the other too-large files and the Phone row names the
  count.
- The envelope is unchanged: a file snapshot is "the file's bytes, exactly as on disk" (V34 API
  §5.2) and always was.
- The backend's path check gains the same rule. The server stores what it cannot read, as before.
- The phone's pull skips binary paths in V39 (non-goal); its full pull does not delete local images
  it captured itself, because those live outside the `files` table (§7.4).

### 6.2 `put_file`, up

```json
{ "kind": "put_file",
  "path": "images/2026-10-10-0931-whiteboard.jpg",
  "content_base64": "/9j/4AAQ…",
  "content_hash": "ab…64 hex" }
```

- `path` must satisfy §6.1; any other path is refused on the desk and logged, never applied.
- `content_hash` is SHA-256 over the decoded bytes, the same primitive as snapshot hashes (V34 API
  §7.4). It lets the desk verify the decode and lets `effect_present` be a hash comparison.
- The decoded payload must fit the 2 MB write limit (V34 API §3.5), so bytes are at most about
  1.5 MB. The phone targets 1 MB (§7.1); the desk refuses over the limit.

### 6.3 Rules

| Case | Outcome |
| --- | --- |
| No file at `path` | bytes written through the project `Fs`, parent folders created; `applied` |
| A file at `path` with the same hash | nothing changes; `noop` |
| A file at `path` with a different hash | nothing changes; `noop`, logged. A `put_file` never overwrites: images are immutable and a new picture is a new name |
| `content_base64` does not decode, or its hash is not `content_hash` | refused: skipped and acked (V34 API §10.4) |
| `path` fails §6.1 | refused: skipped and acked |

- **`effect_present`**: a file exists at `path` whose SHA-256 is `content_hash`.
- **Order**: the phone queues `put_file` before the note write that links it, in one flush, so the
  desk drains the bytes first and the link never points at a file that is still in the queue.
- **Checkpoint**: the desk takes its V2 checkpoint before the batch as it does today, so a picture
  is one restore away from undone like any note.
- **Rebase** (V34 API §8.4): `put_file` has no text to rebase against; a pending one is kept until
  acked and pruned on ack.

### 6.4 Where it lives

`crates/thock-sync-core` (`write.rs`, `apply.rs`) and `ThockKit/SyncCore` (`WriteDocument.swift`,
`Apply.swift`), with `fixtures/v1/put_file/` that both runners pass. The fixtures pin the document
shape and that the text applier hands a note back untouched; what the kind does to files is each
store's own tests, since the corpus speaks text. A new `is_syncable_image_path(path, images_dir)`
beside `is_syncable_path` carries the folder-and-extension rule, with vectors under `images` in
`fixtures/v1/paths.json`. The backend accepts the five extensions anywhere (it stores what it cannot
read); the desk is the party that keeps pictures to the folder, both for what it uploads and for the
`put_file` writes it applies. `WRITE_VERSION` stays 1: an older desk holds the kind (V34 API §10.4).

The V34 API amendments (§3.5 the binary size note, §4.1 the path rule, §7.3 the kind, §9.2 the
area and the base64 fields, §13 the changelog) land with the sync-core change, not with this spec,
so they merge after the in-flight hold-unsupported-writes change they build on.

## 7. The phone

### 7.1 Capture

A photo button beside the Today / Inbox / Backlog chips opens the system photo picker, with the
camera one tap away. Chosen pictures show as thumbnails above the editor, removable with a tap.
Save writes, in one flush and in this order:

1. one `put_file` per picture, downsized to at most 1 600 px on the long edge, JPEG at quality 0.8,
   quality stepped down until the file is under 1 MB; HEIC is converted, PNG screenshots stay PNG
   when under the limit;
2. the note write the chip already makes, with one `![slug](/images/<name>)` line per picture
   appended to the body after a blank line. For Today and Backlog, where the capture becomes a task
   line, the image lines are indented as the task's continuation, so they travel with the block.

The capture's digest (V13 §4.3) is over the text, unchanged by the images; receipts work as before.

### 7.2 Edit

The same button in the inbox edit sheet. The image lines join the section that `replace_section`
rewrites, after the existing body, and the `put_file`s go first in the same flush.

### 7.3 Share

`ThockShare` keeps links today and says "there is no link here to keep" for anything else. V39 adds
the image activation rule (up to four pictures) beside the link rule. Each attachment is loaded as a
thumbnail through `CGImageSource` at the capture size, never as a full `UIImage`, because the
extension's memory ceiling is a fraction of the app's. The clip sheet shows the thumbnails and a
text field for a title and a line; Save writes the `put_file`s and one inbox note whose title is the
text's first line or `Photo` and whose body is the image lines. A share with a link **and** a
picture stays a link clip; the picture is dropped and the sheet says so.

### 7.4 Local store

The phone keeps the images it captured in a `blobs` table beside `files` in the app-group store
(path, bytes, hash), written by the same `record` that queues the write, so the share extension and
the widgets see them too. They are not snapshots: a full pull leaves them alone, and `prune` drops a
blob once its `put_file` is acked and the phone has no further use for it (V39 has none; the row is
kept for receipts' thumbnails and swept with the capture record).

### 7.5 What the phone shows

An image line is literal text in the phone's editor (`EditorTests.swift` keeps asserting it). The
capture and edit sheets show thumbnails from the blobs table; receipts show a small picture mark on
a capture that carried one. Drawing images in the note is the follow-up, with pulling binary
snapshots.

## 8. Tests

- **Scanner** (`markdown_syntax.rs`): an image-only line yields `Image { alt, src }` with the right
  ranges; an image inside a paragraph, a list item, a fence or front matter stays literal; empty alt;
  percent-encoded `src`.
- **Resolution**: pure cases for every row of §4.3, including a `..` path and a path outside the
  vault.
- **Editor** (gpui, on the existing harness): after `settle`, one block per image line and the
  display text shows the alt; moving the cursor onto the line reveals the source and keeps the block;
  breaking the syntax removes the block after the debounce; a missing file renders no block and the
  not-found suffix; toggling source hides and restores blocks; the buffer is never modified.
- **Completion**: `![` starts a query, `![[` does not; accepting writes the full link and consumes
  an autoclosed `]`.
- **Sync core**: `put_file` fixtures for every row of §6.3; `paths.json` cases for the folder rule;
  both runners.
- **Desk sync** (gpui): a `put_file` lands bytes through `Fs` and the next scan uploads the file as
  a binary snapshot; an over-limit image is listed, not uploaded; a `put_file` for an existing path
  with other bytes leaves it alone.
- **Phone** (ThockKit): the capture builders emit `put_file` before the note write; the downsizer
  lands under 1 MB from a 12 MP input; the share extension's draft parses an image item; receipts
  carry the picture mark.
- **Backend** (Go): the path rule for `images/x.png` and `notes/x.png`.

## 9. Delivery

1. This spec, `v40-phone-inbox-gestures.md`, the V33 §14 amendment, the `AGENTS.md` map line and
   the roadmap entries.
2. **Desk render and insert** (§4, §5): pure Thock crate, no protocol change, value on its own.
3. **Sync core** (§6, with V40 §6): both kinds, fixtures, desk apply, backend path rule, the V34 API
   amendments. After the hold-unsupported-writes change merges.
4. **Phone** (§7): capture and edit sheets, then the share extension.
5. Later: the phone draws images and pulls binary snapshots; the desk accepts a pasted or dropped
   picture.

## 10. Decision log (2026-10-10)

1. **Root-relative links with a leading slash** (`![alt](/images/…)`), over note-relative paths
   (the most standard Markdown, broken by every line move between folders) and Obsidian embeds
   (rendered by Obsidian and Thock only).
2. **Extend the existing sync** (binary snapshots down, `put_file` up), over a separate attachment
   route (clean, but new surface in three implementations) and phone-local images for now (breaks
   the vault-is-a-folder rule).
3. **The image stays visible while the cursor is on its line.** The syntax reveals like any link;
   the block does not jump away while the alt text is being edited.
4. **Folder-and-extension**, not extension alone, for the binary allow-list, so the text boundary
   V34 drew stays explainable in one sentence.
5. **No overwrite, ever**, for `put_file`. A picture is immutable; a collision is a skipped write.
