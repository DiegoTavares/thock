# Thock V23: Personalized rituals and a dashboard that fits

**Status:** Implemented (2026-09-07)
**Owner:** Diego · **Date:** 2026-09-07
**Companion docs:** `../VISION.md` (§4.5 Augmentation not replacement, §4.8 Everything is editable,
§4.9 Modular life, §5.5 Onboarding, §12 Milestone 4), `v18-first-run-onboarding.md` (the Welcome
Tour this extends), `v19-vault-language.md` (the Set Language ritual this one is modelled on),
`v22-inviting-templates-and-example-day.md` (the same non-developer trial that prompted this)

---

## 1. Summary

The second non-developer trial (2026-09-07) hit the problem V22 didn't: the templates were now
inviting, but the **rituals** still belonged to an engineer. **Wrap Today** went looking for commits
and offered to authenticate against GitHub and GitLab; **Week Review** counted pull requests; the
weekly dashboard devoted three of its six stat tiles, a timeline bar, and a full-width panel to
merge requests that would never exist. None of it is wrong for the author of the tool. All of it is
noise for the person he was sitting next to.

The fix is one new file and one new question:

1. **`profile.md` at the vault root.** Who this person is, what they want from Thock, the handful
   of areas their weeks are made of, and a checklist of the outside sources any ritual may go and
   look at. Plain Markdown, at the top level, theirs to edit.
2. **The Set Profile ritual** (`skills/thock/set-profile.md`) is the short interview that writes it.
   The Welcome Tour runs it at step 3 (where it already asked "what brought you here?" and threw the
   answer away), and the Timeline setup runs it at step 0 if it is still missing.
3. **The rituals read it.** Wrap Today, Wrap Yesterday, and Week Review treat the checklist as their
   whole permission list: an unchecked box means *don't look and don't ask*, in silence.
4. **The dashboard reads it too**, through `window.PROFILE` in `weekly/site/data.js`. A vault that
   doesn't track code loses the pull-request panel, its three stat tiles, and its timeline bar, and
   gains carried-over goals and personal items in their place.

Nothing here is required. A vault with no `profile.md` behaves exactly as it did before, and the
dashboard's `"auto"` default decides from the data it already has.

## 2. Locked decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Where the profile lives | `profile.md` at the **vault root**, plain Markdown. Not `.thock/` (agents are forbidden to write there, and this is the user's own description of themselves), and not a `## Section` appended to `AGENTS.md` (that file holds rules for agents; this holds facts about a person). |
| 2 | Not scaffolded | `profile.md` is written only by the Set Profile ritual. A shipped empty one would be a lie, and its absence is exactly the signal the rituals need to fall back to their old behavior. |
| 3 | Fixed headings | The ritual writes `## Who you are`, `## What you want from Thock`, `## What you track`, `## What Thock may pull in`, `## Tone`. The other skills match on those names, so they are a contract, not prose. |
| 4 | The permission list is a checklist | `- [ ] Code`, `- [ ] Calendar`, `- [ ] Email`, defaulting to unchecked. Unchecked means *don't look and don't ask*: a ritual skips that step **in silence** rather than announcing what it skipped. |
| 5 | Code is offered, not assumed | Set Profile raises the Code option **only if** the interview suggests the user writes code. A person who codes will say so; a person who doesn't should never see the word "repository". |
| 6 | Backward compatible by default | No `profile.md` → every ritual behaves as it did before V23 (ask once, record in `sources.md`). An existing developer vault notices nothing. |
| 7 | "None" is an answer | `routines/timeline/sources.md` with both lists explicitly `_None._` is a settled answer, not an empty file. The rituals never re-ask. This is what stops a non-developer being asked about repositories every single day. |
| 8 | The fallback question got kinder | Even with no profile, the wrap/review skills' one-time question is now "is there anything outside your notes I should look at (code repositories, for example)? 'Nothing' is a perfectly normal answer", not "which repositories should this read from?". |
| 9 | `window.PROFILE.code` is tri-state | `true` / `false` / `"auto"` (the default). `"auto"` shows the code panels only in a feed that actually carries PRs or MRs, so an existing vault upgrades correctly without anyone editing `data.js`. |
| 10 | What replaces the code tiles | `carried over` (unfinished goals repeating from the prior week) and `personal` (personal items done). Both are computed from data the dashboard already holds; nothing new has to be recorded for them. |
| 11 | `focus` earns its place | The profile's areas, in order, give each area a stable colour across weeks. Previously a project's colour came from its index within one week, so it changed as the list reordered. |
| 12 | Feeds are normalized on load | The page fills in missing `prs`, `goals`, `personal`, `projects`, `highlights` and `tasks` once, up front. A week written by an agent that had nothing to put in a field may omit it, and a page that throws is worse than a page with an empty panel. |
| 13 | Set Profile is a core skill | Materialized alongside Set Language and reachable as `thock: set profile`, not a Timeline skill. A profile is about the person, and Routines must not assume other Routines exist. |
| 14 | The ritual owns its one file | Set Profile may rewrite `profile.md` whole (after showing the user), and may set `window.PROFILE` and `sources.md`. It rewrites nothing else, and explicitly does not edit the skill files to match. |

