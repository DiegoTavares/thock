# Thock V50 - Desk changes reach a pocketed phone and the wrist

**Status:** Shipped (2026-10-29): merged as #170, the APNs key deployed to Plus, and §8's by-hand
checks passed on TestFlight build 23. The push half was first built on
`v45-push` (2026-10-22); §4 reviews it and §5 lists what it is missing. §11 lists where the build
corrected this spec.
**Owner:** Diego · **Date:** 2026-10-29
**Companion docs:** `v34-vault-sync.md` §7.6 and `v34-vault-sync-api.md` §6.5 (the feed and the
silent push), `v45-reminders.md` §10.6 (how fast the phone hears about desk changes),
`v48-watch.md` §6 (the watch link)

---

## 1. Summary

The watch knows only what the phone tells it, and the phone hears about desk changes only while its
app is open. A desk edit made while the phone sits in a pocket reaches neither the phone's alerts
nor the watch's timeline until someone opens Thock on the iPhone. V50 closes that gap two ways:

- **Silent push (desk → phone).** The service sends the phone a silent push when the desk changes
  the vault. iOS wakes the app briefly, the phone syncs, rings what is new and sends the watch a
  fresh snapshot. This is V34's design, mostly built on `v45-push`.
- **Refresh on open (watch → phone).** Opening the watch app asks the phone to sync. The ask wakes
  the iPhone app in the background, which is the path a watch capture already takes, so the watch
  shows the truth a few seconds after you raise it even when Apple has held a push back.

## 2. Goals & success criteria

- A desk edit to today's planner shows on the watch's complication within about two minutes, with
  the phone locked in a pocket and the iPhone app suspended or not running, as long as iOS delivers
  the push (§3).
- A reminder typed at the desk for later today rings on the phone and the wrist without the iPhone
  app being opened (V45 §10.6's gap).
- Opening the watch app with the phone in reach shows desk changes within ten seconds, whatever
  state the iPhone app is in.
- A desk writing session costs the phone a handful of wakes an hour, not one per save (§6.1).
- The service never sends note content in a push. The payload stays V34 API §6.5's ids and
  counters.

## 3. Non-goals

- **Scheduled background refresh** on the phone (`BGAppRefreshTask`) or the watch
  (`WKApplicationRefreshBackgroundTask`). iOS and watchOS both decide when these run. Add them only
  if the complication still looks stale with V50 in place.
- **The watch as its own sync device** (V48 §14 D1).
- **Pushing to the watch directly** (PushKit complication pushes, WidgetKit push). The service
  cannot read notes, so a push could only wake the watch, and the watch would still need the phone.
- **Guaranteed delivery.** Background pushes are low priority by Apple's design. iOS rations them
  per app, delivers none to an app swiped away from the app switcher or with Background App Refresh
  turned off, and holds them in Low Power Mode. Refresh on open is the backstop. Nothing in the UI
  promises push timing.

## 4. What `v45-push` holds

Three commits on top of build 15 (`30d31d7b11`):

| Commit | What | Onto main |
|---|---|---|
| `beb681031c` Send the phone's silent push through APNs | `apns.go`: an HTTP/2 client with an ES256 provider token (reused for 40 minutes, minted again on `ExpiredProviderToken`). `410`, `Unregistered` and `BadDeviceToken` forget the token, only if it is still the one that failed. `APNS_*` config with a `production` default. `deploy.sh` stores the key as secrets. README and API spec notes. Eight Go tests against a fake APNs. | Cherry-picks cleanly. Keep it, with §5 F2 and F3. |
| `802cc93952` Register the phone for silent push and sync when one arrives | `aps-environment` on the app only (extensions move to a new `ThockExtensions.entitlements`). `registerForRemoteNotifications` at launch. The token goes along at pairing and on change (`PATCH /v1/vault/devices/me`), with a `pushTokenError` diagnostic. `syncForPush` boots, syncs and stops the feed in the background. One ThockKit test. | Conflicts in `ThockApp.swift` and `AppModel.swift`: V45 has since moved `AppModel` into `AppDelegate` and added the notification delegate. Redo it by hand on main, with §5 F1, F4 and F5. |
| `83f04d9011` | The merge of the two. | Drop it. |

Already on main: `UIBackgroundModes` → `remote-notification`, `AppDelegate.model`, and a
`didReceiveRemoteNotification` that only calls `becameActive()` and answers `.newData`.

The design is right: the existing coalescer and `pusher` interface, a background push with
priority 5, the token in the device row, and a forgotten token cleared only if unchanged.

## 5. Findings

**F1. A forgotten token is never offered again.** `updatePushToken` skips the request when the
store's `apns_token` meta equals the token. When the service forgets the token (a sandbox token
against the production service, an app reinstall the service learned about from APNs), the phone
still holds the same token and never sends it again, so pushes stop for good. The README's "the
phone sends it again on its next launch" is not true, and the test asserts the skip. **Fix:** offer
the token once per engine instance, remembered in memory rather than in the store. That costs one
`PATCH` per launch.

