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
  arrives via the `toml` extension, auto-installed by default. *(Superseded for JSON/JSONC/YAML by §7.)*
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
| LSP | `enable_language_server` → `false` in `defaults`; re-enabled per-language for JSON, JSONC, YAML (TOML's block comes with the extension, so it is opted in under `languages`) — see §7, which since refuses the JSON/JSONC/YAML server binaries |
| Prettier | `Markdown.prettier.allowed` → `false` (§7 extends this to JSON, JSONC and YAML) |

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

## 6. Follow-up: visible strings (2026-09-07)

V12 turned the inherited *surfaces* off but left the inherited *copy* alone, so the word "Zed" kept
showing up wherever a surface Thock still ships renders text. This pass swaps the product name in
every string a Thock user can actually reach:

| Surface | Where |
|---|---|
| Title-bar update banner ("Checking for / Downloading / Installing Thock Update…") | `crates/ui/src/components/collab/update_button.rs` |
| About window headline and window title | `crates/release_channel/src/lib.rs` (`display_name`), `crates/zed/src/zed.rs` |
| macOS app menu (menu title, About / Hide / Quit) | `crates/zed/src/zed/app_menus.rs` |
| "Move to Applications" first-run prompt and progress modal | `crates/zed/src/zed/move_to_applications.rs` |
| Launch-failure dialog and `--system-specs` output | `crates/zed/src/main.rs`, `crates/system_specs/src/system_specs.rs` |
| Install CLI prompts and toast (the symlink was already `thock`) | `crates/install_cli/src/install_cli_binary.rs` |
| Updater "installed via a package manager" prompt, and its install error | `crates/auto_update/src/auto_update.rs` |
| Settings window title and setting descriptions | `crates/settings_ui/src/settings_ui.rs`, `page_data.rs` |
| Empty-pane welcome headline | `crates/workspace/src/welcome.rs` |
| Extensions page feature banners and incompatibility tooltip | `crates/extensions_ui/src/extensions_ui.rs` |
| Default icon theme name ("Thock (Default)") | `crates/theme/src/icon_theme.rs`, `assets/settings/default.json` |
| Header comment seeded into a new `settings.json` | `assets/settings/initial_user_settings.json` |
| Command-palette descriptions for About and the log actions | `crates/zed_actions/src/lib.rs`, `crates/workspace/src/workspace.rs` |

`display_name()` is the highest-leverage of these: it feeds the About headline, the "Updated to X"
notification, and the install-CLI toast from one place.

Two behavioural changes ride along, both in the spirit of §3.2:

- `zed::OpenStatusPage`, `zed::GetMerch`, and `zed::OpenTelemetryLog` join the palette filter. They are
  Zed-branded destinations (status page, merch store) or dead under `telemetry: off`, so renaming them
  would have been a lie rather than a fix.
- The Extensions page no longer renders the inherited `Git` and `OpenIn` feature banners. They were the
  only place the word "Git" surfaced in Thock's own chrome, which `VISION.md` §4 forbids.

### Deliberately left alone

- **`ReleaseChannel::app_id` / `app_identifier`, the single-instance handshake, the HTTP user agent.**
  These are OS and protocol identifiers, not copy. `app_id` has to keep matching the macOS bundle
  identifier, and the handshake string is what two running builds use to find each other.
- **The `zed::` action namespace.** The palette still reads `zed: about`, `zed: quit`. Renaming it means
  touching `zed_actions`, every keymap entry, and the keymap-name search path in the command palette:
  a much larger rebase bill than the rest of this pass, and it wants its own decision.
- **Upstream comments in `assets/settings/default.json`.** Visible only via "Open Default Settings", and
  rewriting hundreds of comment lines in the largest fork-owned file would swamp every future merge.
- **AI, collab, debugger, onboarding, and Zeta copy.** Unreachable under `disable_ai` and the V12
  setting flips. Some of it (Zeta, Zed's native agent) names Zed products correctly and should stay.
- **Component-preview fixtures** in `crates/workspace/src/notifications.rs` and `crates/ui/**`, which
  are dev-only surfaces.
- **`crates/windows_resources`.** Thock ships macOS and Linux only.

---

## 7. Follow-up: no Node runtime in a vault (2026-09-09)

§3.1 kept the JSON, JSONC and YAML language servers so `settings.json` and `keymap.json` would still get
schema-aware editing. Measuring a running vault showed what that costs: opening a single `.json` file
starts `json-language-server` (a Node process, ~93 MB of `node_modules` on disk) *and*
`package-version-server`, then npm-installs prettier and initialises the Node runtime — none of which a
Markdown second brain has any use for. On the dogfood vault that was four resident language-server
processes for zero Markdown benefit.

The fix stays at the settings layer, as V12 requires:

| Language | Change |
|---|---|
| JSON, JSONC | `language_servers` → `["!json-language-server", "!package-version-server", "..."]`; `prettier.allowed` → `false` |
| YAML | `language_servers` → `["!yaml-language-server", "..."]`; `prettier.allowed` → `false` |

`enable_language_server` stays `true` for these languages deliberately. Denying the *named* servers rather
than the language's whole LSP capability keeps the machinery intact, so upstream tests that attach a fake
server to a JSON buffer still pass and the rebase stays cheap.

**Cost accepted:** no completions, schema validation or formatting in `settings.json` and `keymap.json`.
Both remain editable as plain text, and `routine.toml` is unaffected — TOML support comes from the `toml`
extension, which is native and pulls no Node.

**Verified** by A/B on the same build against a scratch vault holding one `.json` file: with stock
settings the log shows `starting language server json-language-server`, `package-version-server`,
`Installing default prettier` and a `node_runtime` line; with the settings above none of the four appear
and the process tree is the main process alone.
