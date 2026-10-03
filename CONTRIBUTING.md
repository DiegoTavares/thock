# Contributing to Thock

Thock is one person's product, built in the open. Contributions are welcome as bug reports and
small, focused pull requests. There is no contributor licence agreement; by opening a pull request
you agree your change is offered under the licence of the files it touches (see the Licensing
section of the [README](README.md)).

For anything bigger than a fix, open an issue first so we can agree it fits the product before you
spend time on it. [`thock/VISION.md`](thock/VISION.md) is what Thock is and isn't, and its §12 is
the roadmap.

## Reporting a bug

Open a [new issue](https://github.com/DiegoTavares/thock/issues/new/choose) and pick the bug form.
It asks for what happened, what you expected, steps to reproduce, the Thock version and the
platform.

**Never paste note contents** or anything from your vault. Describe the shape of the problem
instead, for example "a note with a table under a heading".

Security problems don't go in issues: report them privately to the contact address on
[thethock.com/support](https://thethock.com/support). Questions and help with the app also go there.

## Sending a pull request

- **Title:** clear, correctly capitalized and imperative, prefixed with `thock:` when the Thock
  crates or docs are the scope, for example `thock: Add keyboard navigation to the Backlog panel`.
  No conventional-commit prefixes (`fix:`, `feat:`, `docs:`) and no trailing punctuation.
- **Body:** fill in the pull request template. Say what changed and why, link the spec in
  `thock/specs/` if there is one, and say how you tested it.
- **Files outside `crates/thock*` and `thock/`:** list every one in the body with the reason it
  couldn't be avoided. That section is the rebase risk.
- **Release notes:** end the body with a `Release Notes:` section holding one bullet,
  `- Added ...`, `- Fixed ...` or `- Improved ...` for user-facing changes, or `- N/A`:

  ```
  Release Notes:

  - N/A
  ```

- **Tests:** a change isn't done until it has a test at the seam it crosses. Every Thock panel must
  stay fully usable from the keyboard, so a panel change comes with a keystroke test.
- **One thing per pull request.** A bug fix doesn't carry a refactor or an unrelated feature.

`CI` is the single required check, and `main` only moves through pull requests that pass it.

## The test loop

[`thock/TESTING.md`](thock/TESTING.md) is the guide. In short:

```sh
cargo fmt --all -- --check
cargo clippy -p thock -p thock_sync_core --all-targets -- --deny warnings
thock/script/test
```

`thock/script/test` picks the suites that cover what you changed, the same way CI does. Never run
`cargo build`, `cargo test` or `cargo clippy` without `-p`: the workspace is all of Zed and an
unscoped build takes tens of minutes and tens of gigabytes.

## Fork discipline

Thock is a fork of Zed, and every line changed outside `crates/thock*` and `thock/` is a future
merge conflict, so keep upstream touch-points small and mechanical, and prefer adding Thock code
over editing Zed's. Nothing from this repository goes to upstream Zed: no branches, pull requests or
issues against `zed-industries/zed`.

## Rules for humans and agents

The repository's working rules live in [`CLAUDE.md`](CLAUDE.md); [`AGENTS.md`](AGENTS.md),
`GEMINI.md` and `.rules` are links to the same file so every coding agent reads them. They cover
the product invariants, fork discipline, keyboard navigation, Rust and GPUI conventions, testing and
pull request hygiene. Every contributor, person or agent, is expected to follow them. If you use an
agent, you are responsible for what it sends: read and understand the change before opening the
pull request.

## Conduct

Be kind and assume good faith. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
