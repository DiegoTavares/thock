# Testing Thock

One command picks the tests a change needs:

```sh
thock/script/test            # suites for what changed against origin/main, plus uncommitted work
thock/script/test --plan     # only print which suites that is
thock/script/test rust -- backlog::   # one suite, narrowed to one module's tests
thock/script/test --all      # everything (what CI runs on main)
```

CI calls the same script to decide which jobs to start, so a green local run and a green CI run mean
the same thing. The path → suite mapping lives in `suites_for` in that script and nowhere else.

## The suites

| Suite | Covers | Runs | Takes (warm) |
|---|---|---|---|
| `rust` | `crates/thock/` and anything upstream the fork touches | `cargo nextest run -p thock` (falls back to `cargo test`) | ~10 s rebuild + 1 s for ~470 tests |
| `sync-core` | `crates/thock-sync-core/`: the rules the desk and the phone must agree on | `cargo test -p thock_sync_core` | ~10 s |
| `go-plus` | `thock/services/plus/`: the Plus backend and vault sync API | `go vet` + `go test`, against an embedded Postgres it starts itself | ~30 s |
| `go-releases` | `thock/services/releases/`: the update index and manifest writer | `go vet` + `go test` + `write_manifest_test.py` | seconds |
| `go-site` | `thock/site/`: the site server and download gate | `go vet` + `go test` | seconds |
| `ios` | `thock/ios/`: ThockKit, the phone's whole data layer | `thock/ios/script/test` (`swift test`, on the Mac, no simulator) | ~5 s |
| `integration` | the sync contract end to end: the real desk and phone clients against a real Plus server | `thock/script/integration` (starts the server and a throwaway Postgres) | ~30 s |
| `workflows` | `.github/` and the Thock shell scripts | `thock/script/lint-workflows` (actionlint, zizmor, shellcheck) | seconds |

`integration` runs when either side of the sync contract changes: `thock/services/plus/`,
`crates/thock-sync-core/`, `crates/thock/src/vault_sync*`, or ThockKit's `Sync*` sources.
`thock/script/integration --serve` just starts the server and prints its URL, for poking at it by hand.

One heavier check is not part of the default loop: `thock/ios/script/smoke` builds the app, launches
it on a simulator and drives a scripted flow (~45 s). Run it after changing anything under
`thock/ios/Thock`, `ThockShare`, `ThockWidgets` or `Shared`. CI runs it on every iPhone change.

## What not to run

The slow loop on this repo has never been the Thock tests; it is building things nobody asked for.

- **Never `cargo test` or `cargo build` without `-p`.** The workspace is all of Zed: hundreds of
  crates, tens of minutes, tens of gigabytes in `target/`.
- **Never `./script/clippy` bare.** It means `--workspace --release` and builds a second artifact
  tree. Lint with `cargo clippy -p thock -p thock_sync_core --all-targets -- --deny warnings`; add
  `-p zed` only when `crates/zed` changed.
- **Don't build the app (`cargo run`, `cargo build -p zed`) to check logic.** Write a test. Build
  the app only to look at it, and say so when handing over.
- Go: each module pins a patched toolchain in `go.mod`; the script sets `GOTOOLCHAIN=auto` so `go` fetches
  it. Running `go test` by hand with an older local Go needs the same.

## Before pushing

```sh
cargo fmt --all -- --check
cargo clippy -p thock -p thock_sync_core --all-targets -- --deny warnings   # when Rust changed
thock/script/test
```

Formatting and clippy account for most red CI runs in this repo's history, and both are cheaper to
find locally.

## Writing tests that earn their keep

The bugs that reached `main` were rarely logic a unit test covered. They were disagreements between
two parts that each passed their own tests. Prefer the test that crosses the seam:

- **A panel change needs a keyboard test.** Drive the panel with `VisualTestContext` and
  `simulate_keystrokes` (see the tests in `sync_status.rs` and `markdown_conceal.rs`): arrows, `j`/`k`
  under vim mode, the primary action, `escape`. The keymap is part of the feature.
- **A skill that tells the agent to write a file needs a contract test**: parse the exact snippet
  the skill's Markdown prescribes with the Rust type that will read it.
- **A sync rule change needs a fixture** in `crates/thock-sync-core/fixtures/v1`. The Rust and
  Swift runners both read that directory, so one case tests both implementations.
- **A service route needs an auth case** in `thock/services/plus/security_test.go`, which walks the
  route table and expects every non-public route to refuse a missing or foreign credential.
- **Don't pin what a file already states.** A test that hard-codes a Routine's version or a count of
  shipped assets fails on every legitimate change; read the expected value from the manifest.
- In GPUI tests use the executor's timers, never `smol::Timer`.

## CI

`.github/workflows/ci.yml` — a `Plan` job maps the pull request's files to suites; each suite is a
job that is skipped when its paths didn't change. `CI` is the single required check: skipped suites
pass, failed or cancelled ones don't. Pushes to `main` run every suite.

`.github/workflows/security.yml` — dependency review on pull requests, gitleaks over the Thock tree
(`thock/gitleaks.toml` holds the allowlist), `govulncheck` and CodeQL for the Go services and the
workflows; also weekly, because advisories arrive without a push.

A failing check is information about the change; don't make it pass by skipping, ignoring or
loosening the test. If the test is wrong, fix the test and say why in the PR.
