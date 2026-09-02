# Thock V16 — Email view: reading synced mail as a conversation

**Status:** Implemented (2026-08-31) — see §14 for deviations; awaiting merge to `main`
**Owner:** Diego · **Date:** 2026-08-31
**Companion docs:** `v10-markdown-conceal.md` (the machinery this rides on), `v15-unified-gmail-sync.md` (§7.1 — the note format this renders), `../VISION.md` (§4.1 Your files forever, §4.8 Everything is editable)

---

## 1. Summary

V15 lands a Gmail thread as one Markdown note: envelope frontmatter, `# Title`, and one
`## Sender — date` section per message. That file is honest and durable — and reads like a YAML
block stapled to a wall of prose. Long threads are the worst case: the reader wants *the newest
message and the shape of the conversation*, and gets forty screens of quoted history.

V16 adds an **email view**: when a note's frontmatter says `source: gmail`, the same buffer renders
as a conversation — machinery folded into an envelope, message headers drawn as a spine of sender
rows, reply bodies collapsible, quoted history collapsed. It is a *reading skin on the V10 conceal
machinery*, not a format: the bytes on disk never change, the reveal rule is untouched, and the
file stays plain Markdown in any other editor.

The trigger is the `source:` value, not a file extension — deliberately. The signal already exists
in every synced note, the vault stays uniformly `.md`, and a future Outlook/IMAP transport joins by
registering its source name, not by inventing a format.

## 2. Goals & success criteria

- **G1** — A synced email note opens as an envelope (From + Open in Gmail) and a conversation
  spine; the newest message is readable immediately, older replies are collapsed to their header
  rows.
- **G2** — Quoted history (`>` runs and their `On … wrote:` attribution lines) is collapsed by
  default; one interaction expands it.
- **G3** — Bytes untouched: `git diff` after a reading session is empty (V10 G3, re-asserted here
  because creases are a new mutation surface).
- **G4** — The reveal rule holds: cursor on a line shows that line's exact source (V10 §5).
- **G5** — Folding works from the keyboard: vim fold commands operate on message creases, and
  named `thock::` actions cover toggle and message-to-message motion.
- **G6** — Graceful degradation: a hand-edited gmail note whose sections don't parse renders as
  plain V10 Markdown. Email view only ever *adds* rendering to constructs it recognizes.

**Success:** reading a 15-reply thread in Thock beats opening it in Gmail.

## 3. Non-goals

- **Rendering HTML mail, attachments, or images.** The body is V15's plain-text reduction; this
  view styles it, nothing more.
- **Composing or replying.** The `link:` line goes to Gmail for that.
- **Variable line heights, fonts, or sizes** — V10's single-line-height constraint stands.
- **A new file extension or format.** Rejected in the V15 follow-up discussion: it fragments
  wikilinks, external editors, and future tooling for a signal frontmatter already carries.
- **Signature trailers** (`-- ` blocks). Same crease treatment fits later; quoted history first.
- **Other mail sources.** The trigger is a registered set of source names; today it contains
  `gmail` only.

## 4. Trigger and gating

During the addon's existing reparse (V10 §8.2), sniff the buffer's frontmatter: a leading `---`
block containing `source: <registered mail source>` switches the buffer's plan to email mode.
No new subscription, no per-keystroke cost — the sniff is a prefix scan of text the reparse
already holds.

Gating stacks on V10's: vault-gated, full-mode singleton editors only, and

```toml
# .thock/config.toml
[markdown]
email_view = true   # default; false renders gmail notes as plain V10 Markdown
```

plus a per-editor `thock::ToggleEmailView` action ("Toggle email view" in the palette).
Conceal off ⇒ email view off; email view is a layer above conceal, never a replacement.

## 5. Rendering

All colours are theme-derived (V4 precedent, V10 §7): no hardcoded hex, headings-style slots via
`highlight_text`.

### 5.1 The envelope

Frontmatter machinery folds away (zero-width placeholder, the V10 marker path): the `---` fences,
`source:`, `capture:`, `captured:`, and `title:` lines (`title` repeats the H1). What remains
renders styled, one line each, reveal rule per line:

- `from:` — key drawn as a muted `From` label, value in the sender colour.
- `link:` — the raw URL concealed, drawn as an `Open in Gmail ↗` label in the link colour
  (LinkLabel slot). Navigation stays whatever links do today; this is presentation.
- `due:` and unknown keys — left as-is, muted key colour. Never hide what we don't understand.

