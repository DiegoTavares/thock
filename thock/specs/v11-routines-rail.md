# Thock V11 — Places and Verbs in the Routines rail

**Status:** Shipped (2026-08-19)
**Owner:** Diego · **Date:** 2026-08-19
**Companion docs:** `../VISION.md` (§4.5 Everything is editable, §4.6 Modular life, §5 The product experience), `v7-dynamic-routines.md` (the `routine.toml` schema this extends), `v5-agent-and-onboarding.md` (onboarding state, the "Finish setup" signal)

---

## 1. Summary

The Routines panel is the first thing a vault opens on, so the shipped **Daily & Weekly** Routine sets
the expectation for every Routine after it. V7 gave it a shape borrowed from a file tree: a
collapsible Routine parent, its links, and then a **Skills** row that looks like a folder holding
five children. Three problems follow from that shape:

- **"Skills" is a folder that isn't one.** A tree affordance implies files in a directory. It is a
  category label, and it costs a row plus an entire indent level.
- **One-time setup sits among daily verbs.** *Connect Google Workspace* and *Set Up Timeline* run
  once, ever, and hold permanent residence in the most valuable rows in the app.
- **Everything has the same weight.** *Today* is opened forty times a week and *Last Week* twice;
  both are a 12px row with a small icon. So is "ask an agent to rewrite your day".

The root cause is in the manifest, not the panel: `routine.toml` has no way to say that a skill is a
one-time chore or that a link is an archive. The panel can't distinguish what the schema can't
express.

V11 adds the two smallest keys that fix that — `[[skill]] kind` and `[[link]] group` — and rebuilds
the section around them: two captioned zones (**Notes**, **Rituals**) instead of a fake folder, one
collapsed row per demoted group, setup behind a collapsed **Setup** row, keybindings rendered on the
rows they belong to, and removal moved off the header until the pointer is on it.

Both keys are optional with defaults that reproduce V7 behaviour, so a `routine.toml` written last
week keeps rendering exactly as it did.

## 2. Goals & success criteria

- **G1** — The shipped Daily & Weekly section shows no row that isn't either a place worth opening
  today or a verb worth running today. One-time steps are reachable but not resident.
- **G2** — No row in a Routine section is a category label. Zone separators take no keyboard stop and
  cannot be collapsed by accident.
- **G3** — Maximum indent for a top-level row drops from 2 to 1. Nesting appears only where the user
  opened a group.
- **G4** — A bound link or skill shows its chord on its own row, so binding is discovered by using
  the panel rather than by reading `ROUTINES.md`.
- **G5** — Every existing `routine.toml`, including hand-written ones in a user's vault, renders
  without change or warning.
- **G6** — Keyboard parity is preserved and extended: the new group rows are selectable, open and
  close from the keyboard, and closing a group never drops the cursor somewhere the user wasn't.

## 3. Non-goals (explicitly out of V11)

- **A scoped picker or palette for Routine items.** (Direction 02 of the design deck.) The rail stays
  the index.
- **A hero surface for Today with a parsed task tally.** (Direction 03.) It needs the panel to read
  note contents on every vault event; worth doing, worth doing separately.
- **Per-panel single-key addresses** (`1`, `2`, `w`). (Direction 04.) A layer over whichever list
  shape wins, not a replacement for it.
- **Time-aware suggestion windows.** (Direction 05.) Held until the Routines have proven their
  rhythms in real use.
- **Completion tracking for setup steps.** The Setup row is always available while setup skills
  exist; only onboarding has a persisted done state, and that already drives the header chip.

## 4. The section shape

One section per Routine, and within it, in order:

1. **Notes** caption.
2. Every link without a `group`, in manifest order.
3. One collapsed disclosure row per link `group`, in first-appearance order, labelled with the group
   name. Expanding nests its members one level in.
4. **Rituals** caption.
5. Every skill with `kind = "ritual"` (the default), in manifest order.
6. A collapsed **Setup** row when the Routine has `kind = "setup"` skills, same shape as a link
   group.

