# Thock V34 — Vault sync: the same vault on the desk and the phone

**Status:** Design accepted (2026-10-02) — requirements and architecture decided; stack choices
that do not change the architecture are logged in §16 for the first implementation iteration
**Owner:** Diego · **Date:** 2026-10-02
**Design reference:** the interactive page with the decision cards, the architecture, three sync
scenarios stepped through line by line, and the option cards §16 resolves:
<https://claude.ai/artifact/VhXHNDp4bdqChWoxeFiwDU>
**Companion docs:** `v33-iphone-companion.md` (the phone app and its write contract, §14 there, which
this spec turns into a protocol), `v25-thock-plus-hosted-agent.md` (the Plus backend this extends,
the trust model this amends in one place), `v2-invisible-git.md` (the checkpoint service the desk
runs before applying anything from the phone), `v13-inbox-routine.md` (the `<!--inbox:…-->` marker
style the conflict marker follows), `v15-unified-gmail-sync.md` (the "state is a cache, the vault is
the record" posture)

---

## 1. Summary

V33 assumed the whole vault is on the phone and did not say how it gets there. V34 is the how.

A **Thock Plus** user pairs the phone with the desk by scanning one code. From then on the desk
uploads every text file of the vault to the Thock backend as an encrypted blob the backend cannot
open, and the phone downloads them. The phone's writes, which V33 limits to appends, single-line
edits and one gated section rewrite, travel the other way as small encrypted **writes** that sit in
a queue until the desk applies them. The server therefore holds two things per vault: the latest
encrypted snapshot of each file, and the writes the desk has not yet seen. That is enough for the
phone to show the plan while the desk sleeps, for the desk to catch up when it opens, and for
nothing to be lost when both ends touch the same line.

The one key that opens the vault exists on the two devices and nowhere else. Thock the company
stores the vault and cannot read it. File paths are the one thing the server sees in the clear, by
decision, so files sync one at a time.

What this spec does **not** build: multi-device beyond one desk and one phone, history on the
server, sync for anyone without Plus, sync of anything that is not text.

## 2. Goals & success criteria

- Pair in one scan at the desk. No account screen, no key to type, no passphrase to keep.
- The phone shows the latest vault the desk uploaded, whether or not the desk is open now, and
  works fully offline on that copy.
- A tick on the phone shows at the desk within seconds while both apps are in the foreground; a
  change at the desk reaches a phone in a pocket within the time iOS allows (minutes to hours), and
  on the next open at the latest.
- **No write is ever lost.** Two appends to one section both land. Two edits of one line both stay
  in the note, one under the other, marked for the desk to tidy. Nobody is asked to resolve
  anything on the phone.
- A file the phone has displayed but not edited is byte-identical after any number of syncs.
- The server stores nothing it can read except paths, sizes, versions and timestamps. Losing both
  devices loses the server copy, not the vault on disk.
- Only allow-listed text files leave the desk. Images, PDFs and anything binary never do.
- The words *sync*, *server*, *encrypt* and *git* do not appear on the phone (V33 §2). The desk's
  sync popover may say *phone*: *Phone connected · up to date*.

## 3. Non-goals

- **More than one desk or one phone per vault.** The protocol is written for two parties. §16
  notes what a third would need.
- **History on the server.** The server holds the latest version of each file. Time-travel stays
  the desk's local history (V2). Phone-side history is a later V33 tier and will need its own design.
- **Sync without Plus.** No local-network or peer-to-peer path. Plus gates the storage because
  the storage costs money to serve (VISION §4.3).
- **Syncing non-text.** Not even small images. The allow-list is the boundary.
- **A background helper on the desk.** The desk syncs while Thock is open. A daemon is a later
  decision (§16 #6).
- **Running the hosted agent on the server for the desk.** V25's rule stands at the desk. The
  phone's bounded exception is §13.
- **iCloud, in any role.** Not for data, not for keys.

## 4. Core concepts

### 4.1 One vault, one key, two devices

The desk generates a random 256-bit **vault key** when sync is turned on. The pairing code hands
it to the phone. The backend never receives it. Every file version and every write is encrypted
with this key before it leaves a device, and decrypted only after it arrives on the other one.

Losing both devices loses the ability to read the server copy. That is accepted (decision §15 #7):
the vault on the desk's disk is unaffected, and re-enabling sync uploads it again under a new key.
There is no recovery phrase and no key escrow.

### 4.2 Snapshots one way, writes the other

| Direction | Unit | Why |
| --- | --- | --- |
| Desk → server → phone | **Snapshot**: the full content of one file at one version | The desk is the vault's owner and may change anything, including through other editors and rituals. A whole file is the only honest unit. |
| Phone → server → desk | **Write**: one operation from the V33 §14 contract | The phone only ever appends, replaces one line, creates a file or (gated) replaces one section. Shipping the intent, not the file, keeps merging a matter of rules, not diffs. |

The server stores snapshots and queues writes. It merges nothing; it cannot read either.

### 4.3 Writes apply the same way everywhere

Both apps run the **same write-application rules** (§8). The phone applies its own write locally
the moment it is made, so the UI is instant; the desk applies the same write to the file on disk
when it arrives. Because the rules are deterministic over (file content, write), the two ends
converge without negotiating, and a write that no longer finds its target has one fixed answer:
keep both.

### 4.4 The vault is the record, the server is a cache

As with every other Thock sync (V13, V15, V31): what is on the desk's disk is the truth. The server
can be wiped and rebuilt from the desk. The phone can be wiped and rebuilt from the server. The
desk is never rebuilt from either without the user asking (a *restore* is a later V33 tier).

## 5. Architecture

```
┌──────────────────────────────┐        ┌──────────────────────────────┐        ┌──────────────────────────────┐
│ Desk · Thock (crates/thock)  │        │ Backend · thock/services/plus│        │ Phone · Thock iOS            │
│                              │ snaps  │ (Go, Postgres, Cloud Run)    │ snaps  │                              │
│ vault on disk, V2 history    │───────▶│ files: path, version, size,  │───────▶│ decrypted copy in app store  │
│ vault key, pairing QR        │        │        blob id, who, when    │        │ vault key from the QR        │
│ sync service: watch, upload, │ writes │ writes: queue until acked    │ writes │ write queue, applied locally │
│   pull queue, apply, ack     │◀───────│ feed: "something changed"    │◀───────│ rebases queue on new snaps   │
│ only while the app is open   │        │ APNs nudge when phone absent │        │ read-only after Plus lapses  │
│                              │        │ entitlement, devices, quota, │        │                              │
│                              │        │   lapse timer                │        │                              │
│                              │        │ blobs in object storage,     │        │                              │
│                              │        │   signed URLs                │        │                              │
└──────────────────────────────┘        └──────────────────────────────┘        └──────────────────────────────┘
        key never leaves ────────────────────── the server sees ciphertext ─────────────────── key never leaves
```

The backend is the existing V25 service with new tables and routes (§6, §7). Blobs go to an object
storage bucket beside it; the Go process hands out signed URLs and never streams file bodies.

## 6. Data model

### 6.1 What counts as text

Allow-list by extension, case-insensitive: `.md`, `.txt`, `.toml`, `.json`, `.csv`. Anything else
is invisible to sync. Also excluded, whatever the extension: `.thock/history/` (the V2 repository),
`.thock/cache/` and any other cache the desk writes, and files larger than **2 MB**. A file that
grows past the size limit stops syncing and is listed in the sync popover; it is not an error.

The vault's `.thock/*.toml` configs and `routines/**` do sync: the phone reads the configured
planner heading and backlog headings from them (V26) and never writes them (V33 §14).

### 6.2 Server (Postgres, new tables in `thock/services/plus`)

| Table | Row | Notes |
| --- | --- | --- |
| `vaults` | one per Plus user | `user_id` (unique, one vault per user), `created_at`, `bytes_used`, `quota_bytes` (200 MB default from the plan), `lapsed_at` (set when the entitlement ends; rows and blobs deleted 30 days later) |
| `devices` | the desk and the phone | `vault_id`, `role` (`desk` or `phone`, at most one of each), `credential_hash`, `apns_token` (phone only), `last_seen_at` |
| `files` | one per allow-listed path | `vault_id`, `path` (clear text, decided), `version` (monotonic per file), `size_bytes`, `blob_id`, `content_hash` (of the **ciphertext**, for dedup and integrity; the server never has the plaintext hash), `deleted` (tombstone), `updated_by`, `updated_at` |
| `writes` | the phone's queue | `vault_id`, `seq` (monotonic per vault), `path`, `base_version` (the file version the phone had when it made the write), `payload` (encrypted), `size_bytes`, `created_at`, `acked_at` (null until the desk applied it; rows pruned after ack) |
| `pairings` | one-time codes | `vault_id`, `code_hash`, `expires_at` (10 minutes), `used_at` |

The `files` table is the index the phone syncs against: *give me every file whose version is above
the last one I saw*. The `writes` table is what the desk drains on open and on every feed event.

### 6.3 Phone (SQLite, app group container, Data Protection "until first unlock")

Decrypted file contents live in one database, not as files (decision §15 #14). Tables: `files`
(path, server version, content, section index for the Today canvas), `pending_writes` (the local
queue with its `base_version` and whether the server has it), `captures` (V33 §14's device-local
record for receipts), `vault` (key reference in the Keychain, device credential, last feed cursor).

The Data Protection class is the one that lets background sync run while the phone is locked; the
Face ID gate of V33 §4.6 is the app's own and stays in front of every read screen.

### 6.4 Desk (inside `.thock/`, never synced)

`.thock/sync/state.json`: per path, the server version last uploaded or downloaded and the
plaintext hash at that moment. This is what *what changed while Thock was closed* is computed
against (§10.2). The vault key and device credential live in the OS keychain, as the Plus
credential already does.

## 7. Protocol

All routes are on the V25 service, authenticated with a per-device `Bearer` credential (the desk
reuses its Plus credential; the phone gets its own at pairing). Errors are `{"error": "<sentence>"}`
as today. The user's Plus entitlement is checked on every call.

### 7.1 Pairing

1. Desk: *Connect your phone* in the sync popover → `POST /v1/vault` (idempotent, creates the vault
   row and registers the desk device) → `POST /v1/vault/pairings` → a one-time code valid 10
   minutes. The desk shows a QR holding `{code, vault_key, backend_url}`. The key is in the QR and
   only there; the backend has the code's hash.
2. Phone scans → `POST /v1/vault/pair` with the code and its APNs token → a phone device credential.
   The key goes into the Keychain. A second phone pairing replaces the first phone device, and the
   old credential is revoked.
3. Phone performs a full pull (§7.3). Done.

**Re-pairing** (lost or replaced phone, or *Disconnect phone* at the desk) rotates the vault key:
the desk generates a new key, re-uploads every file (cheap under the cap), and the old phone's
credential and key are both useless. Decision §15 #16.

### 7.2 Desk upload

For every changed allow-listed file (watched through the project `Fs` while open; diffed on open,
§10.2): encrypt (§9) → `POST /v1/vault/files/{path}` with `{expected_version, size, content_hash}` →
the server answers a signed upload URL and the new version, or `409` when `expected_version` is
stale (a phone write was applied to the server's view of that file; the desk pulls, re-applies,
retries). Deletes and renames are `DELETE /v1/vault/files/{path}` (tombstone) plus an upload at the
new path; the phone treats a rename as delete-then-create, which is correct because the phone's
own writes name a path and a line, never a file identity.

Uploads are coalesced per file with a short debounce so a typing session does not produce a
version per keystroke; the heartbeat of V2's checkpoint service is a reasonable clock to share.

### 7.3 Phone pull

`GET /v1/vault/files?since=<version cursor>` → the list of files above the cursor, with signed
download URLs for the ones whose `content_hash` differs from the phone's. Download, decrypt, store,
then **rebase** every pending write whose `base_version` is below the new version (§8.3), advance
the cursor. A full pull is the same call with no cursor.

### 7.4 Phone write

Apply locally, then `POST /v1/vault/writes` with `{path, base_version, payload}`; the server
assigns `seq`. The phone keeps the write in `pending_writes` until the server has it and marks it
*sent*; it is removed once a snapshot at or above the write's applied version arrives from the desk
and the write's effect is present in it. A write is idempotent on the server by a client-side id,
so a retried upload after a dropped connection never duplicates an append.

### 7.5 Desk drain

On open and on every feed event: `GET /v1/vault/writes?after=<last acked seq>` → apply each write in
`seq` order to the file on disk (§8), after one V2 checkpoint for the batch → upload the new
snapshots (§7.2) → `POST /v1/vault/writes/ack {through: seq}`. The ack is what prunes the queue. If
the desk crashes between apply and ack, the writes are applied again on the next open; §8 makes
every write idempotent against its own effect, so this is safe.

### 7.6 Change feed and nudges

`GET /v1/vault/feed` is a one-way stream (server-sent events) that emits `{kind: "file"|"write",
version|seq}` whenever the other device changed something. Both apps hold it open while in the
foreground and reconnect on drop (Cloud Run closes connections after an hour; that is fine). When
the phone is not connected and a snapshot arrives, the backend sends one **APNs silent push** per
burst; iOS wakes the app briefly to pull. This is the "seconds in the foreground, Apple's schedule
in the background" promise of §2.

The desk is never pushed: it is either open and on the feed, or closed and catching up on open.

## 8. Writes and merge

### 8.1 The write vocabulary

Exactly the V33 §14 contract, as data:

| Write | Fields | Target |
| --- | --- | --- |
| `create` | `path`, `content` | create-if-missing. If the file exists with other content, the write becomes an `append` of the content under no heading, so a capture is never lost to a race |
| `append` | `path`, `heading` (normalised, V26) or `null` for end of file, `lines`, `create_from_template: bool` | the end of the section under `heading`, before the next heading of the same or higher level |
| `replace_line` | `path`, `heading`, `line_hash`, `new_line` | the single line whose normalised text hashes to `line_hash` within the section |
| `remove_line` | `path`, `heading`, `line_hash` | the same |
| `replace_section` | `path`, `heading`, `base_hash` (of the section's current lines), `lines` | the line range of the section (V33 §7.3, after the before/after screen) |

`line_hash` is over the line with leading list markers, checkbox state, time prefix and trailing
whitespace stripped, so *tick* and *set a time* on the phone still find a line the desk retimed or
ticked meanwhile. Two lines with the same normalised text in one section are disambiguated by
ordinal; if the ordinal is out of range, the first match is used.

### 8.2 Application rules

Deterministic, shared by both apps (§16 #2 decides where the code lives, not what it does):

1. **Missing file**: `append` with `create_from_template` creates the note from the vault's
   template (daily and weekly only, V33 §6.2); otherwise the file is created with just the heading
   and the lines. Nothing is ever dropped for want of a file.
2. **Missing section**: the heading is appended at the end of the file (before any level-1 agent
   heading such as `# Daily Closure`), then the write applies. V33's *the phone never files*
   stands: adding a heading the template lacks is the minimum to keep the user's words.
3. **`replace_line` / `remove_line` target found**: the line is replaced or removed. Idempotent: a
   second application finds the new line and does nothing (its hash matches `new_line`).
4. **Target not found**: `replace_line` becomes an append of `new_line` at the end of the section
   with the **conflict marker** (§8.4). `remove_line` does nothing (the line is already gone;
   nothing to lose).
5. **`replace_section` with a stale `base_hash`**: the section is not rewritten. The phone's
   version of the section is appended below the current one under the same heading with the
   marker on its first line. This is the one write that can produce a bulky duplicate, which is why
   V33 gates it behind a confirmation and the phone warns when its copy is older than the desk's.
6. **Order**: writes apply in `seq` order on the desk and in creation order on the phone. Appends
   from the two devices to one section interleave by arrival; neither is lost.

### 8.3 Rebase on the phone

When a snapshot arrives for a file with pending writes, the phone re-derives its local content:
take the snapshot, apply every pending write in order with the rules above, store. The pending
writes keep their original `base_version` (the server needs it only for ordering and diagnostics;
the desk applies against whatever is on disk). A write already acked whose effect is visible in the
snapshot is dropped from the queue.

### 8.4 The conflict marker

A kept-both line ends with ` <!--thock:also-->`, in the style of V13's `<!--inbox:…-->`. Every
editor tolerates an HTML comment; the desk conceals it (V10) and highlights the line with a quiet
*two versions of this line* hint in the gutter; deleting either line deletes its marker. The phone
renders the marked line normally. Nothing counts markers, nothing nags: the user tidies at the desk
when they see it, or not.

## 9. Encryption and keys

- **Primitive:** ChaCha20-Poly1305 (IETF, 12-byte nonce) with the vault key, one random nonce per
  blob, the path and version as associated data so a blob cannot be replayed at another path or
  version. Native on both ends (CryptoKit on iOS, the RustCrypto crate on the desk); §16 #2 may
  move both behind one Rust core.
- **What is encrypted:** every snapshot body and every write payload (the whole write JSON, so the
  server does not learn headings or line hashes).
- **What is in the clear:** `path`, `version`, `size_bytes`, `content_hash` of the ciphertext,
  timestamps, device role. Decided (§15 #8): per-file sync, debuggability and *what changed*
  support without ever reading a note.
- **Key storage:** OS keychain on the desk; iOS Keychain, `WhenUnlockedThisDeviceOnly`
  equivalent class that still allows background use, on the phone. Never in the vault, never in
  `.thock/`.
- **Rotation:** on re-pairing (§7.1) and on *Turn sync off* at the desk (which also deletes the
  server copy). No periodic rotation.
- **Credentials:** the desk's Plus credential; a separate phone credential minted at pairing and
  revocable alone (`POST /v1/vault/devices/{id}/revoke`, also from the desk's popover as
  *Disconnect phone*).

## 10. Desk side (`crates/thock`)

### 10.1 A sync service like V15 and V31

`VaultSyncService` in `crates/thock/src/vault_sync.rs`: starts with the vault when a Plus
credential and a vault key exist, holds the feed, watches the vault through the project `Fs`,
debounces and uploads, drains and applies writes, and exposes a status row the existing sync
popover renders beside Gmail, Calendar and Readwise: *Phone · up to date · 2 min ago*, *Phone ·
3 waiting from your phone*, *Phone · not connected*, *Phone · 1 file too large to send*.
Failures surface in that row and in a toast with the server's sentence; nothing fails silently.

Before applying a batch of writes it asks the V2 checkpoint service for a checkpoint, exactly as
the hosted agent does before a session, so *undo what the phone did* is a restore away.

### 10.2 Catching up on open

Thock was closed; files may have changed through Obsidian, a shell, or a ritual run at the terminal.
On open the service hashes every allow-listed file (plaintext, locally) against
`.thock/sync/state.json`, uploads the differences and tombstones the missing, then drains the
write queue, then applies and uploads. Mtime is not trusted. The hash pass over a 200 MB vault is
sub-second on any laptop and runs off the foreground thread.

### 10.3 UI

All in the Thock crate: *Connect your phone* (QR), *Disconnect phone*, *Turn sync off* (with the
sentence *This deletes the copy Thock keeps for your phone. Your notes on this computer are not
affected.*), the status row, the too-large list, and the conflict hint in the editor gutter. One
keymap entry for `thock::ConnectPhone`. No new upstream touch-points beyond that.

## 11. Phone side

- The write queue and the rebase of §8.3 are the phone's whole sync logic; the Today canvas reads
  from the local store and never waits on the network.
- The Lock Screen control and widgets write and read through the app group container; a capture
  made from the Lock Screen is queued like any other and sent when the app next runs.
- **Read-only after lapse** (§12): the phone keeps showing the last vault it has; the compose dock
  and every nudge are replaced by one sentence with a *Renew Thock Plus* link.
- The phone never materialises files. *Export* (a zip of the decrypted text files) is a later tier
  so that *your files, forever* holds on the phone too; noted in §16 #9.

## 12. Plus, quota and lapse

- Sync is a Plus benefit under the existing plan rows: `plans.limits.vault_quota_bytes` (200 MB
  default) joins the plan JSON; `GET /v1/entitlement` reports `vault: {quota, used, lapsed_at}`.
- At the cap the desk stops uploading **new files under `reference/`** first and says so in the
  status row; notes under `daily/`, `weekly/`, `inbox/` and `backlog.md` keep syncing until the
  hard cap plus 10 %. The user is told which folder is big. Nothing on the phone is deleted.
- On lapse (cancel, refund, chargeback, revoke): `vaults.lapsed_at` is set, the phone is told on
  its next call and goes read-only, the desk's status row says *Phone · paused, renew Thock Plus*.
  Thirty days later a backend sweeper deletes the vault's rows and blobs. Renewing inside the
  window resumes; after it, pairing starts over with a fresh upload.

## 13. The agent working set (Ask on the phone)

V33 §11 wants the hosted agent on the phone; V25 forbids server-side execution; E2E means the
server cannot read the vault. Decision §15 #6 resolves it with a **bounded exception**: for an Ask
turn the phone decrypts a working set and sends it with the question to an **ephemeral agent
runner** on the backend that holds nothing after the turn. The runner is the V25 harness in a
per-session sandbox fed files from the request instead of a disk.

This spec fixes only the data-side obligations, since Ask is a second-tier V33 feature:

- The phone can produce, from its local store, the set *today, this week, `backlog.md`,
  `memory/index.md`, `profile.md` if present, the inbox notes still waiting*, decrypted, with their
  vault-relative paths, in one call.
- Any write the agent wants is returned to the phone as writes from §8.1 and enters the phone's
  queue, so the agent's write scope on the phone is exactly the phone's.
- The runner, its sandbox, its cost cap per turn and the VISION §4 wording (*notes never leave the
  machine for the agent's sake* becomes *…except the notes you choose to ask about from your phone*)
  are a V25 amendment to write when Ask ships. Logged as §16 #7.

## 14. Tests

- **Rules are deterministic:** a fixture corpus in `crates/thock/` (every shipped template, the
  example first day, notes with tables, code, comments, frontmatter, nested lists, duplicate lines
  in one section) × every write kind × {target present, target changed, target missing, section
  missing, file missing} → expected output. The same fixtures run on the phone (§16 #2 says how).
- **Idempotence:** applying any write twice equals applying it once.
- **Convergence:** for the three scenarios of the design page (desk asleep, phone offline, same
  line edited twice), simulate both devices against an in-memory server; the final files are
  identical on both ends and no line from either side is missing.
- **Round-trip:** encrypt → upload → download → decrypt is byte-identical, and a blob moved to
  another path or version fails to decrypt.
- **Catch-up:** change, add, delete and rename files on disk while the service is stopped; on
  start the server's index matches the disk.
- **Backend:** pairing code expiry and single use; one desk and one phone per vault; `409` on
  stale `expected_version`; ack prunes; quota refusal; lapse sweeper deletes after 30 days and
  not before; feed emits for the other device only.
- **Allow-list:** a `.png`, a 3 MB `.md` and `.thock/history/**` never produce an upload.

## 15. Decision log (2026-10-02)

From the interview, with Diego's answers:

1. **Desk asleep:** the phone still receives the latest. The server stores state, not just relays.
2. **Server holds** the full encrypted vault, always. The stored state is the backup; Plus
   includes it, so the earlier "backup is opt-in" wish is withdrawn as a separate feature.
3. **Topology:** one desk, one phone.
4. **Conflicts:** both lines kept inline with a quiet marker; the desk highlights, the user tidies.
5. **Metadata:** paths in the clear, contents encrypted, so files sync one at a time.
6. **Agent on the phone:** the phone sends the notes it needs to an ephemeral runner. A bounded
   exception to V25's no-server-side-execution rule, to be written up when Ask ships (§13).
7. **Keys:** QR pairing only, no recovery phrase, no escrow.
8. **Phone copy:** every allow-listed text file.
9. **History:** latest state only on the server.
10. **Desk process:** sync runs only while Thock is open.
11. **Latency:** pushed, seconds in the foreground; Apple's schedule in the background.
12. **Plus lapse:** phone read-only, server copy kept 30 days.
13. **Text scope:** Markdown plus the vault's own config formats, by extension allow-list.
14. **Phone storage:** whatever gives the best experience; files need not be materialised.
15. **Pairing:** one QR from the desk. Accounts may replace license keys later (V25 Stage 2).
16. **Re-pairing** rotates the vault key and re-uploads (follows from #7).
17. **Backend:** extend `thock/services/plus`.
18. **Quota:** about 200 MB per vault. **Vaults:** one per Plus user.

## 16. Open decisions, for the first implementation iterations

None of these change the architecture above. Each is a card on the design page with a leaning;
the first iteration that touches the area decides it and records the decision here.

1. **Merge model, confirm or overturn** — this spec is written on *semantic writes from the phone,
   snapshots from the desk* (§4.2, §8). The alternative is a three-way line merge on snapshots
   with a base per file. The write-queue tables and routes follow from the choice, so this is the
   one item to settle before the backend API is frozen. Leaning: as written; keep three-way merge
   as the fallback for `replace_section` if §8.2 rule 5 proves too blunt in use.
2. **Where the shared rules live** — a Rust core in `crates/thock` compiled for iOS through UniFFI
   (one implementation, the fixtures of §14 run once), or a Swift port kept honest by the same
   fixtures. Leaning: Rust core; drift between two implementations means divergent files.
3. **Blob store** — a GCS bucket beside Cloud Run or Supabase Storage beside Postgres. Either way,
   signed URLs and a sweeper for orphaned blobs. Leaning: whichever has the simpler signed-URL
   story in Go; cost is similar at this scale.
4. **Feed transport** — server-sent events (one-way is enough) versus WebSockets. Leaning: SSE.
5. **Phone stack** — SwiftUI over the core from #2 with SQLite (GRDB) and the Data Protection
   class of §6.3. The alternative, plain files in the sandbox, loses on section queries and on
   extension coordination. Leaning: SQLite.
6. **A desk background helper** — only if *desk asleep* turns out to mean *desk closed for days*
   for real users; the service of §10.1 should be a separable crate so a helper can wrap it.
7. **The ephemeral agent runner** (§13) — sandbox shape, per-turn cost cap, VISION wording.
   Owned by the Ask tier of V33, not by this spec.
8. **Journal timestamp locale and the time-chip grammar across devices** — V33 §19; the rules of
   §8.1 strip the time prefix for hashing so this does not block sync.
9. **Export from the phone** — a zip of decrypted text files, so *your files, forever* holds on the
   phone. Later tier, named here so it is not forgotten.
10. **A third device** — a second desk would need the desk role to become *any device that
    uploads snapshots*, with `expected_version` conflicts resolved by the three-way merge of #1
    rather than refused. Not planned; recorded so #1 is chosen with it in mind.

## 17. Delivery

1. **Backend:** tables of §6.2, routes of §7, quota and lapse of §12, feed and APNs of §7.6. Tests
   of §14 (backend rows). No UI.
2. **Desk:** `vault_sync.rs` with pairing QR, upload, catch-up, drain, apply with the §8 rules and
   the fixture corpus, status row, conflict hint. Usable alone: a developer can pair a test client.
3. **Phone:** the store, the queue, the rebase, the pull, pairing. This is the V33 first-release
   data layer; the V33 screens sit on top.
4. **Ask tier:** §13's working-set call and the V25 amendment, with V33's second tier.

VISION §12 Milestone 6 gains this spec's row; §4.3's *sync storage next* becomes this.
