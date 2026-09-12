# Thock V26 — Planner heading resolution & an actionable sync hold

**Status:** Implemented (2026-09-10)
**Owner:** Diego · **Date:** 2026-09-10
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.4 Human-in-the-loop, §4.5 Everything is editable), `v4-day-planner-panel.md` (the parsing model), `v8-calendar-sync.md` (§5.1 placement rules, §10.3 the status row this extends), `v19-vault-language.md` (`[day_planner] heading` as configurable text)

---

## 1. Summary

Editing the daily template silently breaks calendar sync. `## Day planner` is matched by **exact
string equality after lowercasing**, so `## 📅 Day planner`, `## Day planner:`, `## **Day planner**`
and `## Day-planner` all fail to resolve. The reconciler then reports `NoPlannerSection`, the
service holds, and the Day Planner panel keeps rendering tasks — because *it* falls back to parsing
the whole note when the heading is missing (V8 §11.1). Nothing looks broken; meetings just quietly
stop arriving.

V26 fixes both halves:

1. **Resolution (§5–§6)** — headings are matched on a **normalized key** (inline Markdown removed,
   lowercased, every non-alphanumeric character folded to a space), after an exact pass. Decoration
   stops mattering. `[day_planner] heading` additionally accepts a **list**, so a renamed or
   translated heading is one config line rather than a broken vault.
2. **An actionable hold (§7–§8)** — the hold stops being a muted sentence. `SyncState::Holding`
   carries a structured [`HoldReason`], and the Day Planner's status row turns the two structural
   ones into a named problem plus the buttons that fix it: **Add heading** writes the heading into
   today's note, **Use another heading…** picks one of the note's existing headings and writes it to
   `[day_planner] heading`.

The invariant from V8 §5.1 rule 4 stands: Thock still never invents a heading on its own. It now
*asks* instead of holding in silence.

## 2. Goals & success criteria

- **G1** — A template whose planner heading gained an emoji, a colon, bold markers, closing hashes,
  or a hyphen keeps syncing, with no config change and no user action.
- **G2** — A template whose planner heading was *renamed* (`## Agenda`) or *translated*
  (`## Planejamento`) is fixed in one click from the panel, without the user learning that
  `.thock/config.toml` exists.
- **G3** — When sync holds for a structural reason, the panel says which heading it wanted and
  offers the fix. No silent hold survives this spec.
- **G4** — No false positives: a heading that merely *contains* the configured words
  (`## Yesterday's day planner review`) still does not match.
- **G5** — G6 of V8 holds: a vault with no calendar connected gains no new rows, prompts, or writes.

**Success:** the author edits `templates/daily.md` freely for a week — emoji, renames, reordering —
and never has to think about the syncer.

## 3. Non-goals

- **The anchor comment** (`<!-- thock:day-planner -->` as a heading-independent identity). It is the
  right long-term answer for renames and it is deferred to a follow-up; §6.2's alias list covers the
  same ground with config instead of note markup, and it can ship first.
- **Fuzzy / edit-distance matching.** Normalization is a deterministic key comparison. "Close
  enough" matching would make the failure mode *wrong section* instead of *no section*, which is
  worse (the reconciler writes there).
- **Rewriting the user's template.** `AddPlannerHeading` appends to *today's note*, never to
  `templates/daily.md`. Fixing the template is the user's edit — or their agent's.
- **Backlog heading resolution beyond adopting the shared matcher.** V19's configured-then-English
  fallback stays exactly as it is; it just gains the normalization pass.
- **Promoting a too-deep heading.** V26 explains the level-6 dead end (§7.2); it does not restructure
  the note.

## 4. The bug, precisely

`day_plan::planner_section` resolves the planner heading with
`heading_level_and_text(line)?.1.to_lowercase() == wanted.to_lowercase()`. Everything below fails
that test today:

| Heading in the note | Why it misses |
| --- | --- |
| `## 📅 Day planner` | leading emoji is part of the text |
| `## Day planner:` | trailing colon |
| `## Day planner ##` | closing hashes survive `trim_start_matches('#')` on the left only |
| `## **Day planner**` | inline markup is text |
| `## Day-planner` | hyphen |
| `## Agenda` | genuine rename — normalization cannot help |
| `Day planner` + `-----` | setext headings are not parsed (unchanged in V26) |
| `###### Day planner` | matches, but its `Calendar` child would be level 7 (§7.2) |