The two captions appear only when a section holds both links and skills. With nothing to separate,
a caption is decoration, and a Routine that is purely a set of bookmarks should render as a set of
bookmarks.

Applied to the shipped manifest, the section goes from twelve rows across three indent levels to
eight rows across one:

```
⌄ ◷ Daily & Weekly                    Finish setup
  Notes ───────────────────────────────────────
    ▤  Today                                ⌃⌥T
    ▤  This Week
    </> Weekly Dashboard
    ›  Older notes
  Rituals ─────────────────────────────────────
    ▸  Wrap Today                           ⌃⌥⏎
    ▸  Wrap Yesterday
    ▸  Week Review
    ›  Setup
```

Three details carry that layout, and all three were wrong in the first build:

- **A caption is a rule, not a row.** The label is followed by a hairline running to
  the right edge (`ListSubHeader`'s end slot), so a caption reads as a separator between two zones
  rather than as another entry in the list.
- **A group's chevron sits in the icon column.** Putting it in `ListItem`'s own disclosure column —
  where the Routine header's chevron lives — hangs the group off to the *left* of the rows it
  contains, which says "sibling of the section" when it needs to say "member of this zone". In the
  start slot it lands exactly where its siblings' icons do.
- **The right column belongs to shortcuts.** A collapsed group's member count was tried there and
  removed: the chevron already says something is behind the row, and mixing a count into the column
  that otherwise holds chords makes both harder to read.

## 5. Schema additions

Both keys are optional. Both defaults are the current behaviour.

```toml
[[link]]
name  = "Plan 2025"
open  = "finance/plan_2025.md"
group = "Past years"     # demoted into a collapsed row with this label

[[skill]]
id   = "connect-monarch"
name = "Connect Monarch"
kind = "setup"           # "ritual" (default) | "setup"
```

- An unknown `kind` warns and falls back to `ritual`, matching how `LinkKind` already handles a bad
  `kind` on a link. A typo must never make a skill vanish from the panel.
- `kind = "ritual"` is not written back by `render_manifest_toml`, so a definition that migration
  re-renders keeps reading like the hand-written one.
- Groups are per-Routine and per-zone: link groups sit at the end of Notes, setup at the end of
  Rituals. A group has no nesting of its own.

## 6. Places and verbs, made visible

The distinction only works if the two classes don't look alike:

- A ritual's default icon is the **run** glyph (`play_outlined`) in the accent colour; a setup step's
  is `settings`, muted. Links keep their kind-derived icons, muted. A manifest `icon` override still
  wins on the glyph — the colour stays, because the colour is what encodes the class.
- The **row runs the ritual**. Clicking it, or pressing `enter` on it, launches the skill. That is a
  change from V7, where the row opened the skill's Markdown and a hover button ran it; the primary
  action now matches the primary affordance.
- Reading a ritual's instructions moves to its own affordance: a document button on hover, and
  `alt-enter` (`g space` in vim mode) from the keyboard. Skills stay inspectable — VISION §4.5 — they
  just aren't what a click does anymore.

## 7. Keyboard

Existing bindings are unchanged; `j`/`k`, arrows, `g g`, `shift-g` and `enter` all behave as before,
now walking the new row list.

| Key | vim | Action |
| --- | --- | --- |
| `enter` | `enter` | Open a link, run a ritual, toggle a group |
| `alt-enter` | `g space` | `thock::ViewSkill` — open the selected ritual's instructions |
| `right` | `l` | `thock::ExpandGroup` |
| `left` | `h` | `thock::CollapseGroup` |

`CollapseGroup` acts on the selected group row *or* on the group the selected row lives inside, then
parks the cursor on that group's row. Collapsing from inside a group therefore leaves the selection
where the user was looking rather than at whatever index happens to survive the list shrinking. Both
actions are no-ops elsewhere; they deliberately do not collapse the whole Routine section, because
there is no sensible place to leave the cursor when a section's rows all disappear.

Bound rows render their chord flush right via `ui::KeyBinding`, which renders as nothing when the
action is unbound — so the chip appears the moment a user adds a binding, and no row needs to know
whether one exists.

## 8. Architecture

Everything is in `crates/thock/`; the upstream diff is three keymap blocks
(`assets/keymaps/default-macos.json`, `default-linux.json`, `vim.json`), each an additive context
block alongside the existing Thock ones.

- `routines.rs` — `SkillKind { Ritual, Setup }` on `RoutineSkill`, `group: Option<String>` on
  `RoutineLink`, parsing, warnings, and `render_manifest_toml` round-trip.
- `routines_panel.rs` — the section walk and the row renderers.

### 8.1 One walk, two consumers

The cursor is a flat index into visible rows, and the renderer walks the same section to draw it. In
V7 those were two hand-kept-in-sync loops; adding captions (not selectable) and groups (selectable,
and changing the row count when toggled) makes drift a matter of when, not if.

So both derive from one function:

```rust
fn section_items(manifest: &RoutineManifest, expanded_groups: &HashSet<String>) -> Vec<SectionItem>
```

`nav_rows()` is that list minus its captions; `render_routine_section` renders all of it and
increments the row index only on non-caption items. The invariant — *every selectable row is an
actionable item, in rendered order* — is a unit test, and `section_items` is a free function
precisely so it can be one without standing up a workspace.

### 8.2 Group state

`expanded_groups: HashSet<String>` on the panel, keyed `"<routine id>\u{1}<label>"` — the separator
is a character no TOML label can carry. Groups start collapsed. Like `collapsed_routines`, the set
survives re-render and vault refresh, so a group the user opened stays open when a file event
reloads the manifests.

## 9. The header

- The trash button moves to `end_slot_on_hover`. Removal is destructive and rare; it should not be
  the most prominent control on a Routine.
- The `end_slot` it vacates carries the **Finish setup** chip while onboarding is pending (V5 §7.3),
  which is state the user can act on rather than an operation they can regret.

## 10. Migration

The shipped Timeline manifest moves to `version = 8` with *Yesterday* and *Last Week* in an
`"Older notes"` group and both setup skills marked `kind = "setup"`. Vaults whose
`routines/timeline/routine.toml` still hashes to what was installed get the update automatically
through the existing lockfile path; a user-edited definition is left alone and logged, exactly as
V7 specified. Either way the panel renders — an unclassified skill is a ritual and an ungrouped link
is a primary row.

## 11. Decision log

1. **Setup gets a collapsed row, not pure eviction.** The design deck put setup entirely behind the
   header chip. But only *onboarding* has a persisted completion state; a Routine can ship other
   one-time steps (Connect Google Workspace) with no signal that they're done. Hiding those behind a
   chip that vanishes on an unrelated state change would make them unreachable. One always-present,
   always-collapsed row is the honest version, and it still costs one row instead of two.
2. **`group` carries its own label rather than a `"secondary"` enum plus a manifest-level name.**
   Self-documenting in the TOML, no second key to keep in sync, and a Routine can have more than one
   demoted group without any new schema.
3. **Wrap Yesterday stays a visible ritual.** It could go in a group, but a group of one labelled
   "Older notes" inside the Rituals zone reads worse than the row it replaces.
4. **`enter` runs a ritual instead of opening it.** Rituals are the reason skills exist; opening the
   Markdown is the inspection path, not the default one. The inspection path keeps a hover button and
   a keybinding, so nothing became unreachable.
5. **Captions use `ListSubHeader`, not a bespoke label-plus-hairline.** It is the existing Zed idiom
   for exactly this, and it inherits the theme for free. The rule goes in its end slot rather than
   into a hand-rolled `h_flex`.
6. **Captions stay sentence case.** The design mockup set them as letter-spaced small caps, which
   GPUI's text styling has no way to express; literal `NOTES` without the tracking reads worse than
   the mockup, not better, so the rule carries the separation instead.