**F2. The push runs outside any request.** Plus runs on Cloud Run with request-based billing
(`--min-instances 0 --max-instances 1`, `deploy.sh`). CPU is allocated only while a request is
being served. The coalescer sends the leading push from a `go` statement after the triggering
request returns, and the trailing push from a `time.AfterFunc` thirty seconds later, outside any
request. The first is likely to stall. The second can wait until the next request arrives, or be
lost if the instance scales to zero. **Fix (§6.1):** send the push inside a request. The leading
push goes out before the triggering request answers, and a held push goes out on the vault's next
request once its spacing has passed. While the desk is open it polls `GET /v1/vault/writes` every
minute, so a held push leaves within a minute of becoming due.

**F3. Thirty seconds is too often for Apple.** The window lets a desk writing session send about
120 pushes an hour. Apple treats background pushes as low priority, advises against sending more
than two or three an hour, and an app over its budget gets them dropped or delayed. **Fix (§6.1):**
the first change after a quiet spell is pushed at once, later ones are held to one push per spacing
interval, and the held push carries the latest state.

**F4. A background wake ends before the watch and the alerts hear of it.** `syncForPush` syncs,
then calls the completion handler, and iOS may suspend the app right after. Two things are lost:

- Main's background tail (`sendInBackground`) refreshes the alerts, but `syncForPush` does not, so
  a desk reminder would not be scheduled until the next open.
- The watch snapshot and the widget reload are debounced by 1.5 s (`scheduleWatchSnapshot`,
  `scheduleWidgetReload`), so the completion usually runs first. The watch request path has the
  same race today: `sendInBackground` ends its background task after `refreshAlerts`, which can be
  before the debounce fires. That may be why desk changes reach the watch only once the phone is
  opened.

**Fix (§6.2):** one background tail for every wake, which sends the snapshot and reloads the
widgets before it ends.

**F5. `sync()` returns early when a round is already running.** `SyncEngine.sync()` sets `again`
and returns at once if a round is running (the feed loop, or a `storeChanged` round). A background
caller awaiting it then reports done before the data has landed. **Fix:** a caller that joins a
running loop waits for the loop to finish, including the `again` round it asked for.

**F6. A push is dropped while a stale feed looks open.** `sendPush` returns if
`phoneListening(vaultID)`. The phone closes the feed on its way to the background, but a stream an
app left behind when it was killed lives until the server's five-minute cut, and a push skipped in
that window is never retried. **Fix (§6.1):** a skipped push stays held and goes out on a later
request once no phone listens.

**F7. Needs a real-device check:** on a cold launch by push, `WCSession` activation is
asynchronous, and `PhoneWatchLink.canSend` is false until it completes. The tail's watch flush
waits for activation for up to two seconds (§6.2) and logs if it gives up.

## 6. Design

### 6.1 Service: when a push goes out

The coalescer keeps, per vault, `lastSent` and a `held` flag in place of the timer:

- `nudge(vaultID)` is called inside the request that changed the vault (the desk's commit, its
  ack), for every `file` or `ack` event the desk causes, whether or not a phone holds the feed,
  and before the event fans out. It marks the vault `held`, and if no push has gone out within
  the spacing interval it sends one synchronously, bounded by a 5 s timeout, before the request
  answers.
- **Every authenticated desk request** checks `held` once the request is done: if the spacing has
  passed and no phone listens, it sends the push synchronously before answering.
- **A phone request clears `held`** when it starts, since the phone is syncing anyway. Clearing at
  the start, not the end, keeps a change made during a long phone request (a feed stream) held.
- A push skipped because a phone listens leaves the vault `held` (F6). It is not counted as sent,
  so it goes out on the first desk request after the feed closes. Any other failure (no token,
  APNs unreachable) is logged and not retried: refresh on open is the backstop.
- **Spacing: 10 minutes** (D1). The first desk change after a quiet spell reaches the phone in
  seconds. A long session wakes it at most six times an hour, each time with the latest state.
- `max-instances 1` keeps this in-memory state consistent. A cold instance starts with nothing
  held and with no `lastSent`, so its first change pushes at once.

The payload, headers and token forgetting are `beb681031c`'s, unchanged.

