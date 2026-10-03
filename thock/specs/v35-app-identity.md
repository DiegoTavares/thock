# Thock V35 — An identity of its own: bundle id and URL scheme

**Status:** In progress (implemented on `thock-bundle-identifier`; ships as a deliberate release)
**Owner:** Diego · **Date:** 2026-10-03
**Companion docs:** `v12-de-zed-ification.md` §6 "Deliberately left alone" (which kept `app_id`),
`v20-auto-update.md` §8 and §12 (the stable bundle and its one-time identity migration)

---

## 1. Summary

Thock still tells the operating system it is Zed. The macOS bundle carries `dev.zed.Zed` (and
`-Dev`/`-Preview`/`-Nightly`), claims the `zed://` URL scheme, and on Linux uses the same id as its
Wayland/X11 application id and `.desktop` file name. On a machine with both apps installed, two
applications claim one identity: Launch Services, "Open With", notification and privacy (TCC)
settings, keychain access rules, and the dock's window-to-launcher pairing all key on it, and the
winner is whichever app the OS registered last.

V35 gives Thock its own identifier, `com.thethock.Thock`, and its own scheme, `thock://`, while
leaving every `zed://` string inside upstream code alone. Incoming `thock://` links are rewritten to
`zed://` at the two places a URL enters the app, so every existing handler works under both names.

V12 left `app_id` alone because it "has to keep matching the macOS bundle identifier". That
constraint stands; V35 moves both sides together and adds a test so they can't drift.

## 2. Decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Identifier | **`com.thethock.Thock`** for stable, **`com.thethock.Thock-Dev`**, **`-Preview`**, **`-Nightly`** for the other channels, mirroring upstream's shape. Reverse-DNS on the domain we own (`thethock.com`). Distinct from the iPhone app's `com.thethock.ios`, so the two never meet in App Store Connect or a keychain group. |
| 2 | `ReleaseChannel::app_id()` | Returns the same four strings. It is the Wayland application id, the X11 `WM_CLASS`, and (by the comment it carries) the macOS bundle id. A unit test in `release_channel` parses `crates/zed/Cargo.toml` and asserts each channel's `[package.metadata.bundle-<channel>] identifier` equals `app_id()`. |
| 3 | `app_identifier()` (Windows) | **`Thock-Stable`**, `Thock-Dev`, `Thock-Preview`, `Thock-Nightly`. Its only callers are the single-instance mutex and named pipe in `crates/zed/src/zed/windows_only_instance.rs` and `crates/cli/src/main.rs`, both Thock binaries. Changing it only affects Thock-to-Thock, and it stops Thock's CLI from handing paths to a running Zed. Thock does not ship Windows; this is for correctness of `cargo run` there. |
| 4 | URL scheme registered with the OS | **`thock`**, replacing `zed`: `osx_url_schemes = ["thock"]` in every bundle section, `x-scheme-handler/thock` in `zed.desktop.in`. Thock stops claiming `zed://` so a coexisting Zed keeps its links. |
| 5 | How `thock://` reaches handlers | **Rewrite, don't rename.** `OpenRequest::parse` (`open_listener.rs`) maps a leading `thock://` to `zed://` before matching, so `thock://agent`, `thock://settings/…`, `thock://skill?data=…`, `thock://file/…` behave exactly like their `zed://` twins. The three places that decide whether a CLI argument is a URL (`parse_url_arg` in `crates/zed/src/main.rs`, `URL_PREFIX` in `crates/cli/src/main.rs`, the Windows instance forwarder) accept `thock://` alongside `zed://`. `zed://` keeps working everywhere, so links already written into notes don't break. |
| 6 | Strings Thock produces | **Unchanged.** Upstream builds `zed://` links internally (settings deep links, skill share links, agent share links, JSON schema URIs, ACP mention URIs). Those are either never handed to the OS or round-trip back into the same app; renaming them is many upstream files for no user-visible gain. The only externally visible one is the settings "copy link" and skill share link, which now open Zed if Zed is installed; see §6. |
| 7 | Provisioning profile | `script/bundle-mac` **stops embedding** `crates/zed/contents/<channel>/embedded.provisionprofile` in unsigned builds. Those profiles are issued to Zed's team for `dev.zed.Zed*`; with a different bundle id they would be a mismatched profile inside our bundle. Thock's entitlements request no profile-backed capability (Developer ID builds already skip it). |

`thock/ios` already uses `thock://` (`thock://pair…` from the pairing QR code, `thock://today` from
the widget) under its own bundle id. The two apps live on different devices, so registering the same
scheme on the Mac is not a conflict. A desktop `thock://pair` link opened on a Mac reaches the open
listener as an unhandled URL and is logged, which is the same thing that happens today to an unknown
`zed://` path. `thock/site` generates no `zed://` or `thock://` links.

## 3. What changes for existing installs

This must ship as a deliberate release with a note to testers. Nothing is lost; some things are asked
again once.

- **Vault, settings, local state: unchanged.** `config_dir()` and `data_dir()` key off
  `paths::APP_NAME = "Thock"` (`crates/paths/src/paths.rs`), and the database dir off the release
  channel name, not the bundle id.
- **Auto-update: works across the change.** The macOS updater mounts the DMG under a volume named
  `Thock` and rsyncs the new `Thock.app` over the running bundle's path; the Linux updater rsyncs
  `thock.app` with `--delete`. Neither reads the bundle id or the scheme. The new `Info.plist` simply
  replaces the old one.
