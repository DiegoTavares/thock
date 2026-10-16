# Thock

Thock turns a folder of plain Markdown files into a guided, LLM-augmented second brain. Your notes
stay ordinary files in an ordinary folder; Routines scaffold how you use them, Skills are the
rituals your own LLM runs over them, and versioning happens invisibly underneath.

Thock is a hard fork of the [Zed](https://zed.dev) editor. The editor, vim mode, terminal and GPUI
come from Zed; the vault model, Routines, Skills and the panels that make it a note-taking app live
in `crates/thock`. Thock is not affiliated with Zed Industries.

- **Download and site:** https://thethock.com
- **What Thock is and what has shipped:** [thock/VISION.md](thock/VISION.md); features are specified
  in [thock/specs/](thock/specs/).
- **Building and testing:** [thock/TESTING.md](thock/TESTING.md). **Releasing:**
  [thock/RELEASING.md](thock/RELEASING.md).
- **Working in this repository, as a person or an agent:** [CLAUDE.md](CLAUDE.md).

## Licenses

Thock inherits Zed's licensing. The application crates, `crates/thock` included, are
GPL-3.0-or-later ([LICENSE-GPL](LICENSE-GPL)); GPUI and the other library crates are Apache-2.0
([LICENSE-APACHE](LICENSE-APACHE)). Each crate's `Cargo.toml` names its license, and the release
bundles carry the third-party notices that `script/generate-licenses` produces.