### 6.2 Phone

- **Token.** `registerForRemoteNotifications` in `didFinishLaunching`, beside the notifier and the
  watch link. The token goes along at pairing, and `updatePushToken` sends it once per engine
  instance and again if iOS hands over a different one (F1). Practice vaults send nothing.
  `diagnostics.pushTokenError` shows in the You sheet (`Sheets.swift`), under *Last checked*, as *Desk changes
  arrive when you open Thock*, with the reason under it.
- **One background tail**, `syncInBackground() async`, which replaces `sendInBackground` and
  `syncForPush`:
  1. Begin a background task.
  2. `await boot()`, and stop if the phase is not `.ready`.
  3. `await engine.sync()`, which now waits for a running loop (F5).
  4. Stop the feed if the app is in the background.
  5. `await refreshAlerts()`.
  6. `flushWatchSnapshot()`: cancel the debounce, wait for `WCSession` activation for up to 2 s,
     send now, restamped even if nothing the watch draws changed. Reload the widgets the same way.
  7. End the background task.

  The silent push handler, watch writes, watch refreshes (§6.3) and notification answers all go
  through it. The push answers `.newData` when step 3 pulled anything and `.noData` otherwise, or
  `.failed` if the tail is still running at 25 s, inside iOS's thirty.
- `aps-environment` stays on the app target only. Archives for TestFlight get `production` from
  automatic signing.

### 6.3 Watch: refresh on open

- A new message, `WatchRefresh { id, at }`, type `refresh`, watch → phone. It is never put in the
  outbox and never sent by `transferUserInfo`, because a refresh delivered late is worthless.
- `WatchModel.becameActive()` sends it by `sendMessage` when the app is active, the phone is
  reachable, the snapshot's `madeAt` is more than 60 s old and no refresh is in flight. It sends at
  most one a minute. The same check runs when the phone comes into reach and when the watch's
  session finishes activating, because a cold launch is not reachable yet when it becomes active.
- The phone runs `syncInBackground()`, then replies with an empty `ack` for the refresh's id. The
  new snapshot reaches the watch by application context, as every snapshot does. Nothing is
  recorded in `watch_requests`, because a refresh is idempotent.
- The watch draws nothing new. If the snapshot changes, the screen redraws. No spinner, since an
  unanswered refresh leaves things as they were.
- **Versions.** An older phone cannot decode `refresh` and answers with an empty dictionary. For a
  refresh the watch takes any answer as the end of the wait: it does not read an empty one as "not
  understood", which would ask for an update and block captures an older phone still handles. The
  snapshot's `v` is unchanged.

### 6.4 Cost

