# Week Review

Review the previous week's daily notes, plus whatever outside activity the vault's profile allows you to pull in, then produce a summary organized by the areas the user tracks, write it to the weekly markdown file, and append a structured entry to the dashboard data (`weekly/site/data.js`).

**Reads:** the daily and weekly notes, the backlog file, `profile.md`, and any sources recorded in `routines/timeline/sources.md`.
**Writes (append-only):** the weekly `.md` file, `weekly/site/data.js`, the backlog file.

> **Note paths and filenames are vault-configured.** Read `.thock/config.toml` first: `[daily]` / `[weekly]` set each note kind's `dir` and moment-style `filename` format, and `[backlog]` sets `file`. This skill's examples use the defaults (`daily/YYYY-MM-DD.md`, `weekly/GGGG-[W]WW.md`, `backlog.md`); when the config — or the vault's existing notes — use different names, follow those silently. That's configuration, not a doc mismatch worth reporting.

> **Read `profile.md` at the vault root next, if it exists.** It names who this person is, the areas they track (use those names in §5 and in the dashboard entry), the tone to write in, and, under **What Thock may pull in**, the only outside sources you may go and look at. A vault without one is normal: then §3 falls back to `routines/timeline/sources.md`, and asks once if that's missing too.

## 1. Determine the week and locate the weekly file

1. The review covers **Monday–Sunday of the week before today**. Compute those two dates (YYYY-MM-DD).
2. Resolve the weekly file from the config (`[weekly]` `dir` + `filename`), formatting the pattern with the week's Monday. The default names weeks by **ISO week** — `GGGG-[W]WW`, i.e. ISO week-year, a literal `W`, and the zero-padded ISO week number — so the week starting Mon 2026-07-20 is `weekly/2026-W30.md` by default. If the file doesn't exist yet, create it.
3. The **week id** used everywhere below is the weekly filename without `.md` (e.g. `2026-W30`).

## 2. Read the daily notes

1. Find all daily notes within the range — match them against the configured daily dir and filename format.
2. Read each one. Extract tasks and activities from day-planner sections, conversation notes, and anything else relevant.
3. If a weekly note already exists, read it and fold in its `# Week Goals`, `# Tentative`, and `# Personal` sections (these become the goals in the dashboard) plus any context.

## 3. Decide whether there is anything outside the notes to review

**Start from the profile.** Its **What Thock may pull in** checklist is the whole permission list for this step and the next. An unchecked box means *don't look and don't ask*: skip that source in silence and don't mention it in the review. **If Code is unchecked, skip step 4 entirely**; a week reviewed from the notes alone is the normal case for most people, not a gap.

**If there is no `profile.md`,** fall back to `routines/timeline/sources.md`:

1. Read it. If it lists sources, use exactly those and don't ask again.
2. If it's missing, ask the user **one** plain question: is there anything outside their notes this review should read (code repositories, for example)? Accept local checkout paths, hosted forge accounts (host + username), or "nothing". That last answer is common, and just skips step 4.
3. Write the answer back to `routines/timeline/sources.md` (create it if missing; append, never rewrite) so later runs stop asking. Ask again only when the user says the list is stale or a call reveals a source that no longer exists.

The file is plain markdown the user can edit at any time. Two lists explicitly emptied with `_None._` are an **answer**: treat the question as settled and never raise it again.

```markdown
# Timeline Sources

Where the Wrap and Week Review rituals look for work outside your notes.
Edit freely.

## Local checkouts
- ~/dev/webapp
- ~/dev/platform-api

## Forges
- github.com — user: <login> (`gh`)
- gitlab.example.com — user: <login>, id: <numeric id> (`glab`)
```

## 4. Collect the week's pull and merge requests

**Only when the profile checks Code** (or, with no profile, when `sources.md` actually names sources). Otherwise skip to step 5 without a word.

Query only the sources from step 3. Each entry ends up with a repo, number, title, and status (open / merged / closed; reviewed when the user reviewed it rather than authored it). Deduplicate across sources. If a call fails with an auth error, say so in the output and tell the user which login to re-run (`gh auth login`, `glab auth login --hostname <host>`) — never drop a source silently.

**Local checkouts** — commits that never became a PR still count:
```bash
git -C <path> log --author="$(git -C <path> config user.email)" \
  --since=YYYY-MM-DD --until=YYYY-MM-DD --oneline
```

