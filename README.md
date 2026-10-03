# Thock

[![CI](https://github.com/DiegoTavares/thock/actions/workflows/ci.yml/badge.svg)](https://github.com/DiegoTavares/thock/actions/workflows/ci.yml)

Thock is a desktop app that turns a folder of plain Markdown files into a guided, LLM-augmented
second brain. Your vault stays a normal folder on disk. **Routines** (daily and weekly notes,
finance, journaling, team) are opt-in bundles that scaffold folders, templates and quick links, and
ship **Skills**: inspectable Markdown rituals that the LLM you already use runs over your notes.
Custom panels (the Routines rail, Day Planner, Backlog, Agent) sit around a fast editor.

Thock is a fork of the [Zed](https://github.com/zed-industries/zed) editor. It is not Zed, and it is
not affiliated with or endorsed by Zed Industries. Please don't report Thock issues to the Zed
project.

## Getting it

Thock is in early access, a private beta. Builds for macOS and Linux are on
[thethock.com/download](https://thethock.com/download), behind an invite code: join the waitlist on
[thethock.com](https://thethock.com) and an invite arrives when a slot opens. Installed builds
update themselves.

There is also an iPhone companion, Thock Vault, for capturing and journaling away from the desk. It
is distributed through TestFlight during the beta.

## The vault promise

- **Your files, forever, in the open.** Plain Markdown in a normal folder, no proprietary store. If
  Thock disappeared, the vault still opens in any editor.
- **Augmentation, not replacement.** The AI appends its synthesis under its own heading; it never
  silently rewrites what you wrote.
- **Invisible versioning.** History runs underneath so any change is one restore away, without you
  ever touching source control.
- **Everything is editable.** Skills, layouts, prompts and templates are files you (or your agent)
  can open and change.

The full product picture and roadmap are in [`thock/VISION.md`](thock/VISION.md).

## Building from source

Toolchain setup is the same as upstream Zed. Follow the guide for your platform:

- [macOS](docs/src/development/macos.md)
- [Linux](docs/src/development/linux.md)

Thock ships on macOS and Linux only. The pinned Rust version is in `rust-toolchain.toml`.

Then, from the repository root:

```sh
cargo run -p zed      # builds and runs the app; the binary is named `thock`
thock/script/test     # runs the test suites that cover what you changed
cargo clippy -p thock -p thock_sync_core --all-targets -- --deny warnings
```

The `zed` package keeps its upstream name; its default binary is `thock`.

**Never build, test or lint the whole workspace** (`cargo build`, `cargo test` or `cargo clippy`
without `-p`, or a bare `./script/clippy`). The workspace is all of Zed: hundreds of crates, tens of
minutes and tens of gigabytes. Scope commands to `-p thock` and `-p thock_sync_core`, and add
`-p zed` only when `crates/zed` changed. [`thock/TESTING.md`](thock/TESTING.md) has the full test
loop, and [`thock/RELEASING.md`](thock/RELEASING.md) covers how builds ship.

## Repository layout

- `thock/`: the product vision, feature specs (`specs/`), the testing and release runbooks, the Go
  services (`services/`), the website (`site/`) and the iPhone app (`ios/`).
- `crates/thock/`: all of Thock's Rust: panels, the vault model, Routines, Skills, the Backlog and
  history. The shipped Routine catalog and core Skills are in `crates/thock/assets/`.
- `crates/thock-sync-core/`: the sync rules the desktop and the phone must agree on, with shared
  fixtures.
- Everything else is upstream Zed, kept as close to upstream as possible so rebases stay cheap.

## Support and privacy

- Help: [thethock.com/support](https://thethock.com/support)
- Privacy policy: [thethock.com/privacy](https://thethock.com/privacy)
- Bugs: open a [new issue](https://github.com/DiegoTavares/thock/issues/new/choose) on this
  repository and use the bug form. Never paste note contents into an issue.
- Security problems: report them privately to the contact address on the support page, not in a
  public issue.

Contributing: see [CONTRIBUTING.md](CONTRIBUTING.md).

## Licensing

Thock inherits Zed's licensing. The source is licensed primarily under GPL-3.0-or-later
([`LICENSE-GPL`](LICENSE-GPL)), with Apache-2.0 components where marked
([`LICENSE-APACHE`](LICENSE-APACHE)). Each crate states its licence in the `license` field of its
`Cargo.toml`; the Apache-2.0 crates are mostly the GPUI framework and its supporting libraries
(`gpui`, `gpui_*`, `util`, `collections`, `sum_tree` and others).

The Thock crates, `crates/thock` and `crates/thock-sync-core`, are GPL-3.0-or-later.
