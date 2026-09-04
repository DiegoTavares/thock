# Thock V19 — Your vault, in your language

**Status:** Implemented (2026-09-04)
**Owner:** Diego · **Date:** 2026-09-03
**Companion docs:** `../VISION.md` (§4.1 Your files forever, §4.8 Everything is editable, §12 Milestone 4),
`v18-first-run-onboarding.md` (AGENTS.md, the Welcome Tour, the Getting started checklist this rides on),
`v6-backlog.md` and `v17-backlog-categories.md` (the file and parser this makes configurable),
`v5-agent-and-onboarding.md` (the done-marker protocol)

---

## 1. Summary

Thock's first beta tester is a non-developer whose agent is Gemini CLI. The next one may not read
English. Today the vault is English in three different ways, and only one of them is honest: the
prose in the shipped files (a user's to edit), the language the agent happens to answer in (an
accident), and the section headings the Rust parser matches on (a hard dependency the user cannot
see and will break by translating).

V19 makes the vault speak the user's language, and makes the third category safe. A **Set Language**
ritual asks what language the user wants, records it as a binding instruction in `AGENTS.md`, and
translates the vault's prose surfaces. Underneath it, `[backlog] headings` becomes configurable the
way `[day_planner] heading` already is, so a translated `backlog.md` still draws a Backlog panel.

The app's own buttons stay English. This is deliberate and stated plainly to the user: **your notes
and your agent speak your language; the app's chrome doesn't yet.**

## 2. Locked decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Reach | **Vault + agent, not app chrome.** Templates, notes, docs, headings, and every word the agent says. No GPUI string table, no `.ftl` files, no i18n framework. |
| 2 | Where the setting lives | **`.thock/config.toml`** — `[language]` and `[backlog] headings`. Parsing config has no other home (`[day_planner] heading` is the precedent), and `deny_unknown_fields` means the downgrade break happens either way. Accepted: V19 is where vaults stop opening in pre-V19 builds. |
| 3 | `SectionKind::heading()` | **Splits in two.** `id()` returns a stable, language-independent `"soon"` for persisted state and element ids; `heading(&BacklogConfig)` returns the text in the file. Conflating them is the bug this spec exists to prevent. |
| 4 | Heading resolution | **Configured heading first, built-in English default as fallback.** Both orders of the two-write migration work, and a half-translated vault still parses (a vault is hand-editable; half-migrated states are normal). |
| 5 | Skills are not translated by default | Shipped skills are materialized create-if-missing, so a translated one never receives upstream improvements again. The ritual **offers** the skills batch as an explicit second step with that warning, and does not do it silently. |
| 6 | Date words | Where the chosen language isn't English, the ritual rewrites the daily template's title to a **wordless date format** (`YYYY-MM-DD`) rather than emit English month and weekday names. Localized month names are future work (§8). |
| 7 | Entry point | **The Welcome Tour asks first**, and runs the ritual when the answer isn't English. **No fifth Getting started row** — the checklist stays at four. A `thock: set language` palette action reaches the ritual any time, on any vault. |
| 8 | Existing vaults | Gain `[backlog] headings` support by upgrade (defaults unchanged, nothing to do) and the ritual by palette action. Nothing is translated without being asked for. |

## 3. Goals & success criteria

**Primary:** a Brazilian user finishes the Welcome Tour and finds a vault whose templates, backlog
columns, welcome note, and routine docs are in Portuguese — and an agent that has been speaking
Portuguese since the tour's second sentence — without any panel having gone blank on the way.

**Definition of done:**

1. `[backlog]` accepts `headings = { soon = "...", someday = "...", completed = "..." }`; absent or
   partial tables fall back to `Soon` / `Someday` / `Completed` per key.
2. `parse_backlog` and every write helper take the resolved `BacklogConfig`; the Backlog panel and
   the Gmail capture path both pass the vault's.
3. Persisted collapsed-group state keys on `SectionKind::id()`, and state written by pre-V19 builds
   (which keyed on the English heading) still restores.
4. The Backlog panel's column headers render the *configured* headings — a translated `backlog.md`
   shows translated columns.
5. `AGENTS.md` documents a `## Language` section as binding, and its ground rule 3 carves out the
   config-writing exception the ritual needs.
6. `skills/thock/set-language.md` exists, is agent-agnostic, confirms before each write batch, writes
   the config and the headings together, and ends with the V5 done marker.
7. The Welcome Tour opens by asking what language to speak, and hands off to the ritual when the
   answer isn't English.
8. `thock: set language` runs the ritual from the command palette in any vault.
9. VISION §12 Milestone 4 gains a shipped entry in the same change.

## 4. Non-goals

- **Localizing the app's chrome.** "Add Routine", "Finish setup", "Move to Someday", every tooltip
  and empty state stays English. V18 already declared localization a non-goal; V19 narrows that to
  the chrome rather than lifting it.