**GitHub (`gh`)** — only when a GitHub account is recorded:
- Created: `gh search prs --author=@me --created=YYYY-MM-DD..YYYY-MM-DD`
- Reviewed: `gh search prs --reviewed-by=@me --updated=YYYY-MM-DD..YYYY-MM-DD`
- Merged: `gh search prs --author=@me --merged=YYYY-MM-DD..YYYY-MM-DD`

Narrow with `--owner <org>` or `repo:<owner>/<name>` when the user named specific repositories rather than a whole account.

**GitLab (`glab`)** — only when a GitLab host is recorded. Always prefix calls with that host:

```bash
export GITLAB_HOST=<host from sources.md>
```

The events query below needs the numeric user id — `glab api user | jq .id` once, then record it in `sources.md` so you don't look it up every week.

For a single week the counts are small, so one page (`per_page=100`) is enough — no pagination needed. (Only for multi-week backfills does `glab api --paginate` matter, and it concatenates one JSON array per page; merge with `jq -s 'add'`.)

**Authored MRs** (→ `created`), created in the window:
```bash
glab api "merge_requests?scope=all&author_username=<username>&created_after=YYYY-MM-DDT00:00:00Z&created_before=YYYY-MM-DDT00:00:00Z&per_page=100"
```
Map each: `ref` = `<project-path-tail>!<iid>` (e.g. `platform-api!265`), `title`, `status` = `merged`/`open`/`closed` (GitLab `merged`→`merged`, `opened`→`open`, `closed`→`closed`). Mark `draft: true` when the title starts with `Draft:`/`[Draft]`/`WIP:`.

