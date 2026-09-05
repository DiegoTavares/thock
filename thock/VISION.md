# Thock — Product Vision (v0.1)

> **Working tagline:** _Your private second brain, powered by the LLM you already trust._
> _(alternatives to workshop below)_

**Status:** Discussion draft — for the founding design + engineering team.
**Author:** Diego · **Date:** 2026-07-20

**Decisions so far:**
- **Naming (2026-08-07):** the installable life-domain bundle is a **Routine** (formerly "Area" — always a placeholder). A Routine packages a *practiced rhythm* — rituals, their files, their views — and since V7 it is defined by a vault-visible `routines/<id>/routine.toml` the user or their agent can author. Specs V3–V6 keep "Area" as historical record; read *Routine* wherever they say *Area*. See `specs/v7-dynamic-routines.md`.
- **Fork strategy:** _fork required — scope now known._ A custom left-nav pane (Timeline) is a V1 must-have, and Zed's extension API **cannot render any custom panel/dock/UI** (confirmed against the extension API source, docs, and maintainer statements — see §7.1). Panels are core Rust/GPUI. So Thock is a **fork whose custom surface is a small set of new GPUI panels + an invisible-git service**, with the AI rituals riding the *existing* extension + MCP rails (those need no fork). "Prototype first" still holds — but the prototype is a minimal fork, not an extension.
- **v1 audience:** _technical-first_ — ship rough and powerful for engineers who already live in editors; onboarding polish comes later. (§10 Q4)
- **Repo layout:** _single repo — the fork is the product._ Development happens directly on the Zed fork (`github.com/DiegoTavares/thock`, cloned to `~/dev/thock`), **not** a submodule. All non-Zed content (this doc, design docs, Routine packages) lives isolated under `/thock/` so the fork's delta against upstream stays legible and is trivially extractable later (`git filter-repo`). `upstream` remote → `zed-industries/zed` for ongoing rebases. The personal vault (`~/dev/bread-paper`) stays **out** of this repo — private data, separate concern.

---

## 1. The one-sentence pitch