- **APNs** is free.
- **Cloud Run.** One push and one phone round per wake (a few requests: flush, pull, the changed
  files' blobs). With 10-minute spacing a full desk day costs at most about 60 wakes, plus one round
  per watch open. That stays far inside the account's shared 2M-request free tier.
- **Battery.** At most six wakes an hour of desk activity, each a few seconds of network.

## 7. Changes by file

| File | Change |
|---|---|
| `thock/services/plus/apns.go`, `apns_test.go`, `deploy.sh`, `main.go`, `vault_store.go`, `README.md` | From `beb681031c`. |
| `thock/services/plus/feed.go` | The coalescer: spacing, `held`, synchronous send, no timers (F2, F3, F6). |
| `thock/services/plus/vault.go` | `sendPush` returns whether it sent or should stay held. The held check runs after each authenticated vault request. |
| `thock/ios/Config/Thock.entitlements`, `ThockExtensions.entitlements`, `Thock.xcodeproj` | From `802cc93952`. |
| `thock/ios/Thock/ThockApp.swift` | Token registration and its delegate callbacks. The push handler calls `syncInBackground`. |
| `thock/ios/Thock/AppModel.swift` | `received(pushToken:)`, `syncInBackground` (§6.2), `flushWatchSnapshot`, the refresh request. |
| `thock/ios/Thock/Sheets.swift` | The push line in sync details. |
| `thock/ios/ThockKit/Sources/ThockKit/Sync/SyncEngine.swift` | `updatePushToken` once per instance (F1). `sync()` waits for a running loop (F5). |
| `thock/ios/ThockKit/Sources/ThockKit/Watch/WatchMessages.swift` | `WatchRefresh` and the `refresh` case. |
| `thock/ios/ThockWatch/WatchModel.swift` | The refresh on `becameActive`, sent outside the outbox. |
| `thock/specs/v34-vault-sync-api.md` §6.5, `v34-vault-sync.md` §7.6 | Spacing and in-request sending replace "one per thirty-second burst". |
| `thock/specs/v45-reminders.md` §10.6, `v48-watch.md` §2 | Point at V50. |
| `thock/VISION.md` §12 | The V45 entry's push note, and a V50 entry. |

## 8. Tests

- **Go** (`thock/script/test go`):
  - A nudge after a quiet spell pushes before the request answers.
  - A second nudge inside the spacing is held. A desk request after the spacing sends it, and one
    before the spacing does not.
  - A phone request clears a held push.
  - A push skipped because a phone listens stays held and goes out once the feed closes.
  - `beb681031c`'s APNs tests carry over unchanged.
- **ThockKit** (`swift test` on palloma):
  - The token goes along at pairing.
  - A new engine instance offers an unchanged token once, and the same instance offers it once
    only.
  - A refused offer is retried on the next call and recorded in `pushTokenError`.
  - `sync()` called during a running round returns after a round that started after the call
    (a slow fake transport).
  - `refresh` round-trips through the dictionary form, and an older reader rejects it as an
    unknown type.
- **Smoke** (`script/smoke`): `-thock-watch-script "refresh:<line>"` has the practice desk add the
  line to today, sends a refresh, and asserts that the snapshot the tail flushed holds it. A
  simulator has no watch to send to, so a DEBUG build running a watch script writes each flushed
  snapshot to `tmp/watch-snapshot.json`.
- **By hand, on a real phone and watch:**
  - With the phone locked and the iPhone app terminated, a desk planner edit reaches the
    complication.
  - A desk reminder for 15 minutes ahead rings on the wrist.
  - Opening the watch app with the iPhone app terminated shows a fresh desk change.
  - A desk session of 20 saves over 15 minutes wakes the phone at most twice (service logs).
  - Note what happens after a swipe-away and in Low Power Mode, without fixing it.

## 9. Build order

1. Branch `v50-background-sync` from main. Cherry-pick `beb681031c`, then the service changes of
   §6.1 with their tests.
2. The phone half by hand (§6.2), with the ThockKit tests.
3. The watch refresh (§6.3) and the smoke step.
4. **Diego, at developer.apple.com:** an APNs auth key (Keys → Apple Push Notifications service;
   one `.p8` serves sandbox and production), and confirm Push Notifications on App ID
   `com.thethock.ios` (automatic signing should add it on the first archive with the entitlement).
5. Merge the PR, which deploys `thock/services/plus` with the logging pusher. Then
   `APNS_KEY="$(cat AuthKey_….p8)" APNS_KEY_ID=… ./deploy.sh` once.
6. A TestFlight build, then the by-hand checks of §8.

## 10. Decisions (2026-10-29)

1. **D1. Push spacing: 10 minutes**, after an immediate first push. Five would mean up to twelve wakes an hour, more likely to hit iOS's budget, and twenty leaves a burst of desk edits that long out of the pocket. Refresh on open covers the gap.
2. **D2. No scheduled background refresh in V50** (§3). Revisit only with evidence of stale faces.
3. **D3. A refresh is not a request.** It is never queued, never recorded and never retried, so
   the outbox stays the record of what the wrist wrote.

## 11. Corrections made while building (2026-10-29)

1. **The nudge runs for every desk change, not only when no phone hears it.** `beb681031c` called
   the coalescer only when an event found no phone subscriber, so a stream a killed app left
   behind swallowed the push before `sendPush` could hold it, and F6's fix had nothing to hold.
   §6.1 now nudges on every desk `file` and `ack` event, before the fan-out, so the held flag is
   set before the phone's own requests can clear it.
2. **A phone request clears `held` when it starts** (§6.1), so a change during a feed stream stays
   held for the next desk request.
3. **The APNs test's second push** advances the clock by the spacing instead of a minute; the rest of
   `beb681031c`'s tests are unchanged.
4. **The watch also refreshes when the phone comes into reach and when its session activates**, and
   ignores an empty answer to a refresh (§6.3).
5. **`syncInBackground` opens the notebook only while it is still loading.** The launch arguments
   run inside the opening, so awaiting it from a smoke step would wait on itself.
6. **The background tail restamps the watch's snapshot even when nothing changed.** The phone's
   link skips a snapshot that draws the same as the last one, so a refresh that found nothing new
   left the watch on its old instant: it still read as stale, said when it was read, and asked again
   a minute later. A snapshot sent after a sync goes out regardless.
7. **The silent push answers iOS within 25 seconds.** iOS gives a push thirty seconds and may stop
   waking an app that runs over, and a round on a slow network can take longer, so the handler
   answers `.failed` at 25 seconds if the round is still going. The round keeps running under its
   background task.