The second-order problem is that only *sync* fails. `parse_day_plan` treats a missing heading as
"parse the whole note", so the panel keeps working and the user has no signal at all.

## 5. Heading resolution

### 5.1 The normalized key

```rust
/// A heading's comparison key: links reduced to their label, then every
/// character that is not a letter or digit folded to a single space,
/// lowercased and trimmed.
pub fn heading_key(text: &str) -> String
```

Steps, in order:

1. Replace each inline link with its label (`markdown_syntax::inline_links`), so
   `## [Day planner](plan.md)` and `## [[Day planner]]` key as `day planner` rather than dragging the
   destination in.
2. Fold every `!char::is_alphanumeric()` to a space. This is what removes emoji, `**`, `~~`, `` ` ``,
   `_`, colons, closing hashes, hyphens and slashes in one rule.
3. Lowercase (Unicode-aware, as V19 requires), collapse runs of whitespace, trim.

`is_alphanumeric` is deliberately Unicode-wide: `Planejamento`, `日次計画` and `Тайлан` all key to
themselves, so §6.2's aliases work in any language.

**Equality, not containment** (G4). `day planner` ≠ `day planner today`. Adding *words* to a heading
is a rename, and renames are §6.2's job — not something to guess at.

### 5.2 Two passes, exact first

Resolution walks the note's headings twice:

1. **Exact** — `text.to_lowercase() == wanted.to_lowercase()` (V4's rule, unchanged).
2. **Normalized** — `heading_key(text) == heading_key(wanted)`.

Exact wins over normalized wherever both exist, regardless of line order, so a note holding both
`## Day planner` and `## Day-planner` behaves exactly as it does today. Within a pass, the first
heading in file order wins (V4 §5.2, unchanged).

With aliases (§6.2) the passes are ordered `exact(name₀…nameₙ)` then `normalized(name₀…nameₙ)`: an
exact hit on an alias beats a normalized hit on the canonical name, because an exact hit is never
ambiguous.

### 5.3 Where it applies

| Site | What it resolves |
| --- | --- |
| `day_plan::planner_section` | the planner heading, for both the panel and the reconciler |
| `calendar::find_child_section` | the `Calendar` child section (V8 §5.1 rule 1) |
| `backlog::section_line_range` | Soon / Someday / Completed (V19), after its existing configured-then-default fallback |

`parse_day_plan`'s subsection *identity* is untouched: a subsection is still named by its literal
heading text, and `[day_planner.sections]` colour pins still key on the trimmed lowercased name.
Normalization is for *finding* a section, never for naming one.

## 6. Configuration

### 6.1 What is written vs. what is matched

`[day_planner] heading` has always been both "the name Thock looks for" and "the name Thock writes".
V26 splits them: the **canonical** name is what `AddPlannerHeading` writes and what the status row
names in prose; the **matched** set is the canonical name plus its aliases, each in both passes.

### 6.2 `heading` accepts a list

```toml
[day_planner]
heading = "Day planner"                      # unchanged, still valid
heading = ["Agenda", "Day planner"]          # canonical first, the rest are aliases
```

Rules:

- Entries are trimmed; empty ones are dropped. The first surviving entry is canonical.
- A list that is empty or all-blank resolves to the empty heading — which, as today, means "parse
  the whole note" for the panel and "hold" for sync.
- The old scalar form is the one-element list. Nothing in an existing vault changes.

Aliases are what make a rename safe: keeping the old name in the list means half-migrated notes —
yesterday's `## Day planner`, today's `## Agenda` — both resolve while the vault catches up. This is
exactly the state the **Set Language** ritual leaves a vault in mid-translation.

## 7. The hold, made actionable

### 7.1 A structured reason

```rust
pub enum HoldReason {
    /// Nothing structural — no calendars selected yet, no note for today.
    Waiting(SharedString),
    /// Today's note has no heading resolving to `[day_planner] heading`.
    NoPlannerHeading { heading: SharedString },
    /// The planner heading is level 6, so its Calendar child would be level 7.
    PlannerHeadingTooDeep { heading: SharedString },
}
```

`SyncState::Holding { reason: HoldReason }` replaces the free string. `Waiting` renders exactly as
the string did, so no existing hold changes appearance.

### 7.2 The level-6 dead end

A level-6 planner heading resolves fine, but `## Calendar` beneath it would be `#######` — not a
heading at all. Today `reconcile` notices this and returns *no edits*, which is indistinguishable
from "nothing to do". V26 makes it `Reconciled::NoSectionRoom`, reported only when the fetch actually
produced events, so an empty day still says nothing (G5).

