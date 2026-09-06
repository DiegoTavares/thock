# Thock V22 — Inviting templates and the example first day

**Status:** Implemented (2026-09-05)
**Owner:** Diego · **Date:** 2026-09-05
**Companion docs:** `../VISION.md` (§4.5 Augmentation not replacement, §4.8 Everything is editable,
§5.5 Onboarding, §12 Milestone 4), `v18-first-run-onboarding.md` (the startup flow and Welcome Tour
this adjusts), `v19-vault-language.md` (the Set Language ritual that now also translates the example),
`v10-markdown-conceal.md` (the markup the example leans on)

---

## 1. Summary

The first non-developer trial (2026-09-05) found the shipped daily and weekly templates too bare:
three headings and nothing else neither invite writing nor show a person with no Markdown
knowledge what is possible. Two changes, both plain files:

1. **The templates become inviting.** `templates/daily.md` and `templates/weekly.md` gain a `___`
   rule between sections and one italic prompt under each heading ("_What happened, what you
   noticed, how it went._"), so every new page already says what it is for.
2. **The first day is a filled-in example.** On a fresh vault, startup writes today's daily note
   from a shipped example (`crates/thock/assets/example-day.md`) instead of the template: real-looking
   journal prose, timed and untimed checkboxes the Day Planner draws, a quote, a wikilink, some
   emojis, and a closing "How this page works" section. Its opening callout says it is an example,
   names `templates/daily.md`, and says that template is meant to be customized. Startup opens it as
   the active tab, with the rendered `welcome.md` one tab behind.

Nothing here touches an existing vault: templates and the example are create-if-missing, and the
example is written only by the fresh-scaffold branch of startup.

## 2. Locked decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Rule character | `___`, never `---`. V10 conceals only `___` as a drawn rule; `---` is ambiguous with front matter. |
| 2 | Prompts live in the template | Each section carries one short italic line. They copy into every note and are the user's to delete or reword; that is what "everything is editable" means for a template. |
| 3 | Where the example lives | One asset, `crates/thock/assets/example-day.md`, expanded through the same `{{date:…}}` tokens as the templates; the templates remain the source of every later note. |
| 4 | When it is written | Only by `open_startup_vault`'s fresh-scaffold branch, and only when today's note does not exist. **Create vault here** (scaffolding into a folder the user already had) does not write it: a folder with the user's own notes should not gain an invented day. |
| 5 | What opens first | The example day is the active tab; `welcome.md` (V18 §5.2) still opens rendered, one tab behind. The example's callout points at the template; the welcome note still points at the Getting started list. |
| 6 | The tour adapts | Welcome Tour step 4 no longer creates today's note on a fresh vault: it asks the user to open the example and replace its planner lines with their own, and falls back to creating from the template when today's note is missing. |
| 7 | Language | Set Language translates the example day in its first batch **only while it is still the untouched example** (detected by its opening callout). Once the user has written in it, it is theirs and is never touched. |
| 8 | Weekly stays empty | Only the daily note ships filled in. The weekly template gets prompts and rules; no example week is invented. |

## 3. Definition of done

1. `DEFAULT_DAILY_TEMPLATE` and `DEFAULT_WEEKLY_TEMPLATE` keep their title line first (existing
   tests and the planner heading contract hold) and add rules and prompts.
2. `notes::ensure_example_day` writes the expanded example as today's note, returns its path, and
   returns `None` without touching anything when today's note exists. Every `{{…}}` token expands.
3. The example's planner lines parse under the default `DayPlannerConfig` with at least one timed,
   one done, and one open item (so the right rail is populated on first launch).
4. Startup on a fresh vault opens `welcome.md` rendered and then the example day, in that order.
5. The Welcome Tour and Set Language rituals reflect decisions 6 and 7.
6. VISION §12 Milestone 4 gains a shipped entry in the same change.

## 4. Non-goals

- No example weekly note, no example backlog, no example inbox item.
- No change to what **Create vault here** or `reconcile_vault` write.
- No localized example: the Set Language ritual translates it on request, as it does the templates.
- No new GPUI surface. Files only.

## 5. Traps

- The example must only use markup the panels already handle: `___` rules, ATX headings, `- [ ]`
  tasks with `HH:MM - HH:MM` or `HH:MM` prefixes, `[[wikilinks]]` to files that exist (`welcome.md`
  does). Bold and italic are highlighted by the Markdown grammar but their markers stay visible; that
  is fine for a page whose job is to show the markup.
- The closing "How this page works" bullets mention `- [ ]` inside backticks on lines that begin
  with `- `, not `- [ ]`, so the planner and the wrap rituals never mistake them for tasks.