### 5.2 Message rows

Each `## Sender — date` line (V15 §7.1's section header) becomes the conversation's spine:

- `## ` concealed; in its place a small drawn **sender dot** (the checkbox-placeholder precedent).
- Sender text in the sender colour; the ` — date` tail muted.
- Messages whose `From` matches the connected account (§6) use a distinct **own-reply tint** for
  dot and name, so the reader's half of the conversation scans at a glance.
- The blank line *between* messages (guaranteed by the V15 section renderer) draws as a hairline
  rule — the `___` rule-placeholder precedent — so messages separate without any characters
  existing on disk.

A single-message note has no `##` sections (V15 keeps the bare body) and simply gets the envelope
treatment; that is G6 working as intended, not a special case.

### 5.3 Collapsible replies

Each message body — end of its header line to the end of its section — gets a **crease**
(`Crease::inline`, applied with `insert_creases`): a real, user-toggleable Zed fold with a gutter
chevron and a custom placeholder. Collapsed, the header row stays and the body renders as a muted
`⋯ N lines` pill.

- **Default state on open: all but the newest message collapsed.** The envelope, the spine, and
  the latest reply are the first screen.
- The default is applied **once per buffer open**. After that the user's toggles are law: reparse
  and re-render must preserve fold state (the V10 selection-survival discipline applied to
  creases), and the addon never re-collapses a message the user expanded.
- Toggling: gutter chevron, vim `za`/`zo`/`zc`/`zM`/`zR` (creases are folds — these come free),
  and `thock::ToggleMessage` ("Collapse or expand this email reply") for the palette and keymap.

### 5.4 Quoted history

Within a body, a run of `>`-prefixed lines — together with an immediately preceding attribution
line (a line ending in `wrote:` or matching the common `On <date>, <name>` shape) — gets its own
crease, **collapsed by default**, placeholder `⋯ quoted history (N lines)`. This is Gmail's
trellis-dots move and the single biggest cleanup for long plain-text threads. Expanding is one
chevron or `za`; the quoted lines render in the muted quote style V10 already applies to
blockquotes when open.

### 5.5 Navigation

- `thock::NextMessage` / `thock::PreviousMessage` ("Go to the next/previous email reply") move
  the cursor between message header lines; bound as `] m` / `[ m` in the vim context, added to
  both platform keymaps.
- Zed's outline (`cmd-shift-o`) already lists the `##` headers — a free thread index.

## 6. The own-reply tint

The connected account comes from the V13 §7.4 Google settings resolution (`.thock/google.toml`) —
the same value the sync stamps into the `link:` URL. A message is "own" when the `From` header
value contains the account address, case-insensitively. No account resolved ⇒ every sender uses
the plain sender colour; never an error.

## 7. Interactions and hazards

- **Fold persistence (V10 §10.1).** Creases carry our tagging and the register-time purge extends
  to them: restored plain folds matching a message or quote range are dropped before the default
  state is applied. This is the one hazard class that produced an upstream line in V10; assume it
  can again and spike it first (§10, A1).
- **`unfold_all` / `zR`.** Wipes our collapsed state — fine. Creases remain toggleable; the
  default is not re-imposed (§5.3).
- **Editing.** Reveal rule per V10; typing inside an expanded body is plain Markdown editing.
  Zed's fold behavior on edits inside a collapsed range applies unchanged to creases.
- **Search.** Buffer search matches text inside collapsed creases exactly as it matches inside
  any Zed fold today; no new rules.

## 8. Architecture

The V10 split, extended — pure scanner, GPUI addon:

| File | What |
|---|---|
| `crates/thock/src/markdown_syntax.rs` | pure email scanner: `email_plan(text) -> Option<EmailPlan>` — frontmatter sniff, envelope spans, message section ranges, quote runs. Unit-tested with no GPUI. |
| `crates/thock/src/markdown_conceal.rs` | addon: email-mode fold/highlight spans, crease lifecycle (apply, preserve across reparse, purge), default-state-once, actions. |
| `crates/thock/src/thock.rs` | action registration. |
| `crates/thock/src/vault.rs` | `email_view` config key. |
| `assets/keymaps/default-{macos,linux}.json` | `] m` / `[ m` in the vim-mode block, both platforms in sync. |

`conceal_spans` stays universal; `EmailPlan` is a second product of the same parse text, only
computed when the sniff hits. Expected upstream diff: **zero lines**, with A1 the one risk.

Estimated ~600 lines including tests.

## 9. Implementation plan

1. **Phase 0 — spike (blocking):** creases and conceal folds coexisting on the same buffer — a
   crease whose range contains concealed spans, chevron visible, placeholder correct, persistence
   purge covering both. This is where V10's spike found its surprises; expect the same.
2. **Phase 1 — envelope and spine:** email sniff, envelope folds/styles, message rows with dot,
   tint, and rule. No creases yet — this phase is V10 machinery only and shippable alone.
3. **Phase 2 — creases:** reply folding, default state, state preservation, `ToggleMessage`,
   vim fold verification.
4. **Phase 3 — quoted history and motions:** quote-run detection and creases, `NextMessage` /
   `PreviousMessage`, keymap entries.

## 10. Open assumptions to confirm

- **A1** — Creases and V10 conceal folds compose on the same lines without display-map conflicts
  (Phase 0's whole job).
- **A2** — Crease fold state survives `remove_creases` + `insert_creases` across a reparse, or
  can be carried over by diffing rather than replacing.
- **A3** — The register-time fold purge distinguishes restored-plain-fold from user fold reliably
  for crease ranges.
- **A4** — A 40-message thread parses and renders within the existing 50 ms debounce budget
  (line-based scanner; expected trivially yes).

## 11. Configuration summary

| Surface | Default |
|---|---|
| `[markdown] email_view` | `true` |
| Open state | newest message expanded, older collapsed (§5.3) |
| Quoted history | collapsed (§5.4) |
| Own-reply tint | on when an account resolves (§6) |

## 12. Out-of-scope follow-ups noted during design

- Appending late replies to an already-captured thread (sync-side; needs per-message tracking
  instead of per-thread digests — V15 §9's digest space is per-thread).
- Signature-trailer creases (`-- ` blocks).
- Other mail sources joining the trigger set.

## 13. Decision log (2026-08-31, with Diego)

- **Default fold state:** older replies collapsed, newest expanded — over all-expanded and
  all-collapsed.
- **Quoted history:** auto-collapse by default — over visible-but-foldable.
- **Envelope:** fold machinery keys only, keep styled From/link lines — over one whole-block
  envelope fold and over leaving frontmatter raw.
- **Own-reply tint:** in for V16 — worth reading the account from `.thock/google.toml`.
- **Trigger:** `source: gmail` frontmatter, no new extension (carried in from the V15 follow-up
  discussion that motivated this spec).


## 14. Post-implementation notes (2026-08-31)

Shipped on the plan of §8, with the Phase 0 answers and four deviations:

- **A1 held.** Creases and conceal folds compose: a reply crease wraps rows
  full of conceal folds, `fold_at`/`unfold_at` use it from the gutter, and
  vim's `za` works because the reveal rule drops the header row's conceal
  folds the moment the cursor arrives — so `is_line_folded` reads the body
  fold's true state.
- **The heal got better, not worse.** There is no editor event for line-fold
  changes (`BufferFoldToggled` is multibuffer-only) and the `DisplayMap`
  never notifies, so the addon observes the editor entity itself: any fold
  wipe (`unfold_at`, `zR`, gutter click) heals on the same effect flush, and
  V10's heal-on-next-cursor-move became heal-immediately. Terminates because
  a healed apply is a no-op that doesn't notify.
- **The fold diff learned placeholders.** Toggling the email view gives the
  same `## ` range a different placeholder (marker space vs sender dot), so
  the conceal diff now compares `collapsed_text` as well as range.
- **The inter-message rule was dropped.** §5.2's hairline on the blank line
  between messages has no honest primitive: the blank line has no characters
  to fold, and folding its newline merges the header into the previous row.
  The collapsed spine already separates messages by header rows; revisit
  with `Crease::inline`'s `render_trailer` if the expanded view needs it.
- Two small renderings differ from §5.1's letter: the `from:` key shows as
  the muted literal `from:` (not a re-labeled `From`), and expanding a reply
  re-folds any quote crease inside it rather than remembering the quote's
  own toggled state — the quote default is cheap to re-impose and the case
  is marginal.
- `thock::ToggleMessage` is palette-only (with `za` covering vim); `] m` /
  `[ m` bind `NextMessage` / `PreviousMessage` in both platform keymaps
  under `Editor && ThockMarkdownConceal && vim_mode == normal`, shadowing
  vim's method motions only inside vault notes.