Thock is a desktop app — a private fork of the [Zed](https://zed.dev) editor — that turns a folder of plain Markdown files into a **guided, LLM-augmented second brain**. It ships with pre-built "Routines" (finance, weekly reviews, journaling, team notes) that each come with their own files, layout, and AI rituals, so a person gets the power of a hand-tuned Obsidian-plus-Claude-Code setup **without having to build it themselves.**

## 2. Why this, why now

Today the author runs a system that most people would love to have but almost nobody can assemble:

- A plain-text vault (Obsidian's format) edited in a fast, real code editor (Zed).
- A set of **LLM rituals** — "Friday finance," "week review," "daily closure," "journaling topic" — that read live data (Monarch, GitHub, GitLab), synthesize it, and **append** their findings to human-written notes.
- **Living source-of-truth files** (e.g. `finance_plan_2026.md`) the AI must read before it advises and must update when reality changes, so guidance never drifts.
- A **static HTML dashboard** the LLM feeds with structured data, which then computes its own warnings (time-sinks, lingering projects, carried-over goals).

The problem: this stack is held together by conventions in a `CLAUDE.md`, four slash-command files, two MCP servers, and a folder-naming discipline that only their author fully understands. It is powerful and completely non-transferable. Obsidian is open but generic; Zed is fast but is a code editor with no notion of "your life"; Claude Code is capable but unopinionated. **Nobody ships the opinion.**

Thock is that opinion, productized: the folder structure, the rituals, the layouts, and the AI guardrails become **first-class, visible, editable features** instead of tribal knowledge.

## 3. Who it's for

- **Primary (v1):** technically comfortable people who already keep notes in plain text and want AI woven into their life-admin — but don't want to hand-build the plumbing. "Power journalers," indie hackers, engineers, PKM enthusiasts.
- **Aspirational (later):** anyone who wants a private, local-first "life OS" and is willing to bring their own LLM key. Non-technical users reached through onboarding that hides the machinery.

**Non-goal:** competing with Obsidian as a general note-taking platform, or with Notion as a team wiki. Thock is opinionated and personal by design.

## 4. Principles (the soul of the product)

These are the invariants. Design and engineering decisions should be checkable against them.

1. **Your files, forever, in the open.** Everything is plain Markdown (or whatever format the user prefers) in a normal folder on disk. No proprietary database, no lock-in. If Thock vanished tomorrow, the vault still opens in any editor.
2. **Augmentation, not replacement.** The AI *appends* its synthesis alongside your raw words — it never silently rewrites what you wrote. Your capture and the machine's reflection coexist in the same file (`# LLM Review`, `# AI Week Review`, `# Friday Finance`).
3. **Bring your own brain.** The user chooses and pays for their own LLM (Claude, local model, etc.) via their own key or a console integration. Thock is not a subscription reseller of intelligence.
4. **Human-in-the-loop for anything that matters.** The AI computes and recommends; the human acts. It will tell you exactly how much to pay down your line of credit — it will not (and cannot) move the money.
5. **Living plans over frozen advice.** Canonical files are the source of truth. The AI reads them before advising and edits them when reality shifts, so the plan never drifts from the person.
6. **Modular life.** Nobody wants every module. Routines are opt-in. A user can run only daily notes, or add finance, journaling, team notes — each independently.
7. **Invisible versioning.** Git runs underneath for full history and safety, but the user never types a git command or sees a git pane. Time-travel, not source control.
8. **Everything is editable.** Skills, layouts, prompts, and templates are files the user (and their LLM) can open and change. Power users can rewrite the rituals; the app just ships great defaults.

## 5. The product experience

Thock looks like a focused, three-pane writing environment. Zed's speed and editing quality are the foundation; the chrome around it is re-conceived for life-management rather than code.

### 5.1 Left rail A — **Timeline** (the "now" navigator)
A small, always-present list of the files you almost always want: **Today**, **Yesterday**, **This Week**, **Last Week**. One click (or keystroke) opens the right note. It resolves the app's naming conventions for the user (daily = `YYYY-MM-DD.md`, weekly = ISO week `YYYY-Www.md`, e.g. `2026-W30.md`) so they never think about filenames. Creating today's note if it doesn't exist yet is a single action — replacing the current "open Obsidian just to trigger a plugin" workaround. _(Since V7 these rows are the Timeline Routine's own templated quick links — the panel is purely sections-per-Routine — while note creation and the `thock: open …` actions stay core.)_

### 5.2 Left rail B — **Routines** (the modular navigator)
A switchable list of the life-domains the user has enabled: _Daily & Weekly_, _Finance_, _Journaling_, _Team_, etc. Each Routine is a bundle of folders, templates, quick links, a right-pane context view, and skills — defined by a vault-visible `routines/<id>/routine.toml` (V7), so the app-shipped catalog is just one way a Routine gets there: the user's own agent can author one directly in the vault. Users add or remove Routines from a gallery. Beneath the Routines view, the full file tree remains available for people who want to roam freely.

### 5.3 Right rail — **Context** (page-aware companion)
The right pane changes with the open document:

- On a **daily note** → a time-block view of the day (a day-planner rail).
- On a **weekly note** → the week's calendar with meetings and important markers.
- On a **finance** file → the current dashboard: accounts, budgets, the computed sweep and LoC residual.
- On any file → the relevant **skills** for that context, one click away.

This is where Thock stops feeling like a text editor and starts feeling like an instrument tuned to the thing you're doing.

### 5.4 **Skills view** — the rituals, made visible and editable
Every Routine exposes its skills as first-class, inspectable objects, not hidden slash-commands. Example skills, drawn directly from the author's working setup:

| Skill | What it does |
|---|---|
| **Friday Finance** | Pulls live Monarch data, computes the credit-card sweep and line-of-credit residual, presents an ordered action list, waits for the user to actually move the money, then logs what happened into the day's note. |
| **Week Review** | Reads the week's daily notes, aggregates GitHub PRs + GitLab MRs, groups work by project, picks highlights, appends an AI review to the weekly file **and** feeds the dashboard. |
| **Daily Closure** | Reads checked/unchecked tasks, pulls the day's commits, scans recent days for multi-day context, and appends a review with suggestions. |
| **Journaling Topic** | Analyzes weeks of notes to detect avoidance/momentum and surfaces a neglected topic to write about. Read-only — the human owns the reflection. |

Each skill is openable, has a plain-language description, a prompt/logic body the user or their LLM can edit, and clear declarations of **what it reads** (data sources) and **what it writes** (which files, append vs. edit). Trust comes from that transparency.

### 5.5 **Onboarding** — teaching what's possible
A first-run flow that (a) points Thock at a new or existing folder, (b) connects an LLM, (c) lets the user pick their starting Routines from a gallery, and (d) walks them through their first ritual (e.g. create today's note, run a daily closure). The goal is that within ten minutes a new user has done one real, valuable thing — not stared at a blank editor.

### 5.6 **Backlog** — the holding pen
_(added 2026-07-24 — not part of the original plan)_ A bottom-dock checklist over a plain `backlog.md` with three sections: **Soon**, **Someday**, and a dated **Completed** history. The daily/weekly wrap rituals offer to move unfinished tasks there (always confirming — all, none, or some), so lingering work stops dying in yesterday's note. Checking an item off in the panel records it as done in today's note and files it under Completed with the date. Spec: `specs/v6-backlog.md`.

### 5.7 **Inbox** — the front door
_(added 2026-08-23)_ Everything Thock knows arrives through a keyboard at a desk; away from it, a thought
either survives until you sit down or it doesn't. The **Inbox** Routine gives the vault a front door: an
`inbox/` folder anything can write a note into — a link shared into Google Tasks from a phone, an email
labeled `thock/inbox`, a drag from Finder, a file written by hand — and a **Triage Inbox** ritual that
proposes a destination for each item (a Soon or Someday task, an append to today's note, a note filed into
a folder, or "leave it") and files only what the user confirms, logging every move to an append-only triage
log. Capture is dumb, instant and thumb-sized; triage is deliberate, assisted, and at the desk — nothing
decides anything on the phone. A watched drop directory outside the vault (iCloud Drive / Syncthing) is the
deferred third transport. Spec: `specs/v13-inbox-routine.md`.

## 6. Relationship to Zed — kept / removed / added

**Kept**
- The editor core: speed, multi-format editing, full file-tree access, Markdown as the default.
- Zed's AI integration path that talks to external models via console/agent, so users bring their own LLM.

**Removed / disabled (initially)**
- The subscription-gated AI/billing model — Thock users bring their own key; no reselling of intelligence.
- The Git pane and manual git surface — versioning becomes invisible (see §7).
- Editor chrome and affordances that assume "you are writing software," where they conflict with the life-OS framing.

**Added**
- The **Routines** left rail (V7: one section per enabled Routine — quick links like Today / Yesterday / This Week / Last Week, then skills) + gallery / enable-disable.
- The page-aware **Context** right rail (time blocks, week calendar, finance dashboard).
- The **Skills view** (inspect + edit rituals; declared read/write scopes).
- The **Backlog** bottom panel — Soon / Someday / Completed over `backlog.md`, fed by the wrap rituals.
- **Onboarding** flow.
- **Invisible git** automation.
- A **Routine package format** — the `routine.toml` bundle (folders + templates + quick links + skills + docs) that makes a domain installable, whether shipped in the catalog or authored in the vault by the user's agent.

## 7. Technical shape (for the engineers)

_High-level and provisional — meant to frame feasibility, not prescribe implementation._

### 7.1 Settled constraint: the panes require a fork (Zed extensions can't render UI)

Confirmed 2026-07-20 against primary sources (Zed's `crates/extension_api` trait, `docs/src/extensions`, and maintainer statements): **the Zed extension API is entirely non-visual.** Extensions can contribute language servers, themes, slash commands, and MCP/context servers — but there is **no** method to render a custom panel, dock, view, or webview, in stable or nightly. All panels (project tree, outline, terminal, agent, git) are compiled Rust/GPUI inside the core, registered via a `Panel` trait not exposed to extensions; WASM extensions are sandboxed with no handle to the window.

Consequence for Thock — a clean split:

- **Requires touching core (fork):** the Timeline/Routines pane, the page-aware Context pane — each is a new GPUI `Panel` registered in the workspace dock. Plus the invisible-git background service.
- **Does _not_ require a fork:** the AI rituals. Daily Closure, Week Review, Friday Finance, and the Monarch/GitHub/GitLab connectors fit the existing **extension + MCP** model and can load into our fork as ordinary Zed extensions.

Design implication: keep the fork's custom surface **small and panel-shaped**, and push as much logic as possible into extensions/MCP so we stay mergeable with upstream. The relevant upstream hope — RFC #53403 "Visual Extension API" (Apr 2026) — is maintainer-gated and explicitly deprioritized, so it must not be counted on.

_Source pointers:_ `zed-industries/zed` `crates/extension_api/src/extension_api.rs`; `docs/src/extensions/developing-extensions.md`; Discussion #53403; Issues #17325, #18877, #21208.

### 7.2 Building blocks

- **Base:** private fork of Zed (Rust + GPUI). We inherit a fast, native, cross-platform editor. Risk: staying mergeable with upstream vs. diverging — mitigated by §7.1's small-fork/large-extension split.
- **Vault = folder on disk.** No new storage engine. Conventions (naming, PARA-style folders) are encoded in the app so the user doesn't maintain them by hand.
- **Routines as packages.** A Routine is a declarative bundle: folder scaffolding + templates + quick links + a set of skills + docs, defined by a vault-visible `routines/<id>/routine.toml` (V7). Installing (or activating a vault-authored) Routine writes its scaffolding into the vault, records hash-lockfile provenance, and registers its links/skills. This is the key extensibility primitive — and since V7 the user's own agent can author new Routines directly in the vault, no rebuild required.
- **Skills = portable, declarable rituals.** Today they're Claude Code slash-commands with implicit behavior. In Thock a skill declares its **inputs** (files, MCP data sources), its **actions**, and its **outputs** (which files, append vs. edit) so the UI can show scope and the app can sandbox writes. The runtime executes them through the user's chosen LLM.
- **Data connectors via MCP.** Monarch, GitHub/GitLab, calendar, etc. arrive as MCP servers (the author already runs Obsidian + Monarch MCP). Thock should make connecting an MCP source a first-class, guided step rather than hand-edited JSON.
- **Invisible versioning.** A background service commits meaningful checkpoints (autosave/idle/pre-AI-write) to a hidden git repo, exposes a human "history / restore this version" UI, and surfaces conflict recovery — all without the word "git" ever appearing.
- **Dashboards as an output type.** The `structured data (data.js) → static HTML that computes its own analytics` pattern is a repeatable Routine capability: skills emit machine-readable feeds; a bundled viewer derives insight. Worth generalizing into the Routine format.

## 8. Why it's valuable

- **It sells an opinion, not a blank canvas.** The hard part of PKM isn't the tool — it's designing the system. Thock ships proven systems. That's the differentiated value Obsidian/Notion/Zed structurally can't offer.
- **Local-first + BYO-LLM is a real position.** Privacy-conscious, lock-in-averse users are underserved by cloud note apps. "Your files, your model, your machine" is a clear, honest promise.
- **The rituals compound.** Value grows the longer you use it — weeks of notes make the week-review and journaling skills smarter. That's retention that doesn't depend on a walled garden.
- **A genuine wedge exists:** people already cobbling Obsidian + Claude Code together (a visibly growing crowd) are proof the demand is real and currently unmet by a polished product.

## 9. Feasibility — the honest read

**Encouraging**
- The concept is already **de-risked by a working prototype**: the author's own vault _is_ Thock minus the packaged UX. We're productizing a proven workflow, not inventing an unproven one.
- Zed gives us a world-class editor for free. The genuinely new surface area is chrome + orchestration, not a text engine.
- Markdown-on-disk means low storage/architecture risk and instant interop.

**Hard parts to respect**
- **Forking Zed is a serious commitment.** Rust + GPUI is a real codebase; keeping a private fork current with upstream is ongoing tax. We should decide early: deep fork vs. thin layer (extension/overlay) vs. building panes as Zed extensions where possible. This is the single biggest architectural fork-in-the-road.
- **Invisible git is deceptively subtle.** Autosave churn, merge conflicts, large binaries (the vault already holds multi-MB images), and "restore" UX are all edge-case minefields. Getting "never lose data, never show git" right is a project of its own.
- **Skills need a real trust + safety model.** The moment an AI can write to a user's files and read financial data, scope declarations, dry-runs, previews, and confirmation gates stop being nice-to-haves.
- **BYO-LLM UX is fiddly.** Keys, model choice, local vs. cloud, cost visibility, and graceful failure need thought so non-experts aren't stranded.
- **Onboarding a non-technical user into a fork of a code editor** is a real design challenge — the gap between "engineer's dream" and "my mom could use it" is wide, and v1 should pick a lane honestly.

**Provisional recommendation:** Build the **thinnest thing that proves the core loop** first — Timeline rail + one Routine (Daily/Weekly) + one working skill (Daily Closure) + invisible git — on top of Zed, before committing to the full Routines/Skills package framework. Treat it as a personal tool that earns its way to being a product.

## 10. Open questions for the team

1. **Fork depth:** deep Zed fork, thin overlay, or extension-based? What keeps us mergeable with upstream long enough to matter?
2. **Routine package format:** what's the minimum declarative spec for a bundle (folders + views + skills + connectors)? _(Largely answered by V3/V7: `routine.toml` schema 2; connectors and view specs still open.)_
3. **Skill contract:** how do we declare/enforce a skill's read/write scope so users can trust it and the app can sandbox it?
4. **Audience for v1:** technical-first (ship rough, powerful) or approachable-first (invest in onboarding early)? These pull the design in different directions.
5. **Invisible git:** what exactly triggers a checkpoint, and what does "restore" look like to someone who's never heard of a commit?
6. **Distribution & model:** open-source core? paid Routines? one-time vs. subscription (for the app, never the intelligence)?
7. **The name & tagline:** does "Thock" land, and how do we say the value in one line? (see below)

## 11. Tagline candidates (to workshop)

- _Your private second brain, powered by the LLM you already trust._
- _The opinionated second brain. Your files, your model, your machine._
- _Plain text in. Clarity out. Your life, with an AI that actually knows it._
- _A second brain that ships with a system — not a blank page._
- _Local-first life OS. Bring your own brain._

## 12. Feature roadmap (living)

> This section is the running build log. It is **updated over the course of the project** as features move `planned → in progress → shipped`. Status reflects code on the `main` fork, not intent.

### Milestone 0 — Fork foundation
- [x] **Fork Zed, isolate Thock delta** — `/thock/` docs + `crates/thock/`, `upstream` remote for rebases. _(shipped)_
- [x] **Vault model** — folder + `.thock` marker + config, naming conventions encoded. _(shipped)_
- [x] **Timeline panel** — Today / Yesterday / This Week / Last Week GPUI dock panel. _(shipped)_
- [x] **Daily & weekly note creation** — resolve `YYYY-MM-DD.md` / ISO week `YYYY-Www.md`, create-if-missing. _(shipped)_
- [ ] **Invisible git — checkpoint service** — background snapshots to hidden `.thock/history` git-dir. _(in progress)_

### Milestone 1 — The core loop (thinnest thing that proves it)
- [x] **Daily & Weekly Routine** — first packaged Routine, shipped as the installable **Timeline** bundle (scaffolded folders + weekly dashboard + Week Review skill; the daily note's page-aware context view shipped later as the Milestone 3 Day Planner rail). _(shipped)_
- [x] **Daily Closure skill** — shipped as the Timeline Routine's **Wrap Today / Wrap Yesterday** skills: read the day's tasks, pull its commits (`gh` / `glab` / local git), scan the prior few daily notes for multi-day context, and append a `# Daily Closure` review to the day's note — append-only, never rewriting what the user wrote. _(shipped)_
- [ ] **Invisible git — restore UI** — human "history / restore this version" surface; no git vocabulary. _(planned)_
- [ ] **Checkpoint triggers** — autosave / idle / pre-AI-write commit points. _(planned)_
- [x] **BYO-LLM connection** — ride Zed's existing agent/console rails; user brings their own key.
- [x] **Backlog pane & capture** — `backlog.md` (Soon / Someday / dated Completed) + a bottom-dock editable checklist panel: inline task editing, Soon ↔ Someday moves, reveal-in-file; mark-done appends to today's note (create-if-missing) then files the task under Completed with the date. The Timeline Routine's wrap skills (v2) offer unfinished tasks for the backlog — all / none / some, user-confirmed, deduplicated. Fully keyboard-operable: arrows and vim motions across the three columns, with `i` / `o` / `space` / `<` `>` / `y y` / `g space` for edit, add, complete, move, copy and reveal (spec §6.6, added 2026-08-17). V17 added optional **categories**: a `###` heading inside Soon or Someday becomes a named, collapsible group in that column, tasks written above the first one stay loose at the top, adds land in the group they were started from, a Soon ↔ Someday move carries its category (creating the heading when the destination lacks it), and the closed set is remembered across restarts (spec `specs/v17-backlog-categories.md`, added 2026-09-02). Added 2026-07-24, spec `specs/v6-backlog.md`. _(shipped)_

- [x] **Concealed markup in the Markdown editor** — a vault note reads like preview without leaving the editor: while the cursor is elsewhere, heading markers, link syntax and HTML comments fold away invisibly, headings take three stable theme colours, `[[wikilink]]` and `[text](url)` labels are coloured (internal vs external visibly distinct), a `- [ ]` draws as a real checkbox, and a `___` line as a rule. The cursor's line always shows its exact source, the buffer is never touched (display-only by construction), and Markdown outside a vault is unaffected. Toggle per editor with **thock: toggle markdown source** (`cmd-alt-m`), default per vault via `[markdown] conceal`, and `g d` / go-to-definition on a `[[wikilink]]` opens the linked note. Spec `specs/v10-markdown-conceal.md`. _(shipped)_

### Milestone 2 — Routines & Skills framework
- [x] **Routine package format** — declarative bundle (folder/file scaffold + skills + surfaces + doc), materialized create-if-missing and recorded in a per-vault registry. Shipped in V3 as a compiled-in `manifest.toml`; V7 inverted the source of truth to a vault-visible `routines/<id>/routine.toml` (schema 2: quick links with date templates and open kinds, icon, `agent_doc`) with hash-lockfile provenance and a `[[routines.installed]]` registry. _(shipped)_
- [x] **Dynamic Routines** — `routine.toml` files appearing in the vault are discovered without a restart, validated with visible errors, and offered for explicit activation; removal deletes only declared files left unmodified since activation. Any Routine link or skill is keybindable via the generic `thock::OpenLink` / `thock::RunSkill` actions. Spec `specs/v7-dynamic-routines.md`. _(shipped)_
- [x] **Agentic Routine authoring** — the core **New Routine** ritual: the user's agent interviews them, writes the `routine.toml` + docs + skills into the vault (`routines/ROUTINES.md` is the self-describing format reference), and a ready marker earns an activation toast. _(shipped)_
- [x] **Routines left rail + gallery** — V7: the panel is purely sections-per-Routine, with **Add Routine** listing the catalog and the "In this vault" discoveries, and remove-with-confirmation that preserves user-modified files. V11 rebuilt the section around *places and verbs*: two captions (**Notes**, **Rituals**) replace the old "Skills" folder row, `[[skill]] kind = "setup"` puts one-time steps behind a collapsed **Setup** row instead of among the daily verbs, `[[link]] group = "…"` demotes rarely-opened destinations into a collapsed row of their own, bound links and rituals render their chord on the row, and removal moved off the header onto hover. A ritual now runs on `enter`/click, with `alt-enter` (`g space`) to read its instructions; `left`/`right` (`h`/`l`) close and open groups. Spec `specs/v11-routines-rail.md`. A standalone gallery UI is still to come. _(shipped)_
- [x] **Skills view** — a Routine's skills are inspectable, openable Markdown files with a plain-language summary; read/write scopes are declared in the manifest. Surfacing those scopes in the UI is still pending. _(shipped)_
- [ ] **Skill contract & write sandbox** — enforce inputs/outputs so writes are previewable and scoped. Scopes are now _declared_ in the manifest but not yet enforced. _(planned)_

### Milestone 3 — Context rail & connectors
- [x] **Page-aware Context right rail — day planner** — first page-aware panel (spec `specs/v4-day-planner-panel.md`): a right-dock Day Planner that follows the active editor item and renders a daily note's checklist as a time-block day grid — timed tasks as duration-scaled blocks in Google-Calendar-style overlap columns, time-less tasks as unscheduled chips, done tasks struck through. Read-only with reveal-on-click into the editor, live re-parse on edit, and a `[day_planner]` config section. Week-calendar and finance-dashboard context views still pending. _(shipped)_
- [x] **Calendar sync into the daily note** — a background service mirrors the day's Google Calendar events into a `## Calendar` subsection of the daily note's `# Day planner`, as ordinary checklist lines the existing panel already renders. OAuth loopback + keychain, day-window polling with conditional requests, and a reconciler that corrects times but never touches a line the user edited and never deletes one. Read-only toward Google; ships inside the Timeline Routine as the Connect Calendar skill, with a status row in the Day Planner panel. Spec `specs/v8-calendar-sync.md`. _(shipped)_
- [x] **Gmail capture into the Backlog** — label an email `backlog` in Gmail (name configurable) and within a poll it becomes an unchecked task under the Backlog's Someday section: title-mode tasks link back to the thread in Gmail, full-mode tasks archive the email's text under `archives/emails/` and carry an inert `[[wikilink]]` to it. One task per thread, read-only toward Google, dedup via `.thock/state/gmail/` with the vault's `<!--gmail:…-->` markers as the rebuildable record. The OAuth flow is unified with calendar sync into a single **Connect Google Workspace** action (one consent, both read-only scopes, one keychain entry), and the Backlog panel gains a capture status row. Spec `specs/v9-gmail-backlog-capture.md`. _(shipped; capture pipeline superseded by V15's unified label → folder sync — title mode and the flat-label fallback retired)_
- [x] **Sectioned Day Planner** — tasks carry the subsection they came from, and each subsection under `# Day planner` renders in its own theme-derived colour, hashed from the section name so it's stable across re-parse and restart (pinnable via `[day_planner.sections]`). Prerequisite for calendar sync. Spec `specs/v8-calendar-sync.md` §11. _(shipped)_
- [x] **Inbox Routine — capture from anywhere, triage at the desk** — a default-installed Routine with an `inbox/` landing zone (a file in it *is* an untriaged item), a **Triage Inbox** ritual that proposes a destination per item and files only what the user confirms — appending tasks or note lines, moving kept notes, deleting what it filed (invisible history plus the append-only `archives/inbox/triage-log.md` make that recoverable and legible) — and an editable `triage-policy.md` written by a six-question **Set Up Inbox** interview. Two read-only mobile transports ride V9's machinery behind an `InboxSource` trait: a Google Tasks list (the account's default, so a phone share sheet just works) and a `thock/inbox` Gmail label, polled by one `InboxService` with dedup in `.thock/state/inbox/` and a queue-depth status row in the Backlog panel. Gmail's labels moved to nested `thock/backlog` / `thock/inbox` (old flat `backlog` honored with a visible rename hint), the unified consent gained `tasks.readonly`, and the Google account consolidated into `.thock/google.toml`. The watched drop directory *outside* the vault (iCloud Drive / Syncthing) is deferred. Spec `specs/v13-inbox-routine.md`. _(shipped; the Gmail transport moved into V15's unified sync — `InboxService` now carries Google Tasks and the landing-zone queue)_
- [x] **Unified Gmail sync — labels route, folders mean** — one pipeline replaces the two parallel Gmail stacks: `.thock/gmail.toml` holds an ordered `[[sync]]` map of Gmail label → vault folder (defaults `thock/backlog` → `archives/emails`, `thock/inbox` → `inbox`; a new pair is a two-line config edit, zero code), the sync service's whole job is landing one V13-format note per thread in the mapped folder, and behavior attaches to the *folder*: notes landing in `archives/emails` get their `- [ ] Title [[stem]]` Someday line appended by a backlog-owned integration hook. One claim pass makes the both-labels fast lane structural, one digest space spans every mapping (V9's old digests honored read-only so nothing is ever captured twice), and title-mode import, its picker, and the flat-label fallback are retired. Spec `specs/v15-unified-gmail-sync.md`. _(shipped)_
- [x] **Email view — reading synced mail as a conversation** — a gmail note (`source: gmail` frontmatter, no new extension) renders as an email in place: machinery frontmatter folded into an envelope (From + Open in Gmail), `## Sender — date` rows drawn as a conversation spine with an own-reply tint, reply bodies and quoted history collapsible via real Zed creases (older replies and quotes collapsed by default), `] m` / `[ m` motions between messages. Display-only on the V10 conceal machinery — bytes on disk never change. Spec `specs/v16-email-view.md`. _(planned)_
- [x] **Week Review skill** — ships with the Timeline Routine: aggregate daily/weekly notes + GitHub PRs (`gh`) / GitLab MRs (`glab`), append an AI review to the weekly note, and feed the dashboard. Rides the `gh`/`glab` CLIs; guided MCP connectors still pending. _(shipped)_
- [x] **Lifestyle Routine — money in the service of a life you named** — the routine that replaces the planned Friday Finance skill with something wider: an editable **Coach** persona (Cal Newport's lifestyle-centric planning, Morgan Housel's psychology of money) that every skill adopts; a ten-question setup interview writing `lifestyle/vision.md`, then — only once real numbers have landed — `lifestyle/plan.md` (cost of the vision → gap → dated targets → weekly mechanics, every number citing the sentence it came from); one source-agnostic data contract over an MCP server, a folder of statements, or plain conversation, normalizing into `lifestyle/site/data.js` as the single numeric record; and a **Money Ritual** that proposes the period's moves from `accounts.md` rules, checks plan drift, ties the week back to the vision, and logs only what the user confirms they did. Read-only toward every source — Thock never moves money. Ships in the Add Routine catalog (not default-installed) with a burn-vs-income Money Dashboard; all prose and HTML in the vault, the only Rust a catalog registration. Spec `specs/v14-lifestyle-routine.md`. _(shipped)_
- [ ] **Journaling Topic skill** — detect avoidance/momentum, surface a neglected topic. Read-only. _(planned)_
- [x] **Dashboard output type** — `data.js → static HTML that computes its own analytics`, shipped as the Timeline Routine's Weekly Dashboard and generalized into the Routine format as an openable browser link. _(shipped)_