- **Localized month and weekday names** in note titles or filenames (§8).
- **Translating `guide/index.html` at install time.** The ritual may translate it as an opt-in batch,
  but no localized guide ships.
- **A language picker in the UI.** The setting is a file and a ritual, per V5 §5.3 doctrine.
- **Machine-translation infrastructure.** The user's own agent does the translating, in the
  conversation, with confirmation — which is also why the results are theirs to edit.
- **Multi-language vaults.** One vault, one language.

## 5. Design

### 5.1 The language setting

```toml
[language]
tag  = "pt-BR"          # BCP 47, for anything that later needs a machine-readable locale
name = "Portuguese (Brazil)"   # what the agent was told to speak, in the user's words
```

Both fields are informational today — nothing in Rust branches on them. They exist so the setting is
inspectable, so a re-run of the ritual knows what it did last time, and so §8's date work has a
locale to read. The binding instruction is the `AGENTS.md` section, not the config.

`AGENTS.md` gains, appended by the ritual (never shipped pre-filled):

```markdown
## Language

Speak and write in **Portuguese (Brazil)** — in conversation, and in everything
you append to a note. Keep unchanged: file and folder names, the marker paths
under `.thock/`, and the task syntax (`- [ ]`). Section headings the app parses
are named in `.thock/config.toml`; follow that file, not your memory.
```

The last sentence matters. Without it an agent that has been told "write in Portuguese" will
helpfully translate `## Soon` on its next append, and the panel goes blank.

### 5.2 Configurable backlog headings

`[backlog]` grows a `headings` table, resolving into `BacklogConfig` the way `[day_planner]` resolves
into `DayPlannerConfig`:

```rust
pub struct BacklogHeadings { pub soon: String, pub someday: String, pub completed: String }

pub struct BacklogConfig {
    pub file: String,
    pub headings: BacklogHeadings,
}
```

`SectionKind` splits its one string accessor into two, because the current `heading()` is doing two
incompatible jobs:

| Accessor | Value | Used for |
|---|---|---|
| `id()` | `"soon"` / `"someday"` / `"completed"`, always | Persisted collapsed-group state, GPUI element ids, anything that must survive a language change |
| `heading(&BacklogConfig)` | the configured text | Matching sections in the file, creating a missing section, the panel's column label |

`from_heading` becomes `from_id`, and accepts the three English defaults as legacy aliases so
collapsed groups persisted by a pre-V19 build still restore (decision 3, done criterion 3).

**Resolution is forgiving** (decision 4). `section_line_range` is tried with the configured heading
and, on a miss, with the built-in English default. Writes always use the configured heading. The
consequence is that the ritual's two writes — config and file — are safe in either order, and a vault
where the user translated `## Soon` by hand before touching config still renders.

The English defaults stay in `DEFAULT_BACKLOG` and `DEFAULT_CONFIG_TOML`; a fresh vault is unchanged.

**The panel gets translated columns for free.** `render_section_header` labels the column with
`section.heading(&config)` instead of the constant, so a Portuguese vault reads *Em breve · Algum dia
· Concluído* while the tooltips beside them stay English. That inconsistency is the honest shape of
decision 1, and the customize page should say so in one line.

### 5.3 The Set Language ritual

`skills/thock/set-language.md`, a core-owned skill materialized beside `new-routine.md` and
`welcome-tour.md`. Written to the agent, agent-agnostic, append-and-confirm, with the V5 contract
shape. The beats:

1. **Ask.** One question: what language should I use with you? Accept it in the user's own words.
   If the answer is English, record it and stop — nothing to translate.
2. **Explain the shape, in one breath.** Your notes and I will be in <language>; the app's own buttons
   are still English for now.
3. **Bind yourself first.** Append the `## Language` section to `AGENTS.md` (§5.1) before anything
   else, so every later step — and every future session — is already in the right language.
4. **Propose the plan.** Show the config keys to be written and the list of files to be translated.
   Get a yes.
5. **Write the settings.** `[language]`, `[backlog] headings`, `[day_planner] heading` in
   `.thock/config.toml` — the ritual's one sanctioned write under `.thock/` (§5.4).