**Reviewed MRs** (→ `reviewed`): do **not** use `reviewer_username` — it returns MRs merely touched in the window and is noisy. Use real review actions from the events feed:
```bash
glab api "users/<user id>/events?after=YYYY-MM-DD&before=YYYY-MM-DD&per_page=100"
```
Take events with `action_name: "approved"` and `target_type: "MergeRequest"` (optionally also `commented on` a `MergeRequest`/`DiffNote` on someone else's MR). Resolve the project path via `glab api "projects/<project_id>"` (field `path`) to build the `ref`.

Note: `action_name: "accepted"` means the user merged an MR (counts toward `merged` on MRs they authored); `"opened"` means they authored it. Use these to sanity-check the authored list.

**Any other forge** — if `sources.md` names a host with no CLI recipe here, ask the user how to query it (a CLI, an API token, or "skip it"), and add the recipe to `sources.md` for next time.

## 5. Organize the content

1. **Group tasks by area.** When `profile.md` lists areas under **What you track**, those are the names: use them verbatim, and only add a new one when the week genuinely contained something none of them covers. Without a profile, infer short, consistent names from the notes themselves (a developer's week might yield Web App, Platform API, Infrastructure; a teacher's, Lessons, Marking, Studio). Reuse the names earlier weeks already used. Only include what was actually worked on, with no padding. (The dashboard field is still called `projects`; the names in it are whatever the user calls their areas.)
2. **Set the `goal: true` flag** on any project that served a `# Week Goals` item that week. This is the one judgment call the dashboard cannot make itself — it drives the "time sink" warning (share of work outside your goals). Leave it off for projects that were not goals. Never flag `Personal` or `Team` as lingering-exempt yourself; the page handles that.
3. **Pick 2–3 highlights** — the week's most notable accomplishments, in your own words. For an unfinished in-progress week, leave highlights empty.

## 6. Offer unfinished tasks to the backlog

The vault keeps a holding pen — the configured backlog file, `backlog.md` by default — with the sections `## Soon`, `## Someday`, and `## Completed` (the Backlog panel in the bottom dock renders it). After organizing the week you know what stayed unfinished — the unchecked (`- [ ]`) items from the weekly note's goal sections plus tasks the daily notes show lingering across the week:

1. List those unfinished tasks.
2. Ask the user **one** question: move **all** of them to the backlog, **none**, or **some** (the user picks which)? The default destination is **Soon**; the user can say "someday" for any task. If nothing is unfinished, skip this step silently.
3. **Deduplicate before appending:** skip any chosen task whose text already appears in `backlog.md` — compare whitespace-insensitively and ignore any ` ✅ YYYY-MM-DD` completion suffix — in **any** section, `## Completed` included. Report skips as "already in backlog: …".
4. Append the remaining tasks as `- [ ] <task text>` at the end of the matching section. Create `backlog.md` or a missing section heading if needed — never rewrite, reorder, or delete anything already in the file.

Record the moves in the review you write next (e.g. suffix a goal with `→ backlog (Soon)`). Never move a task without the user's answer, and never edit the weekly or daily notes' existing content.

## 7. Write the markdown review

Append (never overwrite) a `# AI Week Review` section at the end of the weekly file, in this format:

```
## Week Review: YYYY-MM-DD to YYYY-MM-DD

### Area Name
- Task or activity completed
- Another task
```

When step 4 ran and found something, add one more section under it:

```
### Pull & Merge Requests
- **webapp#2425** - [search] Add end-to-end stress tests (created, open) [GitHub]
- **platform-api!265** - Optimize metadata search query (created, merged) [GitLab]
- **base-image!18** - Add en_US locale to the base image (reviewed) [GitLab]
```

**Omit that heading entirely when step 4 was skipped or came back empty.** Tag each PR/MR with the forge it came from (`[GitHub]`, `[GitLab]`, or whatever `sources.md` names). Keep it short and factual, in the tone the profile names. No commentary unless asked.

## 8. Append to the dashboard (`weekly/site/data.js`)

The dashboard reads `window.WEEKS` (an array, newest last). Append **one new week object** immediately before the closing `];` of the `window.WEEKS` array. Never rewrite existing entries — new weeks carry their MRs inline with `src` tags.

**First, check `window.PROFILE` at the top of the file matches the vault's profile.** It is what makes the page fit this person:

```js
window.PROFILE = {
  code: false,                      // true when profile.md checks "Code"
  focus: ["Studying", "Teaching"]   // "What you track", in order
};
```

With `code: false` the page drops the pull-request panel, its three stat tiles, and its timeline bar, and shows carried-over goals and personal items instead; `focus` keeps each area the same colour week after week. If the block is missing, add it above `window.WEEKS`. If the user's areas have changed, update `focus`; that list is the only part of `data.js` you may rewrite.

Schema (match this exactly):

```js
{
  id: "2026-W29",                    // week id = weekly filename stem
  week: 29,                          // the WW number
  label: "Week 29",
  range: "Jul 13 – Jul 19, 2026",
  status: "reviewed",                // "reviewed", or "in-progress" if the week isn't over
  goals:     [ { text: "…", done: true } ],   // from # Week Goals
  tentative: [ { text: "…", done: false } ],  // from # Tentative (or [])
  personal:  [ { text: "…", done: false } ],  // from # Personal (or [])
  highlights: [ "…", "…" ],          // 2–3, or [] for an in-progress week
  projects: [
    { name: "Web App", goal: true, tasks: [ "task one", "task two" ] },
    { name: "Personal / Finance", tasks: [ "…" ] }   // omit `goal` when not a week goal
  ],
  prs: {
    created: [
      { ref: "webapp#2425", title: "[search] Add end-to-end stress tests", status: "open",   src: "github" },
      { ref: "platform-api!265", title: "Optimize metadata search query",  status: "merged", src: "gitlab" },
      { ref: "platform-api!263", title: "Draft: Upgrade to Java 21", status: "open", src: "gitlab", draft: true }
    ],
    reviewed: [
      { ref: "webapp#2410", title: "[docs] Add the settings page", src: "github" },
      { ref: "base-image!18", title: "Add en_US locale to the base image", src: "gitlab" }
    ]
  }
}
```

Rules:
- **When the profile doesn't check Code**, still write `prs: { created: [], reviewed: [] }` (the field is part of the schema and the page reads it), and skip the `window.REPOS` rule below entirely.
- Put **every** PR/MR in `prs.created` or `prs.reviewed` with an explicit `src: "github"` or `src: "gitlab"`. `status` applies to `created` entries only.
- For a ref to become a clickable link, its repo short-name must be mapped in `window.REPOS` in `data.js` (`github: { "<short-name>": "<owner>/<repo>" }`, `gitlab: { host: "<host>", paths: { "<short-name>": "<full/project/path>" } }`). Add a row the first time a repository shows up — the same repositories `sources.md` lists. Unmapped refs still render, just as plain text.
- The dashboard computes stats, sparklines, and warnings from this data. It infers **lingering areas** and **carried-over goals** across weeks automatically; you only need accurate `projects` (with `goal` flags), `tasks`, `goals`, and `prs`. Keep the area names stable across weeks so lingering detection works (the page canonicalizes common variants, but consistency helps).
- After editing, verify the file still parses: `node -e "global.window={};require('./weekly/site/data.js');console.log(window.WEEKS.length,'weeks')"`.

## Output

Show the user the markdown review (§7). Mention which tasks (if any) moved to the backlog, that the dashboard entry was appended, and that they can open `weekly/site/index.html` to view it.