### Milestone 4 — Onboarding & de-Zed-ification
- [x] **First-run onboarding** — a fresh vault opens on a *rendered* welcome note; a vault-local
  **Introductory Guide** (`guide/index.html`, the dashboard-output pattern) tells the philosophy in plain
  language; a **Getting started** checklist pinned atop the Routines rail walks read-the-introduction →
  customize (`guide/customize.md` with the live theme selector popped on top — dark is only the default —
  then text size, "⌘⇧P does anything", the panel toggles, and how to rebind, agent included) →
  connect-your-agent → take-the-tour, each step checking off on real evidence and the
  section retiring itself when done (fresh vaults only, state in `.thock/state/getting-started/`); and the **Welcome Tour**
  core ritual has the user's own agent create the first note, demonstrate the append contract, and finish
  with a first Wrap Today. The vault also became agent-agnostic: canonical `AGENTS.md` with
  `CLAUDE.md`/`GEMINI.md` symlinks, slash-command bridges generated for Gemini CLI alongside Claude Code,
  a Gemini fast-tier mapping, and de-Clauded skill copy — a Gemini user gets the full experience. The
  folder picker and a pick-your-Routines step stay deferred. Spec `specs/v18-first-run-onboarding.md`. _(shipped)_
- [x] **Remove code-editor chrome** — the inherited Zed surface is off by default: no sign-in/user menu, no git/collab/debugger/diagnostics/LSP buttons, no tasks, Jupyter or gutter runnables, telemetry and auto-update off, no unconditional phone-home (extension update ping guarded, prettier-for-Markdown off so opening a note downloads nothing). Language servers are opt-in per language (TOML via auto-installed extension, JSON/JSONC/YAML kept for settings editing); the command palette hides the debugger/task/repl/collab/account namespaces; menus list the Thock panels and drop the Run menu and Zed marketing links. All setting-level flips and hides, never code removal, so rebases stay cheap. Spec `specs/v12-de-zed-ification.md`. _(shipped)_
- [x] **Your vault, in your language** — the vault and the agent speak the user's language while the app's
  chrome stays English (stated plainly, not hidden). A **Set Language** ritual (`skills/thock/set-language.md`,
  `thock: set language` in the palette, and the Welcome Tour's new opening question) records the language as a
  binding `## Language` section in `AGENTS.md`, writes `[language]` plus the parsing config, and translates the
  templates, welcome note, customize page, and Routine docs in confirmed batches — shipped skills only as an
  opt-in extra with a stops-receiving-updates warning, existing notes never. Underneath it `[backlog] headings`
  became configurable like `[day_planner] heading`: `SectionKind` split `id()` (stable keys for persisted
  state and element ids) from `heading()` (the configured text), matching falls back to the English defaults so
  half-migrated vaults still parse, and the Backlog panel's columns render the configured headings. The daily
  template retitles to a wordless `YYYY-MM-DD` date; localized month names, chrome i18n, and divergence
  tracking for translated skills stay deferred. Spec `specs/v19-vault-language.md`. _(shipped)_
- [x] **The website, and a door with a code on it** — `thethock.com` is live from the same GCP project as the
  release index: a static file server on Cloud Run built from `thock/site`, `www` redirecting to the apex. The
  landing page keeps its waitlist; `/download` is the only place install links appear, and it shows them to a
  visitor holding an **invite code** — the release-manifest URL sits in `gate.json` sealed under the code
  (PBKDF2 → AES-GCM) and the browser decrypts it, so approval is a human replying to a signup with the code.
  Signups go to the site's own `/waitlist` route, one Firestore document per address, and a Cloud Logging
  alert emails each new one — no third-party form service. Unlocked, the page reads `channels/stable.json` and renders version, per-platform
  downloads, and checksums, so a release is still just a tag. Spec `specs/v21-site-hosting-and-download-gate.md`. _(shipped)_
- [ ] **BYO-LLM cost visibility** — key/model choice, local vs cloud, graceful failure. _(planned)_

---

_This is a starting point, not a spec. It exists to give designers and engineers a shared picture of the destination so we can argue productively about the route._
