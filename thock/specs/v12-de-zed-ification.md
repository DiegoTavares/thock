# Thock V12 — De-Zed-ification

**Status:** Shipped (2026-08-23)
**Owner:** Diego · **Date:** 2026-08-23
**Companion docs:** `../VISION.md` (§4 invariants, §12 roadmap), `v5-agent-and-onboarding.md` (§6.3 established the
"setting-level, never code removal" rule and `disable_ai`)

---

## 1. Summary

Thock inherits everything Zed ships: sign-in, collaboration, a git pane, a debugger, tasks, Jupyter,
telemetry, auto-update, and a fleet of language servers that download compilers' worth of tooling the
moment a file opens. None of that belongs in a Markdown second brain, and some of it actively violates
the product ("git" visible in the UI, phone-home traffic from a private vault).

V12 turns the inherited surface off. It follows the precedent set by V5 §6.3: **flip defaults and hide,
never delete**, so upstream rebases stay cheap. Investigation showed ~90% of the work is default-settings
flips in `assets/settings/default.json` (a file the fork already owns), a further chunk is
command-palette filtering callable from `crates/thock` with zero upstream churn, and only four upstream
files need small (1–8 line) edits.

Decisions locked in with Diego (2026-08-23):

- **AI stays BYO-CLI only.** `disable_ai: true` remains; the Thock Agent panel (user's own CLI agent) is
  the only AI surface. No Zed agent panel, no API keys in Thock — reaffirms V5.
- **LSP survives for TOML, JSON/JSONC, and YAML only.** Markdown ships no LSP in Zed anyway. TOML support
  arrives via the `toml` extension, auto-installed by default.
- **Kept visible:** terminal panel, outline panel, search status button, project panel, and the full
  extension system including the store page.
- **Hidden:** sign-in/user menu, collab panel, git panel, debugger, diagnostics/LSP status buttons,
  tasks, Jupyter/REPL, gutter runnables/breakpoints.
- **Network:** telemetry off, auto-update off, the unconditional extension update ping guarded, the
  default `html` extension auto-install dropped, prettier-for-Markdown off (it was the one path that
  npm-installed prettier — and downloaded a whole Node runtime — just for opening a `.md` file).

## 2. What is already inert (verified, no action)

- A never-signed-in install performs **zero** auth and zero RPC; collab only auto-connects for Zed staff.
- Crash/minidump upload requires `ZED_MINIDUMP_ENDPOINT` at build time — unset in fork builds.
- Billing, plan chips, trial upsells, cloud web search, zeta usage chrome: all dead under
  `disable_ai: true` or gated on a zed.dev provider that is never authenticated.
- Chat panel and notification panel no longer exist in this upstream vintage.
- Feature flags arrive only over the signed-in cloud websocket; no separate fetch.
- Release channel `dev` never polls for updates (but see §3.4 — don't rely on it).

## 3. Changes

### 3.1 Default settings (`assets/settings/default.json`, zero rebase risk)

Each flip carries a short `// Thock:` comment in the style of the existing `disable_ai` block.

| Area | Change |
|---|---|
| Title bar | `show_user_picture`, `show_user_menu`, `show_sign_in` → `false` |
| Collab | `collaboration_panel.button` → `false` |
| Git | `git_panel.button` → `false` |
| Debugger | `debugger.button` → `false` |
| Diagnostics | `diagnostics.button` → `false` |
| LSP status | `global_lsp_settings.button` → `false` |
| Tasks | `tasks.enabled` → `false` |
| Jupyter | `jupyter.enabled` → `false` |
| Gutter | `runnables`, `breakpoints` → `false` |
| Telemetry | `diagnostics`, `metrics` → `false` |
| Updates | `auto_update` → `false` |
| Extensions | `auto_install_extensions`: `{"html": false, "toml": true}` |
| LSP | `enable_language_server` → `false` in `defaults`; re-enabled per-language for JSON, JSONC, YAML (TOML's block comes with the extension, so it is opted in under `languages`) |
| Prettier | `Markdown.prettier.allowed` → `false` |

Settings/Keymap/Themes remain reachable via the macOS menu bar and command palette after
`show_user_menu` goes; the popover's only unique content was account/plan chrome.

### 3.2 Command-palette filtering (`crates/thock`, zero upstream churn)

From `thock::init`, hide namespaces for surfaces that keep their actions registered but should not
appear in the palette: `call`, `channel`, `client`, `collab`, `collab_panel`, `debugger`, `dev`,
`feedback`, `onboarding`, `repl`, `task`, plus the individual action types `zed::OpenOnboarding`,
`zed::OpenAccountSettings`, and `zed::ShowWelcome` (which live in the `zed` namespace, so their
namespace can't be hidden wholesale). Pattern copied from
`agent_ui::update_command_palette_filter` (the `disable_ai` machinery).

### 3.3 Small upstream edits (each called out in the PR body)

1. **`crates/extension_host/src/extension_host.rs`** — guard `check_for_updates` so the
   `api.zed.dev/extensions/updates` request is not fired unconditionally on every launch. On-demand
   store browsing keeps working.
2. **`crates/zed/src/zed/open_listener.rs`** — the last `FIRST_OPEN` path still opens Zed onboarding;
   route it to `thock::open_startup_vault` like `main.rs` already does.
3. **`crates/onboarding/src/basics_page.rs`** — the "Start Free Trial / Sign In" Zed-agent button is not
   gated by `disable_ai`; gate it.
4. **`crates/zed/src/zed/app_menus.rs`** — drop debugger/task menu items, and replace the Help menu's
   Zed links (telemetry, bug report, twitter, "Join the Team") with Thock-appropriate entries.
   "Extensions" stays.

### 3.4 Build config

`script/bundle-mac` exports `ZED_UPDATE_EXPLANATION` so that even a future `stable`-channel build can
never fetch Zed release binaries; the in-app updater then shows the explanation string instead.

## 4. Explicitly not doing (and why)

- **Unregistering panels in `zed.rs`** — highest-churn spot in every rebase; hidden buttons deliver the
  same UX. Revisit only if startup cost of dormant panels ever matters.
- **Trimming grammars / built-in language registrations** — `crates/grammars` and
  `languages/src/lib.rs` are upstream-churn hotspots, and other grammars power syntax highlighting
  inside Markdown code fences. Binary-size savings are not worth the merge tax.
- **Touching `call::init` / `channel::init` / `collab_ui::init`** — `TitleBar::new` panics without
  `ActiveCall::global`, and `collab_ui::init` is what initializes the title bar. With no sign-in they
  build empty stores and render nothing.
- **Blocking Node runtime download** — JSON/YAML servers are npm-based; Node stays lazy and only
  downloads if such a server starts without a system node. With prettier-for-Markdown off, plain
  note-taking never triggers it.
- **Removing the zed.dev model provider registration** — already unreachable under `disable_ai`; code
  removal would only buy conflict surface. Becomes relevant only if the BYO-CLI decision is ever
  reversed.

## 5. Acceptance

- Fresh vault, fresh config dir, no network: launch shows no sign-in affordance, no git/collab/debugger
  buttons, no onboarding, and `lsof`/proxy shows no requests to `*.zed.dev` at startup.
- Opening a `.md` file downloads nothing (no prettier, no Node, no LSP).
- Opening `routine.toml` gets TOML validation (extension auto-installed); `settings.json` editing keeps
  schema completions.
- Command palette shows no `debugger:`, `task:`, `repl:`, `collab:`, `client:` actions.
- `git diff` against upstream outside `crates/thock/` + `thock/` + `assets/` remains ≤ 4 files for this
  feature.
