# Thock

This repo is **Thock** — a private fork of the [Zed](https://zed.dev) editor that turns a folder of
plain Markdown files into a guided, LLM-augmented second brain. It is **not** Zed, and work here is not
upstream work. Read `thock/VISION.md` before making product decisions; it is the source of truth for
what this app is and what is already shipped (§12 is the living roadmap).

The short version: a vault is a normal folder on disk. **Routines** (finance, daily/weekly, journaling, team)
are installable bundles defined by a vault-visible `routines/<id>/routine.toml` that scaffold folders,
templates, quick links, and **Skills** — inspectable Markdown rituals the user's LLM runs. Custom GPUI panels
(Routines rail, Day Planner, Backlog, Agent) are the fork's reason to exist, because Zed's extension API
cannot render UI.

_(`AGENTS.md` links to `.rules`, the shorter sibling of this file that every other agent reads. When you change a rule here, mirror it there.)_

## Product invariants

Check design and implementation decisions against these (VISION.md §4 is the full list):

- **Your files, forever, in the open.** Plain Markdown in a normal folder. No proprietary store. The vault
  must still open in any editor if Thock disappears.
- **Augmentation, not replacement.** AI *appends* its synthesis (`# Daily Closure`, `# AI Week Review`); it
  never silently rewrites what the user wrote.
- **Human-in-the-loop where it can't be undone.** Inside the vault the agent acts freely: writes are
  vault-scoped, the user's own words are never changed unless they ask, and a checkpoint precedes every
  session. Confirmation is for what leaves the vault or can't be taken back (money, email).
- **Invisible versioning.** Git runs underneath for history and safety. The word "git" must never appear in
  the UI — it's time-travel, not source control.
- **Everything is editable.** Skills, layouts, prompts, and templates are files the user (or their agent) can
  open and change. Ship great defaults, not hidden behavior.
- **Modular life.** Routines are opt-in and independent. Nothing should assume a particular Routine exists.

## Repo layout and fork discipline

Development happens directly on the fork (`github.com/DiegoTavares/thock`). All Thock code and docs are
isolated so the delta against upstream Zed stays legible and extractable:

- `thock/` — VISION.md and `specs/` (V1–V7). New features get a spec here.
- `crates/thock/` — all Thock Rust: panels, vault model, routines, skills, backlog, history.
- `crates/thock/assets/` — shipped Routine catalog (`routines/`) and core skills (`skills/`).

### Never push to Zed

- **Never push branches to, or open PRs/issues against, `zed-industries/zed` or any upstream remote.** All
  branches and PRs go to `DiegoTavares/thock` only. If a remote named `upstream` exists it is fetch-only,
  for rebases. Verify the remote before any push.
- Do not add Thock credentials, vault paths, or personal data to anything that could reach upstream.

### Keep syncing with upstream cheap

Every line changed outside `crates/thock/` and `thock/` is a future merge conflict. Before editing
an upstream file, ask whether the change can live in the Thock crate instead.

- **Add, don't rewrite.** Prefer new files and new modules over restructuring upstream ones. Prefer
  registering a new panel over changing how the workspace lays out docks.
- **Keep upstream touch-points small and mechanical** — a registration call, a keymap entry, a `Cargo.toml`
  member. Ideally each is a one-liner that re-applies trivially after a rebase.
- **Never reformat, reorder, or "clean up" upstream code**, and don't fix unrelated upstream bugs in passing.
- **Disable rather than delete.** When de-Zed-ifying (git pane, billing surfaces, code-editor chrome), prefer
  hiding/gating behind Thock config over ripping upstream code out.
- Keymap changes go in the existing `assets/keymaps/default-{macos,linux}.json` blocks — add entries, don't
  restructure the file. Keep macOS and Linux in sync.

## Panels — keyboard navigation is mandatory

Every Thock pane must be fully operable without the mouse. A pane that can only be clicked is
incomplete, and this applies to new panes and to any pane being extended.

Each pane must support:

1. **Arrow keys** — `up`/`down` move the selection through rows, `left`/`right` collapse/expand or move
   between columns where the pane has that shape. Use the `menu::` actions (`SelectNext`, `SelectPrevious`,
   `SelectFirst`, `SelectLast`, `Confirm`, `Cancel`) rather than hand-rolled key handling, and add `"menu"`
   to the pane's `KeyContext` so the default bindings apply (see `routines_panel.rs`).
2. **Vim motions when vim mode is enabled** — `j`/`k` (and `h`/`l`, `g g`/`shift-g` where they map naturally)
   must move the selection, bound under the pane's key context gated on vim mode, so a vim user never has to
   reach for the arrow keys.
3. **Shortcuts** — a `ToggleFocus`-style action to reach the pane, plus bindings for its primary operations
   (open, run, mark done, add, remove). Anything a row can do on click needs a keyboard path.

Additional expectations:

- The pane owns a `FocusHandle`, declares a distinct `key_context` (`ThockRoutinesPanel`,
  `ThockBacklogPanel`, `ThockDayPlannerPanel`, `ThockAgentPanel`, …), and keeps a visible
  selection highlight so keyboard focus is never ambiguous.
- Selection must survive re-render and list refresh (vault file events, re-parse) — don't reset to the top
  when the underlying data reloads.
- Every user-visible action needs a real named action so it appears in the command palette and is bindable.
  Routine links and skills are reachable through the generic `thock::OpenLink` / `thock::RunSkill`
  actions — keep new dynamic content bindable the same way rather than inventing per-item actions.
- `escape` returns focus to the editor; toggling focus twice should not trap the user in the dock.

## Rust coding guidelines

* Prioritize code correctness and clarity. Speed and efficiency are secondary priorities unless otherwise specified.
* Do not write organizational comments or comments that summarize the code. Comments should only explain "why"
  the code is written in some way, when there is a non-obvious reason.
* Prefer implementing functionality in existing files unless it is a new logical component. Avoid creating many small files.
* Avoid using functions that panic like `unwrap()`, instead use mechanisms like `?` to propagate errors.
* Be careful with operations like indexing which may panic if the indexes are out of bounds.
* Never silently discard errors with `let _ =` on fallible operations. Always handle errors appropriately:
  - Propagate errors with `?` when the calling function should handle them
  - Use `.log_err()` or similar when you need to ignore errors but want visibility
  - Use explicit error handling with `match` or `if let Err(...)` when you need custom logic
  - Example: avoid `let _ = client.request(...).await?;` - use `client.request(...).await?;` instead
* When implementing async operations that may fail, ensure errors propagate to the UI layer so users get
  meaningful feedback. A failed vault write or skill run must surface to the user, never fail silently.
* Never create files with `mod.rs` paths - prefer `src/some_module.rs` instead of `src/some_module/mod.rs`.
* When creating new crates, prefer specifying the library root path in `Cargo.toml` using `[lib] path = "...rs"`
  instead of the default `lib.rs`, to maintain consistent and descriptive naming (e.g., `gpui.rs` or `main.rs`).
* Avoid creative additions unless explicitly requested.
* Use full words for variable names (no abbreviations like "q" for "queue").
* Use variable shadowing to scope clones in async contexts for clarity, minimizing the lifetime of borrowed references.
  Example:
  ```rust
  executor.spawn({
      let task_ran = task_ran.clone();
      async move {
          *task_ran.borrow_mut() = true;
      }
  });
  ```

### Vault I/O

* The vault is user data that may be open in another editor at the same time. Reads and writes go through the
  project `Fs`, not `std::fs`, so file events and tests behave.
* Writes to user notes are **append-or-insert-section**, never whole-file rewrites, unless the user explicitly
  asked for an edit. Create-if-missing is the norm for daily/weekly notes.
* Never assume a file, folder, or Routine exists — a vault is hand-editable and half-migrated states are normal.
  Missing content is an empty state to render, not an error to panic on.

## Timers in tests

* In GPUI tests, prefer GPUI executor timers over `smol::Timer::after(...)` when you need timeouts, delays, or to drive `run_until_parked()`:
  - Use `cx.background_executor().timer(duration).await` (or `cx.background_executor.timer(duration).await` in `TestAppContext`) so the work is scheduled on GPUI's dispatcher.
  - Avoid `smol::Timer::after(...)` for test timeouts when you rely on `run_until_parked()`, because it may not be tracked by GPUI's scheduler and can lead to "nothing left to run" when pumping.

## GPUI

GPUI is a UI framework which also provides primitives for state and concurrency management.

### Context

Context types allow interaction with global state, windows, entities, and system services. They are typically passed to functions as the argument named `cx`. When a function takes callbacks they come after the `cx` parameter.

* `App` is the root context type, providing access to global state and read and update of entities.
* `Context<T>` is provided when updating an `Entity<T>`. This context dereferences into `App`, so functions which take `&App` can also take `&Context<T>`.
* `AsyncApp` and `AsyncWindowContext` are provided by `cx.spawn` and `cx.spawn_in`. These can be held across await points.

### `Window`

`Window` provides access to the state of an application window. It is passed to functions as an argument named `window` and comes before `cx` when present. It is used for managing focus, dispatching actions, directly drawing, getting user input state, etc.

### Entities

An `Entity<T>` is a handle to state of type `T`. With `thing: Entity<T>`:

* `thing.entity_id()` returns `EntityId`
* `thing.downgrade()` returns `WeakEntity<T>`
* `thing.read(cx: &App)` returns `&T`.
* `thing.read_with(cx, |thing: &T, cx: &App| ...)` returns the closure's return value.
* `thing.update(cx, |thing: &mut T, cx: &mut Context<T>| ...)` allows the closure to mutate the state, and provides a `Context<T>` for interacting with the entity. It returns the closure's return value.
* `thing.update_in(cx, |thing: &mut T, window: &mut Window, cx: &mut Context<T>| ...)` takes a `AsyncWindowContext` or `VisualTestContext`. It's the same as `update` while also providing the `Window`.

Within the closures, the inner `cx` provided to the closure must be used instead of the outer `cx` to avoid issues with multiple borrows.

Trying to update an entity while it's already being updated must be avoided as this will cause a panic. This
bites hardest when a panel action reaches back into the `Workspace` that is already being updated — defer with
`cx.defer` in those paths.

`WeakEntity<T>` is a weak handle. It has `read_with`, `update`, and `update_in` methods that work the same, but always return an `anyhow::Result` so that they can fail if the entity no longer exists. This can be useful to avoid memory leaks - if entities have mutually recursive handles to each other they will never be dropped.

### Concurrency

All use of entities and UI rendering occurs on a single foreground thread.

`cx.spawn(async move |cx| ...)` runs an async closure on the foreground thread. Within the closure, `cx` is `&mut AsyncApp`.

When the outer cx is a `Context<T>`, the use of `spawn` instead looks like `cx.spawn(async move |this, cx| ...)`, where `this: WeakEntity<T>` and `cx: &mut AsyncApp`.

To do work on other threads, `cx.background_spawn(async move { ... })` is used. Often this background task is awaited on by a foreground task which uses the results to update state.

Both `cx.spawn` and `cx.background_spawn` return a `Task<R>`, which is a future that can be awaited upon. If this task is dropped, then its work is cancelled. To prevent this one of the following must be done:

* Awaiting the task in some other async context.
* Detaching the task via `task.detach()` or `task.detach_and_log_err(cx)`, allowing it to run indefinitely.
* Storing the task in a field, if the work should be halted when the struct is dropped.

Storing a task in a field cancels the previous one on replace — that is wrong for effects that must all run
(for example one task per vault file event). Detach those or keep them in a collection.

A task which doesn't do anything but provide a value can be created with `Task::ready(value)`.

### Elements

The `Render` trait is used to render some state into an element tree that is laid out using flexbox layout. An `Entity<T>` where `T` implements `Render` is sometimes called a "view".

Example:

```
struct TextWithBorder(SharedString);

impl Render for TextWithBorder {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().border_1().child(self.0.clone())
    }
}
```

Since `impl IntoElement for SharedString` exists, it can be used as an argument to `child`. `SharedString` is used to avoid copying strings, and is either an `&'static str` or `Arc<str>`.

UI components that are constructed just to be turned into elements can instead implement the `RenderOnce` trait, which is similar to `Render`, but its `render` method takes ownership of `self` and receives `&mut App` instead of `&mut Context<Self>`. Types that implement this trait can use `#[derive(IntoElement)]` to use them directly as children.

The style methods on elements are similar to those used by Tailwind CSS.

If some attributes or children of an element tree are conditional, `.when(condition, |this| ...)` can be used to run the closure only when `condition` is true. Similarly, `.when_some(option, |this, value| ...)` runs the closure when the `Option` has a value.

Prefer the existing `ui` crate components (`ListItem`, `Label`, `Icon`, `Button`) over bespoke `div()` trees,
so Thock panels inherit the theme and match the rest of the app.

### Input events

Input event handlers can be registered on an element via methods like `.on_click(|event, window, cx: &mut App| ...)`.

Often event handlers will want to update the entity that's in the current `Context<T>`. The `cx.listener` method provides this - its use looks like `.on_click(cx.listener(|this: &mut T, event, window, cx: &mut Context<T>| ...)`.

### Actions

Actions are dispatched via user keyboard interaction or in code via `window.dispatch_action(SomeAction.boxed_clone(), cx)` or `focus_handle.dispatch_action(&SomeAction, window, cx)`.

Actions with no data defined with the `actions!(some_namespace, [SomeAction, AnotherAction])` macro call. Otherwise the `Action` derive macro is used. Doc comments on actions are displayed to the user.

Action handlers can be registered on an element via the event handler `.on_action(|action, window, cx| ...)`. Like other event handlers, this is often used with `cx.listener`.

Thock actions live in the `thock` namespace and doc comments become the command-palette description,
so write them for a note-taker, not an engineer.

### Notify

When a view's state has changed in a way that may affect its rendering, it should call `cx.notify()`. This will cause the view to be rerendered. It will also cause any observe callbacks registered for the entity with `cx.observe` to be called.

### Entity events

While updating an entity (`cx: Context<T>`), it can emit an event using `cx.emit(event)`. Entities register which events they can emit by declaring `impl EventEmitter<EventType> for EntityType {}`.

Other entities can then register a callback to handle these events by doing `cx.subscribe(other_entity, |this, other_entity, event, cx| ...)`. This will return a `Subscription` which deregisters the callback when dropped.  Typically `cx.subscribe` happens when creating a new entity and the subscriptions are stored in a `_subscriptions: Vec<Subscription>` field.

### Panels

New docked panes implement the `Panel` trait and are registered with the workspace.

* Each panel needs a unique `activation_priority`; upstream already uses 0–10, so pick above that and check
  the other Thock panels before choosing.
* Panel construction and any action handler that reaches the workspace can double-lease it. Use `cx.defer`
  when opening items or mutating the workspace from inside a panel update.

## Build

- Use `./script/clippy` instead of `cargo clippy`. Scope it to what you changed (`-p thock`) when
  iterating — a full-workspace build is slow and fills the disk with incremental artifacts.
- Prefer `cargo test -p thock` for Thock work; run wider tests only when touching shared crates.
- To see a change in the real app, ask the user to drive the GUI — set it up and launch, but don't automate
  clicks.

## Specs

Non-trivial features get a spec in `thock/specs/` (`vN-<slug>.md`) before implementation, following the
existing ones. When a feature ships, update its roadmap entry in `thock/VISION.md` §12 in the same
change — status there must reflect code on `main`, not intent.

## Pull request hygiene

PRs go to `DiegoTavares/thock` — never upstream.

- Use a clear, correctly capitalized, imperative title (for example, `Add keyboard navigation to the Backlog panel`).
- Prefix with `thock:` when the Thock crate or docs are the scope, matching existing history.
- Avoid conventional commit prefixes (`fix:`, `feat:`, `docs:`) and trailing punctuation.
- Call out explicitly in the body any file touched **outside** `crates/thock/` and `thock/`, and why
  it couldn't be avoided — that section is the rebase risk.
- Include a `Release Notes:` section as the final section, with one bullet:
  - `- Added ...`, `- Fixed ...`, or `- Improved ...` for user-facing changes, or
  - `- N/A` for docs-only and other non-user-facing changes.
- Format release notes exactly with a blank line after the heading:

```
Release Notes:

- N/A
```

## Rules hygiene

This file and `crates/thock/.rules` are read by every agent session — keep them high-signal. Don't edit
them inline during feature work; propose additions under a **"Suggested .rules additions"** heading in the PR
description instead. A new rule must be non-obvious, repeatedly encountered, and specific enough to act on.
Rules that apply only to the Thock crate belong in that crate's own `.rules`, not here. Rules are
**traps to avoid**, not maps of the architecture — architecture belongs in `thock/specs/`.