- **Keychain (macOS).** Google, Thock Plus, Readwise and vault-sync credentials are internet-password
  items keyed by server URL (`https://thock.local/google`, …) and account, not by bundle id, so they
  are still found. Their access list names the app that created them by its code-signing designated
  requirement, which for a Developer ID build includes the bundle identifier. After the update the
  first read may show a keychain prompt: **allow it (Always Allow), or reconnect Google / Plus once**.
  Ad-hoc signed builds already prompted on every update, so they see no difference. Linux's Secret
  Service keys on attributes only and is unaffected.
- **macOS per-app settings reset**: notification permission, privacy permissions (Calendars,
  Automation, Camera/Microphone if ever used), "Open With" defaults for `.md` files, Login Items, and
  saved window state (`~/Library/Saved Application State/dev.zed.Zed.savedState`). The old entries
  remain under `dev.zed.Zed` and do nothing; if Zed is installed they are Zed's anyway.
- **Linux launcher.** The bundled desktop entry becomes `share/applications/com.thethock.Thock.desktop`
  and the updater removes the old `dev.zed.Zed.desktop` from `thock.app`. A copy the user put in
  `~/.local/share/applications/` (or a pinned launcher made from it) keeps launching Thock, but the
  running window no longer matches it (its app id changed), so docks show a second icon. **Re-copy the
  new `.desktop` file and re-pin.**
- **`zed://` links** no longer open Thock from outside the app (a browser, another app) once macOS or
  the desktop picks up the new registration. `thock://` does. Inside Thock, `zed://` links still work.
- **Coexisting with Zed** now works: a stable Thock bundle, a `cargo run` dev build
  (`com.thethock.Thock-Dev` on Wayland/X11) and any Zed channel each have their own identity, launcher,
  and scheme.

## 4. Upstream touch-points

All mechanical; each is a string swap or a one-line addition.

| File | Change |
|---|---|
| `crates/zed/Cargo.toml` | `identifier` and `osx_url_schemes` in the four `bundle-*` sections |
| `crates/release_channel/src/lib.rs` | `app_id()`, `app_identifier()` strings; the drift test |
| `crates/release_channel/Cargo.toml` | `toml` as a dev-dependency, for the test |
| `crates/zed/resources/zed.desktop.in` | `x-scheme-handler/thock` |
| `script/bundle-linux` | `APP_ID` values |
| `script/bundle-mac` | stop embedding Zed's provisioning profiles |
| `crates/zed/src/zed/open_listener.rs` | `thock://` → `zed://` rewrite in `OpenRequest::parse`, and its test |
| `crates/zed/src/main.rs` | `parse_url_arg` accepts `thock://` |
| `crates/zed/src/zed/windows_only_instance.rs` | forwarder accepts `thock://` |
| `crates/cli/src/main.rs` | `URL_PREFIX` gains `thock://` |

## 5. Tests

- `release_channel`: `app_id_matches_bundle_identifier` reads `../zed/Cargo.toml` relative to
  `CARGO_MANIFEST_DIR`, parses it with `toml`, and for every `ReleaseChannel::ALL` asserts
  `package.metadata.bundle-<dev_name>.identifier == app_id()`, and that each section's
  `osx_url_schemes` is `["thock"]`.
- `zed` (`open_listener.rs`): `test_parse_thock_scheme_urls` checks that `thock://agent`,
  `thock://settings/…`, `thock://` and `thock://skill?data=…` parse to the same `OpenRequestKind`
  as their `zed://` twins.
- Not automatable here: Launch Services registration, the keychain prompt, and Linux dock pairing.
  Verify by hand on the release candidate: `defaults read /Applications/Thock.app/Contents/Info.plist
  CFBundleIdentifier`, `open thock://agent` with Zed also installed, and on Linux
  `xdg-mime query default x-scheme-handler/thock` after copying the `.desktop` file.

## 6. Non-goals and what stays

- **Renaming `zed://` inside upstream code** (`ZED_URL_SCHEME`, `zed_urls`, settings deep links, skill
  share links, schema and mention URIs). See decision 6. The visible consequence: a settings or skill
  share link copied out of Thock is still `zed://…`; pasting it into a browser opens Zed if installed,
  or nothing. Rewriting those producers is a later, separate change if testers share links.
- **The `cli: register zed scheme` action.** It registers this app as the handler for `zed`, by its own
  bundle id. With distinct ids it now does exactly what it says (and would take `zed://` from Zed), so
  it stays an explicit user choice rather than something Thock does on its own.
- **The CLI binary name, settings and data paths, the `zed::` action namespace.**
- **Files that describe Zed's own distribution**, left as `dev.zed.Zed`: `script/install.sh`,
  `script/uninstall.sh`, `script/flatpak/*`, `crates/zed/resources/snap/snapcraft.yaml.in`,
  `nix/build.nix`, `docs/src/*`, the flatpak check in `crates/cli/src/main.rs`, and the
  `/dev.zed.Zed*.json` line in `.gitignore`. Thock ships none of these channels; they are inert here
  and changing them is pure rebase cost.
- **Migrating old macOS per-app state** (saved state, defaults) from `dev.zed.Zed` to the new id. It is
  either Zed's or worthless.
