# Thock on iPhone

The phone app of `thock/specs/v33-iphone-companion.md`, first release (J1): the Today canvas, capture
with destination chips, the journal, clips, plan nudges, receipts, and the entry points outside the
app. It syncs through the contract in `thock/specs/v34-vault-sync-api.md`.

It runs with no backend and no desk: the **practice notebook** on the first screen starts an
in-process implementation of the sync server and a simulated desk, and the app talks to them through
the same client code it uses against the real service.

## Layout

| Path | What |
| --- | --- |
| `ThockKit/` | Swift package with everything that is not a screen. `swift test` runs on the Mac. |
| `ThockKit/Sources/ThockKit/SyncCore/` | The shared rules of the API contract §7–§9, ported to Swift: `heading_key`, `line_hash`, `section_hash`, `apply`, `effect_present`, the envelope, path rules. |
| `ThockKit/Sources/ThockKit/Vault/` | Reading notes for display (cards, planner, journal, receipts) and building the writes the phone may make (V33 §14). |
| `ThockKit/Sources/ThockKit/Editor/` | The editor's block and inline model: the subset, opaque blocks, round-tripping. |
| `ThockKit/Sources/ThockKit/Store/` | The SQLite store in the app group: notes, the write queue, rebase, prune, captures. |
| `ThockKit/Sources/ThockKit/Sync/` | Wire models, the HTTP transport, pairing, the sync engine. |
| `ThockKit/Sources/ThockKit/Local/` | `LocalBackend` (the contract's routes, in memory), `SimulatedDesk`, the sample vault. |
| `Thock/` | The SwiftUI app. |
| `ThockShare/` | Share Sheet extension: the clip sheet. |
| `ThockWidgets/` | Today widget (Home and Lock Screen) and the Idea and Journal controls. |
| `Shared/` | Theme, fonts, and code compiled into all three targets. |
| `Config/` | Info.plists and the app group entitlement. |

`Thock.xcodeproj` uses synchronized folders, so adding a file to a folder adds it to the target.

## Running it

```sh
cd thock/ios/ThockKit && swift test          # the data layer, on the Mac
open thock/ios/Thock.xcodeproj               # then run the Thock scheme on a simulator
```

From a terminal:

```sh
cd thock/ios
xcodebuild -project Thock.xcodeproj -scheme Thock -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath build build
xcrun simctl install booted build/Build/Products/Debug-iphonesimulator/Thock.app
xcrun simctl launch booted app.thock.ios -thock-practice
```

`-thock-practice` opens the practice notebook on a fresh install. In the app, the `…` button at the
top right has the practice desk's controls: close the desk (writes wait on the phone), sort the inbox
at the desk (receipts turn to *filed*), add a line from the desk, end Thock Plus (read-only).

Debug builds take a few more launch arguments, for looking at a screen or running a flow without
tapping:

| Argument | Effect |
| --- | --- |
| `-thock-open <idea\|journal\|clip\|ask\|receipts\|you\|dock>` | open that sheet |
| `-thock-day <offset>` | show another day (`-1` is yesterday) |
| `-thock-looks <dark\|light\|system>` | set the appearance |
| `-thock-gate` | ask for Face ID in the practice notebook too |
| `-thock-type "<keys>"` | type into the first editor (`\n` return, `\b` backspace, `{B}` `{I}` `{List}` `{Task}` `{Pick}`), and write the Markdown it would save to the app's `tmp/editor-dump.md` |
| `-thock-script "<steps>"` | run steps separated by `;`: `capture:<inbox\|today\|backlog>:<text>`, `journal:<text>`, `tick:<label prefix>`, `soon:<label prefix>`, `asleep`, `awake`, `triage`, `plan`, `lapse`, `renew` |

## Against a real backend

The welcome screen scans the desk's QR, or takes the pairing link pasted (the simulator has no
camera). The link is the contract's `thock://pair?v=1&code=…&key=…&backend=…`, and it can be opened
from a terminal too:

```sh
xcrun simctl openurl booted 'thock://pair?v=1&code=K7MP-4QZX&key=<43 chars>&backend=http%3A%2F%2Flocalhost%3A8080'
```

Cleartext HTTP is allowed for local addresses only.

## Fixtures

`ThockKit/Tests/ThockKitTests/Fixtures/v1/` holds phone-authored cases in the contract's §9.2 format
(`make_fixtures.py` regenerates them; expected texts are written by hand, only hashes are computed).
`envelope.json` was produced by an independent implementation (Go, `x/crypto/chacha20poly1305`). When
`crates/thock-sync-core/fixtures/v1` exists in the checkout, the same test runner runs those too.

`CONTRACT-NOTES.md` lists where the frozen contract was ambiguous or contradicts itself, what this
port does in each case, and what should be settled before the desk implements the same rules.