## 3. Definition of done

1. `skills/thock/set-profile.md` ships as a core materialized file and is reachable through the
   `thock::SetProfile` action (`thock: set profile` in the palette).
2. The Welcome Tour runs it at step 3; the Timeline setup runs it at step 0 when `profile.md` is
   missing, and honors an existing one without re-interviewing.
3. Wrap Today, Wrap Yesterday, and Week Review read `profile.md` before they run, gate every outside
   source on its checklist, and omit the `## Commits` / `### Pull & Merge Requests` headings entirely
   when there is nothing to put under them.
4. `weekly/site/index.html` hides the pull-request panel, the `created`/`merged`/`reviewed` tiles,
   the legend entry, and the timeline bar when the profile says no code, retitles "Work by Project"
   to "Where your week went", and shows `carried over` and `personal` instead.
5. Areas keep the same colour from week to week.
6. The dashboard renders without throwing for: a feed whose weeks omit `prs` entirely; a feed with
   no `window.PROFILE`; `code: true`; `code: false`; and `"auto"` in both directions.
7. `AGENTS.md` names `profile.md` in its ground rules and its map.
8. The Timeline Routine's manifest version is bumped (9 → 10) and its `reads`/`writes` and summaries
   stop promising commits unconditionally.
9. VISION §12 Milestone 4 gains a shipped entry in the same change.

## 4. Non-goals

- **No Rust-side profile model.** Nothing in the app parses `profile.md`; it is read by agents only.
  The one Rust change is materializing the new skill and registering its action.
- **No migration of existing vaults.** Shipped files are create-if-missing, as always. An existing
  vault gains the Set Profile ritual and can run it; nothing rewrites its skills or its `data.js`.
- **No new panel and no new UI.** The profile is a file and an interview.
- **No personas beyond the interview.** There is no "developer mode" flag, no preset bundles, and no
  branching install. The user's own words in `## What you track` are the whole model.
- **No changes to the Inbox or Lifestyle Routines' behavior** beyond one phrase of developer jargon
  removed from the Money Ritual's error advice.

## 5. Implementation notes

### 5.1 The file

```markdown
# About you

## Who you are
## What you want from Thock
## What you track
## What Thock may pull in
- [ ] Code (commits, pull requests, merge requests)
- [ ] Calendar (meetings and appointments)
- [ ] Email (labelled mail)
## Tone
```

### 5.2 The action

`thock::SetProfile` mirrors `thock::SetLanguage`; both now go through one `run_core_skill` helper in
`agent_panel.rs` that checks the workspace is a vault and launches the ritual on the default model
tier. Palette-only, no keybinding, like Set Language.

### 5.3 The dashboard contract

```js
window.PROFILE = {
  code: "auto",   // true | false | "auto"
  focus: []       // the areas from profile.md, in order
};
```

`SHOW_CODE` resolves once at load. When false, `#prPanel` and `#legendPrs` are removed from the DOM
rather than hidden, so the render path is guarded by a single `if (SHOW_CODE)` and never writes into
a missing node.
