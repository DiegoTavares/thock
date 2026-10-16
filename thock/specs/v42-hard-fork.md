# Thock V42 — Hard fork from Zed: prune what Thock never uses, own the app shell

**Status:** In progress (step 4 of §9)
**Owner:** Diego · **Date:** 2026-10-15
**Companion docs:** `v12-de-zed-ification.md` (the hide-don't-delete pass this replaces with deletion),
`v35-app-identity.md` (bundle id and `thock://`, whose upstream-preserving rewrite this simplifies),
`v20-auto-update.md` (the updater kept here), `../TESTING.md`, `../RELEASING.md`

---

## 1. Summary

Thock was built as a soft fork. Every change had to stay cheap to rebase onto upstream Zed, so V12
hid the code-editor surface with settings instead of removing it, and product code had to avoid
upstream files. The cost has been paid without the benefit. The last upstream merge was
`97d854b89b` on 2026-07-23, and nothing has been merged since.

Meanwhile the binary carries all of Zed. The workspace has 241 crates, the shipped app compiles
204 of them, and the `thock` crate needs 108 of those. The other 96 are AI providers, Copilot, edit
prediction, collaboration, the debugger, remote development, dev containers, the REPL and tasks.
None of it reaches a Thock user, and all of it costs build time, CI minutes and binary size.

V42 makes the fork a hard fork:

1. **Declare it.** Upstream becomes a source of fixes to pick, not a branch to follow. The fork rules
   in `CLAUDE.md` change to match, and a new agent skill, **zed-upstream-review**, finds upstream
   changes worth porting using nothing but git history.
2. **Prune.** Delete the crates Thock doesn't use, cut the dependency edges that drag AI, git UI and
   remote code into crates Thock does use, and remove the repository's Zed-only tooling.
3. **Own the shell.** `crates/zed` becomes `crates/thock_app`, the `zed::` action namespace becomes
   `thock::`, and the title bar, menus and open-URL handling are Thock code rather than patched Zed
   code.

Nothing here changes what a Thock user can do, apart from removing Open Recent, the extension store
page and the status-bar pickers (decisions 4, 5 and 13).

## 2. Decisions

| # | Decision | Why |
|---|---|---|
| 1 | **Hard fork. No more upstream merges.** Fixes come in as individual cherry-picks or hand ports. | Upstream hasn't been merged for 84 days, and Thock's direction (a note app) diverges further from Zed's (an AI code editor) every week. A merge would now be a project of its own. |
| 2 | **Upstream changes are tracked with git trailers, not a ledger file.** A review records `Upstream-Reviewed-Through: <sha>`, and each port carries `(cherry picked from commit <sha>)` or `Upstream-Commit: <sha>`. | Git history is enough to answer "what have we looked at, and what did we take", and it can't drift from the code. |
| 3 | **Reviews compare against Zed's latest stable tag** by default, not upstream `main`. | Stable fixes have been through a preview cycle, so there are fewer reverts to follow. The skill accepts any ref when a fix on `main` is urgent. |
| 4 | **Keep the extension host, drop the store.** `extension_host` stays, so the TOML extension and user extensions keep working. `extensions_ui` and the `zed: extensions` page go. TOML becomes a built-in language (decision 11). | `routine.toml` editing depends on TOML. A store that browses Zed's catalogue is Zed's product, not Thock's. |
| 5 | **Drop Open Recent** (`recent_projects`). Switching vaults goes through Open Folder. | `recent_projects` is the crate that pulls in `dev_container` and the SSH and remote-server pickers. A Thock user has one vault, rarely two. |
| 6 | **Delete code, don't hide it.** This replaces V12's "flip defaults and hide, never delete" rule for everything this spec touches. | Hiding only made sense for cheap rebases, which decision 1 retires. Dead code still costs compile time and attack surface. |
| 7 | **Product code still lives in `crates/thock`.** The hard fork permits editing upstream crates; it doesn't make that the default. | Keeps the domain separate from the inherited editor, and keeps cherry-picks applying cleanly to crates we haven't touched. |
| 8 | **Rename the shell crate, not the inherited library crates.** `crates/zed` becomes `crates/thock_app`. `zed_actions`, `editor`, `workspace` and the rest keep their names. | Renaming a library crate rewrites every `use` and breaks every future cherry-pick into it. The shell crate is the one place upstream fixes rarely matter. |
| 9 | **Licenses stay.** `LICENSE-GPL`, `LICENSE-APACHE`, `legal/` and `script/generate-licenses` are untouched. | The fork is still a derivative of GPL and Apache-licensed code, and the repository is public. |
| 10 | **macOS and Linux only.** Windows-only crates are deleted with the rest. | Thock ships on two platforms (V12 §6). Windows code is upstream surface with no user. |
| 11 | **TOML is a built-in language, not an extension.** Grammar and queries go into `crates/languages` like JSON and YAML; no language server. | The extension's language server (taplo) downloads a binary from GitHub the first time a `.toml` file opens, which the no-network check in §8 would catch. `routine.toml` needs highlighting and brackets, not generic TOML diagnostics, and a built-in language needs no seeding code and no archive in the repo. |
| 12 | **Repository tooling with no Thock caller goes too.** `script/` keeps only what CI, the release workflow and bundling run; `README.md` is rewritten for Thock; `CONTRIBUTING.md` and `CODE_OF_CONDUCT.md` are deleted. | Decision 6 applied to the repository rather than the binary. A one-person fork has no contribution process to document. |
| 13 | **The status-bar pickers leave with their crates:** language, encoding, toolchain, settings profile, and the language-server activity indicator. | A vault is Markdown and TOML; none of them has a job. Update progress is drawn by the title bar, not by the activity indicator. |
| 14 | **The first upstream review runs after the pruning PRs.** Step 1 only proves the helper reports the right range. | Triage is cheaper once the deleted crates make most of the range `gone`. Security advisories are the exception: one that touches a live crate is ported whenever it appears. |
| 15 | **macOS is validated on palloma, not in CI.** `cargo check -p zed` (later `thock_app`) and the darwin `cargo tree` run there before each pruning PR; CI compiles the app crate only on Linux. | macOS Actions minutes cost ten times Linux ones (`TESTING.md`), and the check takes minutes on a warm Mac. |
| 16 | **The app crate's tests run in CI on ubuntu only.** It joins the nextest job on Linux from step 3, when the keymap and action tests exist; the macOS job keeps `thock` and `thock_sync_core`. | The two new tests need every action registered, which only the app crate does. Linux is enough to catch a dangling binding. |

## 3. Where things stand

Measured on `main` at `46c43fa6da`. A "workspace crate" is a package that `cargo metadata --no-deps`
lists and that also appears in `cargo tree -p <package> -e normal`; every count below is that
intersection, so it can be reproduced on any checkout. (The first draft of this spec counted
`crates/` entries in `Cargo.toml` by hand and arrived at 241 / 204 / 108. Those figures are
superseded.)

| | Linux x86_64 | macOS arm64 |
|---|---|---|
| Workspace members | 249 | 249 |
| Compiled into the app (`-p zed`) | 206 | 206 |
| `thock` crate's own closure | 110 | 111 |

The projection behind §5.3: the closure of `thock` plus the shell crates Thock keeps (vim, search,
file finder, project panel, settings UI, title bar, terminal, theme selector, auto-update…) is about
175 crates today, and cutting the four edges in §5.3 should bring it to about 145. Most of the AI
stack comes in through `settings_ui`'s AI pages and `auto_update_ui`, not through the app crate. Git
UI comes in through `project_panel` and `title_bar`, and remote development through
`recent_projects` and `title_bar`.

The upstream delta today is 62 files and +1,448 / −355 lines outside `crates/thock*` and `thock/`
(`git diff 97d854b89b HEAD`). Most of it is in `crates/zed`, the keymaps, `default.json`,
`release_channel` and `git/src/repository.rs`.

### 3.1 Baselines

The figures every later PR is compared against, taken in step 1 of §9. No clean local build was
measured: on the Linux dev machine it would cost an hour for a number the release jobs already give,
and palloma had 22 GB free, less than a clean debug target needs. The v1.19.0 release jobs (clean
release builds, bundling and `remote_server` included) stand in for both platforms.

| Measure | Value |
|---|---|
| Release bundle, macOS (`Thock-aarch64.dmg`, v1.19.0) | 109 MB |
| Release bundle, Linux (`thock-linux-x86_64.tar.gz`, v1.19.0) | 124 MB |
| Release workflow build job, v1.19.0: macOS (aarch64) / Linux (x86_64) | 101 / 74 min |
| CI wall time on `main`: Clippy / Test (ubuntu) / Test (macOS) | 6 / 5 / 4 min |
| Incremental `cargo build -p zed -j 4` after touching `crates/thock`, Linux dev machine (i5-10500T, 15 GB) | 23 s |

## 4. Declaring the fork

### 4.1 The baseline

The first V42 commit is an empty commit on `main`:

```
thock: Declare the hard fork from Zed

Upstream is no longer merged. Fixes are ported one at a time; see
.agents/skills/zed-upstream-review/SKILL.md.

Upstream-Reviewed-Through: 97d854b89b5b38c189e9dee6351b861fbb202214
Upstream-Ref: main
```

`97d854b89b` is the upstream commit the 2026-07-23 merge brought in, so every upstream commit after
it is unreviewed.

### 4.2 Trailers

| Trailer | On | Meaning |
|---|---|---|
| `Upstream-Reviewed-Through: <sha>` | One commit per review, the last one on the review branch | Every upstream commit up to and including `<sha>` has been considered, and was either ported or deliberately left out. |
| `Upstream-Ref: <ref>` | Same commit | The ref that was reviewed (`v1.9.2`, or `main` for an urgent pick). |
| `(cherry picked from commit <sha>)` | A ported commit, via `git cherry-pick -x` | Cherry-picked from upstream with git's own record. |
| `Upstream-Commit: <sha>` | A hand-ported commit | Ported by hand because the cherry-pick no longer applied. Repeat the trailer if several upstream commits were folded together. |

PRs on this repository land as merge commits, which keeps every trailer on `main`. If a review PR
is ever squashed, the trailers have to go into the squash message.

### 4.3 The upstream remote

The remote is fetch-only and never brings tags into `refs/tags`. Zed's release tags are `v*`, which
is exactly what triggers `release.yml` on `origin`. One stray `git push --tags` would start a Thock
release.

```
git remote add upstream https://github.com/zed-industries/zed.git
git remote set-url --push upstream DISABLED
git config remote.upstream.tagOpt --no-tags
git config remote.upstream.fetch '+refs/heads/main:refs/remotes/upstream/main'
git config --add remote.upstream.fetch '+refs/tags/v*:refs/upstream-tags/v*'
```

Tags land in `refs/upstream-tags/`, which `git push --tags` never touches, and only `main` is
fetched, so Zed's release branches don't pile up under `refs/remotes/upstream/`. The helper checks
this configuration on every run and repairs it if needed: a missing push URL is set to `DISABLED`,
any other value is treated as deliberate and stops the run. It always fetches with a bare
`git fetch upstream`: naming a ref on the command line (`git fetch upstream main`) makes git skip the
configured refspecs, and the tags never arrive.

### 4.4 Rules

The "Repo layout and fork discipline" section of `CLAUDE.md` is rewritten. "Never push to Zed"
stays word for word. "Keep syncing with upstream cheap" is replaced by:

> ### Hard fork
>
> Thock is a hard fork of Zed (V42). Upstream is not merged; fixes are ported one at a time with the
> `zed-upstream-review` skill, and every port carries `(cherry picked from commit …)` or
> `Upstream-Commit:`.
>
> - Product code goes in `crates/thock`. Edit an inherited crate when the change belongs there (a bug
>   in `editor`, a dependency edge to cut), not to host Thock features.
> - Delete what Thock doesn't use instead of hiding it behind a setting.
> - Don't rename inherited library crates or reformat their code: both break every future cherry-pick
>   into them.
> - The `upstream` remote is fetch-only and fetches tags into `refs/upstream-tags/`. Never fetch
>   upstream tags into `refs/tags` — Zed's `v*` tags are Thock's release trigger.

The keymap rule ("add entries, don't restructure the file") is relaxed in the same edit. Removing
bindings is now expected.

### 4.5 The zed-upstream-review skill

**Location:** `.agents/skills/zed-upstream-review/SKILL.md`, next to the inherited skills, with
`.claude/skills` committed as a symlink to `../.agents/skills`. Every agent then reads the same
file, the way `AGENTS.md`, `CLAUDE.md` and `GEMINI.md` already share one. `.claude/worktrees/` is
added to `.gitignore` in the same change.

**Helper:** `thock/script/upstream-candidates` does the mechanical half, so the skill spends its
judgment on the commits that matter:

```
thock/script/upstream-candidates [--ref <upstream ref>] [--base <sha>]
```

1. Checks the remote configuration from §4.3, refusing to run while `upstream` has a push URL other
   than `DISABLED`, then runs a bare `git fetch upstream`.
2. Finds the baseline from the newest `Upstream-Reviewed-Through` trailer on `origin/main`, unless
   `--base` overrides it.
3. Resolves the target. The default is the newest stable tag: the highest `refs/upstream-tags/v*`
   without a pre-release suffix.
4. Lists `git log --no-merges --reverse <base>..<target>`. Zed squash-merges PRs, so each commit is
   one PR with a `(#NNNNN)` subject and a `Release Notes:` section.
5. Drops commits already ported, found by `-x` line, by `Upstream-Commit` trailer, or by an equal
   `git patch-id`.
6. Sorts each commit by the paths it touches:
   - **live**: it touches at least one path that still exists in Thock's tree.
   - **gone**: every path it touches is in a deleted crate or file. Only counted, never listed one
     by one.
7. Prints one tab-separated row per live commit: sha, PR number, subject, the live crates it
   touches, the release-note line, and a coarse signal (`security`, `crash`, `data`, `platform`,
   `perf` or blank). The signal comes from keywords in the subject and body (panic, crash, hang,
   deadlock, data loss, corrupt, CVE, security, Wayland, X11, macOS). With `--labels` it also reads
   each PR's labels through `gh`, one call per commit, which is why that is off by default.

The script is read-only toward the repository. It fetches but never checks out, commits or pushes.

**Skill procedure:**

1. **Run the helper.** Report the range (`<base>..<target>`, how many commits, live versus gone) and
   check Zed's published security advisories
   (`gh api repos/zed-industries/zed/security-advisories`). Any advisory that touches a live crate
   tops the list, whatever the range.
2. **Triage the live commits into three tiers.** Read the subject, the release note and, when they
   aren't enough, the diff or `gh pr view <n> --repo zed-industries/zed`.
   - **Port:** security fixes, crashes and panics, data loss or corruption in buffers, `fs` or
     saving, gpui platform fixes for macOS, Wayland or X11, and vim correctness fixes.
   - **Consider:** editor, vim, markdown, terminal and gpui behaviour or performance improvements a
     note-taker would notice, plus `ui` component fixes.
   - **Skip:** anything whose only live paths serve features Thock removed or never uses (LSP
     features, language-specific behaviour, code navigation, git UI), new upstream features, CI and
     tooling, and Windows.
3. **Check prerequisites.** For each Port or Consider commit, note whether it depends on earlier
   upstream commits Thock doesn't have: a new API or a moved file. Add the prerequisite to the
   candidate list, or downgrade the commit to Skip with the reason.
4. **Trial-apply.** In a scratch worktree off `origin/main`, cherry-pick the Port and Consider
   commits in upstream order with `git cherry-pick -x`, then run `cargo check -p <crate>` for each
   crate touched. Record whether each one applied cleanly, conflicted, or applied but failed to
   build. Don't resolve conflicts at this stage.
5. **Report.** Write the report to the session scratchpad: Port, Consider and notable Skips, each
   with its PR link, live crates, trial result and a one-line reason. Ask Diego which commits to
   port with a multi-select question, Port tier pre-selected and Consider tier listed. Don't ask
   about Skips. They are in the report if he wants to argue.
6. **Port.** On a branch named `upstream-<target>` (for example `upstream-v1.9.2`), cherry-pick the
   chosen commits with `-x`. When a pick conflicts, read the upstream diff for intent and resolve to
   the same end state with the smallest change, then finish with `GIT_EDITOR=true git cherry-pick
   --continue`. When it can't be applied, hand-port it as its own commit with the upstream subject
   and an `Upstream-Commit:` trailer. Validate with `cargo check` and `cargo test -p` for each
   touched crate, plus `thock/script/test`.
7. **Close the review.** The branch's last commit is the empty review commit from §4.1, with the new
   `Upstream-Reviewed-Through` and `Upstream-Ref`. It is added even when nothing was ported, so the
   next run starts after this range.
8. **Open the PR on `DiegoTavares/thock`.** Title it `thock: Port upstream Zed fixes from <target>`.
   The body has three sections: Ported, with PR links; Considered and left out, with reasons; and
   the standard `Release Notes:` section.

**Guardrails written into the skill:**

- Never push to `upstream`, and never open an issue, PR or comment on `zed-industries/zed`. `gh`
  calls against upstream are read-only (`pr view`, `api` GETs).
- Never fetch upstream tags into `refs/tags`.
- Never let a port touch `crates/thock` or `thock/`. A port that needs Thock-side changes is
  reported, and Diego decides.
- Run reviews when Zed publishes a stable release, and right away when a security advisory touches
  a live crate.

**Tests:** `thock/script/test-upstream-candidates`, a bash test next to the helper, is the new
`upstream` suite in `thock/script/test`, and `ci.yml` runs it in an `Upstream` job on ubuntu whenever
either script changes. The suite builds a throwaway repository with a fake upstream history (commits
touching live and deleted paths, one already cherry-picked with `-x`, one hand-ported with a trailer,
one patch-id duplicate, stable and `-pre` tags) and checks the helper's output row by row. It also
checks that the helper refuses to run when `upstream` has a push URL.

## 5. Pruning

### 5.1 Crates leaving the app

These are removed from `crates/zed` (`Cargo.toml` dependencies, `init` calls in `main.rs` and
`zed.rs`, and the `collab`, `calls`, `onboarding` and `extensions-ui` features). Then their crates
are deleted from the workspace once nothing else depends on them:

- **AI and prediction:** `agent`, `agent_ui`, `agent_skills`, `acp_tools`, `ai_onboarding`,
  `anthropic`, `open_ai`, `ollama`, `lmstudio`, `llama_cpp`, `mistral`, `deepseek`, `x_ai`,
  `open_router`, `opencode`, `codestral`, `copilot`, `copilot_chat`, `copilot_ui`, `edit_prediction`,
  `edit_prediction_ui`, `edit_prediction_context`, `edit_prediction_metrics`, `language_models`,
  `language_models_cloud`, `prompt_store`, `web_search`, `web_search_providers`, `streaming_diff`,
  `audio`
- **Collaboration:** `call`, `collab_ui`, `livekit_client`, `livekit_api`
- **Debugger, REPL, tasks:** `debugger_ui`, `debugger_tools`, `dap_adapters`,
  `debug_adapter_extension`, `repl`, `tasks_ui`
- **Remote and dev containers:** `recent_projects`, `dev_container`, `remote_connection`
- **Code-editor tools:** `diagnostics`, `language_tools`, `lsp_locations`, `project_symbols`,
  `toolchain_selector`, `language_selector`, `language_onboarding`, `snippets_ui`, `journal`,
  `csv_preview`, `svg_preview`, `encoding_selector`, `settings_profile_selector`, `sidebar`,
  `activity_indicator`
- **Zed's own surfaces:** `onboarding`, `feedback`, `extensions_ui`, `component_preview`,
  `inspector_ui`, `miniprofiler_ui`, `input_latency_ui`, `git_ui`

`acp_thread`, `agent_servers` and `agent-client-protocol` stay, because the Thock Agent panel is
built on them. `git` stays for invisible history. `lsp`, `dap`, `rpc`, `client`, `remote` and
`language_model` stay because `project` and `editor` depend on them. Removing those means a
vault-only `project`, which is out of scope (§10).

The list is a starting point, not a contract. A crate stays if removing it turns out to need a
change in `crates/thock`. The PR body says which crates stayed and why.

Step 3 took every crate above out of the app and deleted the thirty that nothing else used. Step 4
cut the §5.3 edges and deleted the thirty-two that this orphaned: the whole AI and prediction stack
(`agent`, `ai_onboarding`, `codestral`, `copilot`, `copilot_chat`, `copilot_ui`, `edit_prediction`,
`edit_prediction_context`, `edit_prediction_metrics`, `edit_prediction_ui`, `language_models`,
`language_models_cloud` and the provider crates `anthropic`, `bedrock`, `deepseek`, `llama_cpp`,
`lmstudio`, `mistral`, `ollama`, `open_ai`, `open_router`, `opencode`, `x_ai`, plus `web_search`,
`streaming_diff`, `eval_utils`, `aws_http_client`, `html_to_markdown` and `shell_command_parser`),
and `remote_server` with `dap_adapters` and `debug_adapter_extension`, which only
`recent_projects`'s tests still reached. What remains of the list waits for `title_bar` (step 5):
`git_ui` (and through it `prompt_store`, `agent_skills` and `panel`), `recent_projects`,
`dev_container`, `remote_connection`, `call`, `livekit_client`, `livekit_api` and `audio`. `vim`'s
tests also link `git_ui`; that edge goes with the crate. `agent_settings` stays registered for good:
`workspace` reads it to decide whether the multi-workspace sidebar exists. `audio` is no longer
initialized, and its `default.json` section is gone with the settings page that read it.

### 5.2 Everything that referenced them

- **Keymaps.** About 250 bindings in each default keymap name actions from removed crates (`agent::`
  102, `git::` 47, `notebook::` 19, `debugger::` 17, `git_panel::` 16 and others on macOS). Each one
  is removed from `default-macos.json`, `default-linux.json` and `vim.json`. A test loads every
  bundled keymap and fails on an action name that doesn't resolve, so a binding to a removed action
  can't come back (§8).
- **Settings.** `default.json` entries whose only reader was a removed crate are deleted, along with
  their `// Thock:` comments from V12. The `settings_content` structs stay. They're inert, and
  removing them is a large edit for no user benefit. A user's `settings.json` that still sets a
  removed key keeps parsing. Step 3 deleted `repl`, `journal` and `collaboration_panel`; `jupyter`
  stays because the editor's settings still unwrap it, and the AI, git and call sections wait for the
  crates that still register them (steps 4 and 5).
- **Command palette filter.** `thock::init` stops hiding namespaces that no longer exist.
- **Menus.** `app_menus.rs` loses Open Recent, Extensions and anything else that pointed into a
  removed crate.
- **Bundling.** `script/bundle-mac` and `script/bundle-linux` stop building `remote_server`, a second
  release build of most of the workspace. This lands in step 2; the crate itself goes in step 3, once
  `recent_projects` no longer depends on it.

### 5.3 Edges to cut inside kept crates

| Kept crate | Drags in | Cut |
|---|---|---|
| `settings_ui` | `agent`, `language_models`, `copilot`, `edit_prediction` | Delete the AI, edit-prediction and Copilot pages and their setting items. |
| `project_panel` | `git_ui` | Remove the git status decorations and git actions. Invisible history never shows in the tree anyway. |
| `auto_update_ui` | `prompt_store`, `agent_skills` | Remove the post-update notice about Zed's agent rules moving to skills. Update notifications stay. |
| `title_bar` | `git_ui`, `recent_projects`, `call` | Rewritten as Thock code in §6.3. |
| `workspace` (welcome page) | Recent project actions | The empty-pane welcome drops its recent-projects list. |

Each cut is the smallest change that removes the dependency edge. These crates are not refactored
beyond that.

### 5.4 TOML without the extension

`extension_host` stays. `extensions_ui` goes, and with it the `zed: extensions` action and menu
entry.

TOML support today is the `toml` extension, auto-installed on first launch: one download from
`api.zed.dev`, then a second download of the taplo language server from GitHub the first time a
`.toml` file opens. V42 makes TOML a built-in language instead (decision 11). `crates/languages`
gains a `toml` directory (`config.toml`, `highlights.scm`, `brackets.scm`, `indents.scm` and
`outline.scm`, taken from the extension's own queries) and links the `tree-sitter-toml` grammar the
way the other built-in grammars are linked. No language server is registered. The `toml` entry in
`auto_install_extensions` and the `"TOML": { "enable_language_server": true }` block leave
`default.json`. Opening `routine.toml` then needs no network and no `extensions/installed/toml`.

What a user loses is taplo's generic TOML diagnostics. `routine.toml` has no schema today, so
nothing Thock-specific was being validated; if the vault ever wants real validation it belongs in
`crates/thock`, which already parses the file. A user who installed the TOML extension by hand keeps
it, and the two `TOML` languages coexist: `LanguageRegistry` resolves a path by walking the
available languages from the most recently registered and only replaces a candidate on a strictly
longer suffix match, so the extension's `TOML` (registered after the built-ins) wins `.toml` files,
while `language_for_name("TOML")` walks forward and returns the built-in. The extension's `toml`
grammar also replaces the native one in the grammar table once it loads. Both are tree-sitter-toml
(the extension pins `tree-sitter/tree-sitter-toml`, the app links the `tree-sitter-grammars` fork
published as `tree-sitter-toml-ng`), so the same queries run either way. taplo still never starts,
because `enable_language_server` is `false` for every language Thock doesn't list in `default.json`.

Themes are unaffected. Bundled themes stay, user themes in `~/.config/thock/themes/` keep loading,
and the Customize App ritual's theme section already names both. Its mention of themes "from
extensions" is dropped.

### 5.5 Repository cleanup

Upstream-only tooling with no Thock user is deleted:

- `.agents/skills/zed-cherry-pick` (Zed's release-branch process) and `.agents/skills/lint-creator`
  (Zed's dylint setup). `.agents/skills/gpui-test` stays, because Thock writes gpui tests.
- `.factory/` (Zed's brand-writing skills), `.cloudflare/` (Zed's docs proxy), `docs/` (Zed's
  manual), `ci/`, `Dockerfile-collab*`, `Dockerfile-cross.dockerignore`, `Dockerfile-distros*`,
  `Procfile*`, `compose.yml`, `REVIEWERS.conl`, `default.nix` and the rest of the Nix files.
- `extensions/` except `extensions/test-extension`, which `extension_host`'s tests load.
- Workspace members that are never built for the app: `collab`, `remote_server`, `eval_cli`,
  `eval_utils`, `edit_prediction_cli`, `extension_cli`, `docs_preprocessor`, `theme_importer`,
  `schema_generator`, the `*_benchmarks` crates, `gpui_web`, `gpui_windows`, `windows_resources`,
  `explorer_command_injector`, `auto_update_helper`, `etw_tracing` and `aws_http_client`.
  `gpui_platform` loses its Windows and web arms. Zed's `cargo xtask` tooling (`xtask`,
  `compliance`) goes with them. Four of the list had to wait for the crates that depended on them:
  `eval_utils` (`agent`), `aws_http_client` and `bedrock` (`language_models`) went in step 4, as did
  `remote_server`, once `recent_projects`'s tests stopped linking it.
- `script/`, down to what `ci.yml`, `release.yml`, `promote-release.yml`, the deploy workflows and
  the bundle scripts call (`bundle-mac`, `bundle-linux`, `linux`, `check-keymaps`,
  `generate-licenses`, `clippy` and their helpers under `script/lib`). The other hundred scripts
  (collab deploys, nightly bumps, Zed's issue bots, Windows PowerShell) go. The keep-list is computed
  in the PR by grepping those callers, not written by hand.
- `.github/` loses Zed's issue and pull-request templates and any composite action no Thock workflow
  references.
- `README.md` becomes a short Thock README: what the app is, where `thock/VISION.md`, the site and
  the licenses are. `CONTRIBUTING.md` and `CODE_OF_CONDUCT.md` are deleted (decision 12).

The list was computed on Linux, so `gpui_macos` shows up as unbuilt there. Every deletion is checked
against `cargo tree --target aarch64-apple-darwin` as well before it is made.

## 6. Owning the shell

### 6.1 `crates/zed` becomes `crates/thock_app`

- The package is renamed `thock_app`. The binary stays `thock`, so bundles, the CLI symlink and the
  updater don't change.
- Every `-p zed` / `--package zed` changes: `ci.yml`, `script/bundle-*`, `thock/script/test`,
  `TESTING.md`, `RELEASING.md`, `CLAUDE.md`.
- Zed-only modules are deleted: `edit_prediction_registry.rs`, `quick_action_bar/repl_menu.rs`,
  `telemetry_log.rs`, the hang-detection telemetry under `reliability/`, `visual_test_runner.rs`
  with its `visual-tests` feature, and the Zed Nightly bundle metadata. `migrate.rs` stays because
  it migrates settings. The local crash handler (`crashes`) stays, and minidump upload remains off
  (V12 §2).

### 6.2 `zed::` actions become `thock::`

The `zed` namespace in `zed_actions` is renamed `thock`, so the palette reads `thock: about` and
`thock: open settings`. Each renamed action keeps its old name in `deprecated_aliases`, which gpui
already supports, so a user's `keymap.json` that says `zed::OpenSettings` keeps working. The
inherited `zed_actions::` aliases are preserved too.

Thock's own actions already live in `thock::`. A test asserts that no two actions register the same
name, so a collision fails in CI rather than at startup.

The `zed://` and `thock://` handling from V35 becomes direct. `open_listener.rs` parses `thock://`
natively and the rewrite to `zed://` is removed. `zed://` stops being accepted. After V35, Thock no
longer registers that scheme with the OS, so those links never reach it anyway. V35 shipped in
v1.17.0, so nothing blocks this step.

### 6.3 Title bar

`title_bar` is rewritten in place as Thock's title bar, without the collaborator avatars, call
controls, branch picker, project picker, sign-in or user menu. It keeps the window controls, the
vault name, the update button and the platform-specific chrome from `platform_title_bar`.

This is a removal, not a redesign. A title bar with Thock-specific content belongs in a later spec.

## 7. What a user notices

- **Open Recent is gone** (ctrl-r, alt-cmd-o, File → Open Recent). Open Folder switches vaults.
- **The extension store is gone.** Installed extensions keep working. TOML highlighting is built in,
  so nothing downloads on first launch or when `routine.toml` opens; taplo's generic TOML
  diagnostics go with it.
- **The status bar loses its code-editor pickers:** language, encoding, toolchain, settings profile,
  and the language-server activity indicator (decision 13).
- **Palette names change** from `zed:` to `thock:`. Old names in a personal keymap keep working.
- **The settings window loses its AI pages.** They configured features that were already off.
- Nothing else changes. Every panel, binding and ritual behaves the same, and the vault on disk is
  untouched.

## 8. Testing

- **Bundled keymaps resolve.** A `thock_app` test loads `default-macos.json`, `default-linux.json`
  and `vim.json` against the app's registered actions and fails on any unknown action name. This is
  the guard for §5.2.
- **No duplicate action names** after the namespace rename (§6.2).
- **Deprecated aliases.** A keymap binding `zed::OpenSettings` dispatches `thock::OpenSettings`.
- **TOML is built in.** The app crate's bundled-languages test finds `TOML` with no extension
  installed, and `routine.toml` resolves to it.
- **`thock://` URLs** open the same handlers V35's tests cover, without the rewrite.
- **The upstream suite** from §4.5.
- **Existing suites** (`thock/script/test`, `cargo clippy -p thock -p thock_sync_core -p
  thock_app`) stay green throughout. From step 3 the ubuntu half of CI's nextest job also runs the
  app crate (`-p zed`, later `-p thock_app`), which is where the keymap and action tests live; the
  macOS half keeps `thock` and `thock_sync_core` (decision 16). A failing upstream test in a kept
  crate is fixed, never skipped. A test of removed behaviour is deleted along with the behaviour.

**Manual check on both platforms, by Diego, before the last PR merges:**

- Fresh config directory, no network. Launch, open a vault, edit a note in vim mode, run a ritual
  through the Agent panel, open the terminal, settings, keymap editor and theme selector, and open
  `routine.toml` with highlighting.
- `lsof` (macOS) or `ss` (Linux) during startup shows no connection except the update check to
  `updates.thethock.com`.
- Install the previous release, then update to a build from this branch through the updater.

## 9. Delivery

Each step is its own PR, and `main` stays shippable between them.

1. **Declare.** Commit the baseline (§4.1), rewrite the rules (§4.4), add the skill, helper and test
   suite (§4.5), and fix `.gitignore`. Record the baselines in §3.1: crate counts for both targets
   (the darwin ones from palloma), release bundle sizes from the v1.19.0 assets, the CI wall times of
   the Rust jobs, the incremental rebuild time after touching `crates/thock`, and one clean build
   with peak RSS on palloma. The Linux clean build is skipped on purpose.
2. **Clean the repository** (§5.5), including the `remote_server` bundling change from §5.2 and the
   README rewrite. Low risk, because nothing deleted here is built for the app.
3. **Cut the app's dependencies** (§5.1, §5.2) and the keymap test. The app crate joins the ubuntu
   nextest job (§8).
4. **Cut the edges** (§5.3), make TOML built in (§5.4), and delete the crates that become orphaned.
5. **Own the shell** (§6).
6. **Review upstream for the first time** (§4.5), from `97d854b89b` to the newest stable tag, once
   the pruning has decided which paths are gone (decision 14). A security advisory that touches a
   live crate doesn't wait for this step.

Each PR's body records crate counts before and after, and each pruning PR is preceded by
`cargo check -p zed` and the darwin `cargo tree` on palloma (decision 15). PR 5 records the full set
of measurements against §3.1.

## 10. Not in this spec

- **A vault-only `project`.** `project` is what pulls `lsp`, `dap`, `rpc`, `client`, `remote` and
  `prettier` into Thock's own closure. Cutting it is the change that would shrink the 108, and it's
  the deepest change on the list. It gets its own spec when there is a concrete reason.
- **Trimming grammars and built-in languages.** Code fences in notes still highlight. Measure the
  binary after step 5 before deciding.
- **Deleting `settings_content` fields** for removed features (§5.2).
- **A Thock-designed title bar** (§6.3).

## 11. Acceptance

- The app's workspace-crate count on Linux x86_64 is at most 150, down from 206 counted as in §3,
  and the macOS count falls by a similar margin. Both numbers are recorded in the PR.
- Build time and release bundle size are lower than the §3.1 baselines. The PR reports the numbers
  whatever they turn out to be.
- No `zed:` entry in the command palette and no `zed::` action in the bundled keymaps.
- `routine.toml` opens with TOML highlighting on a fresh install with no network.
- `cargo tree -p thock_app` contains none of the crates in §5.1.
- `thock/script/upstream-candidates` run on `main` reports the range `97d854b89b..<latest stable
  tag>`, and an end-to-end skill run produces a review PR with an `Upstream-Reviewed-Through`
  trailer (even if nothing is ported).
- The §8 manual check passes on macOS and Linux.