### 7.3 The status row (extends V8 §10.3)

| Hold reason | Row |
| --- | --- |
| `Waiting(reason)` | `Calendar · {reason}` in muted text — unchanged |
| `NoPlannerHeading` | `Calendar · no "Day planner" heading in today's note` + **Add heading** + **Use another heading…**, with a tooltip naming the exact heading text it wants |
| `PlannerHeadingTooDeep` | `Calendar · "Day planner" heading is too deep for a Calendar section` + a tooltip explaining that a level-6 heading can have no children, so it needs to be level 5 or shallower |

Both buttons dispatch named actions, so they are equally reachable from the command palette, and the
row stays inside the panel's existing keyboard model.

## 8. Actions

Both live in the `thock` namespace with note-taker-facing doc comments:

- **`thock::AddPlannerHeading`** — appends the canonical planner heading to today's note, preceded by
  a blank line, at the level chosen in §8.1. Writes through the open buffer when the note is open (so
  no unsaved keystroke can be lost, V8 G5) and through the project `Fs` otherwise. Create-if-missing
  does *not* apply: with no note for today the service is already holding on `Waiting`, and the
  button is not shown. Triggers a sync on success.
- **`thock::ChoosePlannerHeading`** — reads today's note, lists its ATX headings in a picker
  (`level · text`, filtered by substring, `enter` confirms), and writes the chosen text to
  `[day_planner] heading` in `.thock/config.toml`. A note with no headings shows an empty state
  pointing at **Add heading**. The write goes through `toml_edit`, so the user's comments and key
  order in their own config file survive (the existing `update_config_file` helper reformats, which
  is acceptable for the machine-written `calendar.toml` and not for `config.toml`).

Both are human-in-the-loop: each one is a button the user pressed, and each writes exactly one thing.

### 8.1 Which level `AddPlannerHeading` writes

The heading has to land as a *sibling of the note's other sections*, or the Calendar child ends up in
the wrong place. The level is inferred from the note:

1. The most common heading level among all headings **after the first** — the first is the note's
   title. Ties break shallower.
2. Failing that (a note with zero or one heading), the first heading's level + 1, clamped to ≤ 5.
3. Failing that (no headings at all), level 2.

For the shipped template — `#` title, three `##` sections — this is `##`. For a template whose
sections are `#`, it is `#`. The result is never deeper than 5, so the heading Thock writes can always
hold a Calendar child (§7.2); cases 2–3 are additionally floored at 2.

## 9. Testing

Pure-function tests carry the weight; the panel gets one integration test.

- `heading_key`: emoji prefix, trailing colon, closing hashes, bold, hyphen, wikilink and inline link
  labels, an all-punctuation heading keying to empty, and CJK/accented text surviving intact.
- `planner_section`: each decorated form in §4 resolves; extra words do not (G4); exact beats
  normalized across line order; aliases resolve in both passes; the empty heading still means
  whole-note.
- Vault config: scalar form, list form, list with blanks, empty list; canonical/alias split;
  round-trip through `write_agent_command` keeps a list a list.
- `reconcile`: planner found under a decorated heading; `Calendar` child matched normalized; level-6
  planner with events yields `NoSectionRoom`, without events yields no edits and no hold.
- `CalendarService`: a note missing the heading lands in `Holding { NoPlannerHeading }`; adding the
  heading and re-syncing produces the section.
- `AddPlannerHeading`: level inference for the shipped template, a `#`-sectioned note, a note with
  one heading, and a note with none; the append leaves prior content byte-identical.

## 10. Decisions worth recording

**Why not match on containment?** Because the reconciler *writes* into whatever it resolves. A
false positive is a section of meetings appearing under the wrong heading, and the user's recourse is
manual cleanup. A false negative is a hold — which, after §7, is a visible question with a button.
Asymmetric costs, so the matcher stays strict and the UI absorbs the misses.

**Why aliases before the anchor comment?** The anchor is better (it survives renames with no config
at all), but it only helps notes written *after* it ships, it adds foreign matter to the template,
and it is itself deletable by the same template edit that started this. Aliases need no note change
and fix existing vaults today. The anchor can layer on top later without changing anything here.

**Why does `AddPlannerHeading` write the note and not the template?** The template is a document the
user owns and may have deliberately restructured (V26's whole premise). Writing today's note fixes
today; the user or their agent decides whether the template should change. Appending to a note is
also the only write that cannot lose anything.