6. **Rename the headings that are parsed.** `backlog.md`'s three section headings and the daily
   template's planner heading, to exactly what step 5 wrote. This is a rewrite of the user's file
   and is allowed only because they explicitly asked for it (AGENTS.md ground rule 1's escape hatch);
   say so as it happens.
7. **Translate the prose, in confirmed batches.** `templates/daily.md` and `templates/weekly.md`
   (retitling the daily to a wordless date format per decision 6, with a one-line reason), then
   `welcome.md`, `guide/customize.md`, and each installed Routine's `doc`.
8. **Offer the extras.** The skills under `routines/*/skills/` and `guide/index.html`, each with the
   warning from decision 5: translate these and they stop receiving Thock's updates.
9. **Complete.** Write `.thock/state/onboarded/set-language` with a one-line summary of what changed,
   and tell the user the ritual can be re-run any time from the command palette.

Existing notes are never touched. What the user already wrote stays in the language they wrote it in.

### 5.4 The `.thock/` carve-out

`AGENTS.md` ground rule 3 currently forbids writing under `.thock/` with a single exception for
documented state markers. The ritual needs a second: `.thock/config.toml`, when a skill is explicitly
changing a setting the user asked for. The rule is amended to name both exceptions rather than
softened, because the point of the rule is that agents don't touch app machinery *incidentally*.

`routines/ROUTINES.md`'s protected-path section gets the same amendment.

### 5.5 Welcome Tour integration

The tour's first beat becomes three questions instead of two — language, then their name, then what
they hope Thock helps with — and when the language isn't English it runs the Set Language ritual
before creating today's note, so the first note the user sees is already in their language.

This is the whole of the onboarding wiring (decision 7). No `Steps` field, no fifth row, no panel
change.

**A limitation to state, not hide:** the checklist's first two steps — the Introductory Guide and the
customize page — are read before the agent is connected, so a non-English speaker's first five
minutes are still in English. Nothing can fix that without a language picker before first run, which
is out of scope. The tour is the moment the vault becomes theirs.

## 6. Implementation notes

- **Crate placement:** everything in `crates/thock` and its assets. **No file outside
  `crates/thock/` or `thock/` is touched** — no keymap entry, no `Cargo.toml` member.
- **The identity trap (decision 3).** `section.heading()` is currently the KVP key at
  `backlog_panel.rs:299` (read back through `from_heading` at `:208`) and the element-id seed at
  `:1322`, `:1466`, `:1546`, as well as the file heading at `backlog.rs:304`, `:448`, `:462`. Split
  the accessor *first*, in its own commit, and the rest of the change is mechanical.
- **Call sites to thread the config through:** `backlog_panel.rs` `:414`, `:815`, `:965`, `:999`,
  `:1024`, `:1126`, `:1187`, `:2140`, and `gmail_service.rs` `:707`, `:730`. The panel already holds
  the vault; Gmail capture resolves it on its existing path.
- **Config plumbing** mirrors `DayPlannerConfigContent`: all-optional fields, per-key fallback,
  `is_unset` so an untouched `[backlog]` is not re-emitted on a registry rewrite, and
  `skip_serializing_if` throughout.
- **`deny_unknown_fields`** stays (V7 §9 trap 4 is about *new state*, and these are genuinely
  settings). The cost is that a V19 vault does not open in a pre-V19 build — acceptable at one beta
  tester, and worth revisiting for its own sake (§8).
- **Testing.** Unit: `parse_backlog` round-trips under non-English headings; per-key fallback when
  `headings` is partial; the English-default fallback match (decision 4) in both half-migrated
  directions; `from_id` accepting legacy English keys; `complete_task_edits` and `move_task_edits`
  creating a missing section with the configured heading. Integration: the V5 fake-agent pattern —
  a script writes the config, translates `backlog.md`, and touches the done marker; the panel
  re-renders with translated columns and the same collapsed groups. Live: a Portuguese dry run of
  the tour end to end before the build reaches a tester.

## 7. Phasing & deliverables

| Phase | Ships | Mergeable alone? |
|---|---|---|
| **1 — Configurable headings** | `BacklogHeadings`, the `id()`/`heading()` split, fallback resolution, threaded call sites, translated column labels | **Yes** — removes the last hardcoded parsed heading, valuable with no ritual at all |
| **2 — The ritual** | `AGENTS.md` `## Language` contract + `.thock/` carve-out, `skills/thock/set-language.md`, `thock: set language`, ROUTINES.md amendment | Yes, atop 1 |
| **3 — Onboarding** | Welcome Tour's language question and hand-off, the wordless-date mitigation, a line on the customize page about chrome staying English | Yes, atop 2 |

## 8. Future work (explicitly deferred)

- **Localized dates.** `notes.rs` renders `MMMM`/`dddd` through chrono's `%B`/`%A`, which are
  English-only. Doing this properly means the `unstable-locales` feature, a locale read from
  `[language] tag`, and the matching change in `parse_date` so filename round-tripping survives.
  Decision 6 is the stopgap.
- **App chrome i18n** — a real string table across the Thock panels. A separate project with a
  separate justification.
- **Divergence tracking.** Record which shipped files the ritual translated (and at what version), so
  a later reconcile can tell the user "Wrap Today has been improved upstream; your translated copy is
  three versions behind" instead of silently freezing them.
- **Relaxing `deny_unknown_fields`** to warn-and-ignore, so future settings stop being one-way
  vault upgrades. Only helps builds shipped after the change, which is exactly why it should happen
  early.
- **A shipped translation catalog** — pre-translated templates and docs for common languages, so the
  ritual becomes a copy rather than an LLM call.
