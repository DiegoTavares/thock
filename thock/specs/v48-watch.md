# Thock V48 - Thock on the wrist: today's timeline, the day's tasks and a capture that listens

**Status:** Designed (2026-10-28). Not built.
**Owner:** Diego · **Date:** 2026-10-28
**Design reference:** the clickable watch prototype, the complication families, the architecture
diagram and the findings on what watchOS allows: <https://claude.ai/artifact/86BQKftLcLJPpmBydT3x7s>.
**Companion docs:** `v33-iphone-companion.md` (the phone, its capture sheet and entry points),
`v34-vault-sync-api.md` §6.2, §6.5 (one phone per vault, the feed and push), `v45-reminders.md` §10
(the *Reminder* chip, *Remind me*, the phone's alerts), `v46-phone-edit-in-place.md` §5 (drafts that
survive leaving), `v4-day-planner-panel.md` (the planner grammar the timeline draws)

---

## 1. Summary

The phone is the pen for moments away from the desk, but it still has to come out of a pocket.
V48 adds an Apple Watch app that ships inside the iPhone app:

- **A Today timeline complication.** The large (`accessoryRectangular`) complication draws a
  five-hour window of today's plan, like the Calendar complication. It shows tasks as blocks,
  calendar events outlined, reminders as ticks and a "now" marker that moves on its own. The
  header names the next thing. Tapping it opens the day.
- **The day's tasks.** A list of the day's planner lines, calendar events and reminders. The
  Digital Crown scrolls it, the chevrons or a swipe move a day back or forward, and a tap on a box
  ticks the line.
- **A capture that is already listening.** The small complication (circular and corner) opens
  capture with dictation running. You speak, see the words, choose *Today*, *Inbox*, *Backlog* or
  *Reminder*, and save. A reminder takes its time from a picker with the phone's presets; spoken
  dates are never parsed.
- **The phone does the vault.** The watch never holds the vault key, never talks to the server and
  never parses Markdown. The phone sends it a reading of the days, and it sends the phone requests:
  capture, tick, read a day. The phone runs each request through the code path its own screens use.

## 2. Goals & success criteria

- From the watch face to dictating is one tap on the Capture complication. Saving a dictated
  capture to the remembered chip takes two more (*Done*, *Save*).
- A capture saved on the watch while the phone is in reach lands in the phone's store within
  seconds, even if the iPhone app was suspended or not running. It then reaches the vault on the
  phone's next sync, exactly as if it had been typed on the phone: the same bytes, the same digest
  rules, the same `reminders.md` line and id, and the same immediate local alert for a reminder.
- A capture saved with the phone out of reach waits on the watch, survives the watch app being
  terminated, and is delivered once when the phone is back. It is never delivered twice.
- At 14:00 on a day whose plan has a 14:00 dentist reminder, the timeline's header reads
  *14:00 Dentist* from the moment the previous item ends, with no phone contact that day.
- A desk edit to today's planner shows on the timeline within a minute of the phone syncing it,
  while the phone is awake.
- Ticking a planner line on the watch changes exactly one line of the vault, the same line a tick
  from the iPhone widget changes. A tick on a line the desk has since changed is refused, and the
  watch says so.
- A reminder rings once on the wrist (the phone's alert, mirrored), and its *Done*, *In 1 hour* and
  *Tomorrow 09:00* answers work from the watch.
- Every failure the watch can see is said in words on the watch: not paired, Thock Plus ended,
  refused by the phone, still waiting for the phone.
- No server, contract or desk change. `WRITE_VERSION` and the V34 API are untouched.

## 3. Non-goals

- **The watch as its own sync device** (a credential, the vault key, direct sync over Wi-Fi or
  cellular). The backend allows one phone per vault (a second pairing revokes the first, V34 API
  §6.2), and the watch's background networking is rationed anyway. This is the obvious follow-up
  for cellular watches left without their phone (§14 D1).
- **Keeping the audio.** Captures are text only (§14 D4).
- **Editing.** No line edits, no journal, no backlog moves, no inbox triage, no Ask, no rituals.
- **Scheduling alerts on the watch.** The phone's alerts are mirrored (§10). A watch schedule would
  ring twice.
- **Interactive complications.** A watch complication is a single tap target that opens the app,
  so ticking happens in the day screen.
- **A watch-only install.** The app ships embedded in the iPhone app and needs it.
- **Pictures, links and clips.**

## 4. What watchOS allows

Checked against Apple's API availability on 2026-10-28.

| Need | watchOS | Consequence |
| --- | --- | --- |
| Speech-to-text in our own UI | No. `SFSpeechRecognizer` and `SpeechAnalyzer` are iOS, macOS and visionOS only. | System dictation. `presentTextInputController(withSuggestions: nil, allowedInputMode: .plain)` goes straight to the mic with no picker. The listening screen is Apple's. |
| Opening the app with the mic on | Yes. A complication opens its `widgetURL`, and the app can present dictation on appear. | `thock://capture?voice=1` (§8.1). |
| Large complication | Yes. `accessoryRectangular` (watchOS 9+), also used by the Smart Stack. | The timeline (§7.1). It must read in the tinted and monochrome face modes. |
| Small complications | Yes. `accessoryCircular`, `accessoryCorner`, `accessoryInline`. | Capture (circular, corner) and a one-line summary (inline). |
| A moving "now" | Timeline reloads are rationed (about 50 a day). Entries written ahead are free. | One entry every 15 minutes to midnight (§7.3). |
| Fresh data from the phone | `transferCurrentComplicationUserInfo` wakes the watch, about 50 a day. `updateApplicationContext` is unlimited and latest-wins. | Context for every change, the complication transfer only when today's rows change (§6.2). |
| Controls, Action button | `ControlWidget` on watchOS 26. | An optional Capture control, behind `#available(watchOS 26, *)`. |
| Mirrored notifications | Yes, while the phone is locked, with their actions. | No watch schedule (§10). |
| Date and time picker | SwiftUI `DatePicker` with date and hour-and-minute (watchOS 10+). | *Remind me* (§8.3). |
| The data layer | ThockKit imports Foundation, CryptoKit, SQLite3, ImageIO, CoreGraphics and UniformTypeIdentifiers, all available. | ThockKit gains `.watchOS(.v11)` (§11). |

The minimum is **watchOS 11**, the release that pairs with iOS 18, which is ThockKit's floor.

## 5. Architecture: the watch is a satellite of the phone

```
desk ⇄ Thock Plus ⇄ iPhone (ThockKit store, SyncEngine, ReminderNotifier, WatchLink) ⇄ Watch
                                                    snapshot ↓    ↑ capture, tick, day request
```

- **The phone reads.** `WatchLink` builds a `WatchSnapshot` (§6.1) from the phone's
  `VaultSession`: the planner, calendar lines, the day's reminders and mirror lines, all already
  resolved by ThockKit's readers. The watch gets rows with a time, a label, a kind and a state. It
  never sees Markdown, the vault config, templates or the reminder index.
- **The phone writes.** A watch request is decoded on the phone and run through the call a phone
  screen makes: `VaultSession.capture` for a capture (the same `PhoneWrites.capture`), and
  `VaultSession.tick(hash:ordinal:day:)` for a tick, the call `TickLineIntent` makes. The append
  contract, heading resolution, digests, reminder ids and the immediate alert are the phone's by
  construction, not a second implementation.
- **The watch keeps two files** in its own app group container, shared by the watch app and its
  widget extension: `snapshot.json` (the last snapshot received) and `outbox.json` (requests not
  yet acked, plus the capture draft). Both are JSON written atomically. Neither is a store of
  notes: deleting the watch app loses only unsent requests, and the outbox screen lists those.
- **The vault key and the `tpp_` credential stay in the phone's keychain.** The snapshot is note
  text at rest on the watch, protected by the watch's passcode data protection.

## 6. The watch link

A private contract between the two apps, in `ThockKit/Watch/`, so both targets compile the same
types. It never crosses the network, so it is not part of V34. Every message carries `v: 1`. A
receiver that does not know a message's `v` ignores it and logs it. The watch then shows *Update
Thock on your iPhone* instead of retrying forever.

### 6.1 Messages

```
// phone → watch
WatchSnapshot {
  v, made_at,                              // ISO 8601 instant
  state: "ready" | "not_connected" | "read_only",
  today: "2026-10-28",                     // the phone's VaultSession.today(), not the watch's clock
  days: [WatchDay],                        // today − 7 … today + 7
  reminders: { date_only_at: "09:00", tomorrow_at: "09:00", index: "reminders.md" },
  chip: "today"                            // the phone's remembered chip for the watch entry
}
WatchDay {
  day: "2026-10-28", exists: true,         // false: no note yet, rows are reminders only
  rows: [WatchRow], notes: 2               // non-checkbox lines in the planner, shown as a count
}
WatchRow {
  hash, ordinal,                           // the line's identity for a tick
  kind: "task" | "calendar" | "reminder",
  start: 840, end: 900,                    // minutes after midnight; null when untimed
  label: "Dentist",                        // Inline.plainText, tokens and ids already dropped
  done: false,
  carried_from: "2026-10-27"               // a reminder still open from an earlier day, else null
}

// watch → phone
WatchCapture { v, id: UUID, at, text, destination: "today"|"inbox"|"backlog"|"reminder",
               due: "2026-10-29 09:00" | "2026-10-29" | null }   // ReminderDue.description
WatchTick    { v, id: UUID, day, hash, ordinal }
WatchDayAsk  { v, id: UUID, day }                                   // outside the snapshot's range

// phone → watch
WatchAck     { id, outcome: "saved" | "refused", message }          // message only when refused
WatchDayReply{ id, day: WatchDay }
```

`WatchRow`s come from the same reading the phone's planner card draws, in planner order, then
reminders from the day's `### Reminders` group (V45 §7.1) and, for a day with no note, from the
reminder index. A row is `reminder` when it carries a `LineReminder` or is a mirror line. Calendar
lines are `calendar` and are never ticked, as on the phone.

### 6.2 Transports

- **Snapshot:** `updateApplicationContext` after every store change that changes the encoded
  snapshot. It hooks in where `AppModel.storeChanged()` already calls `scheduleWidgetReload()`, and
  shares its 1.5-second coalescing, so a first pull is one send, not hundreds. When today's
  `WatchDay` differs from the last one sent and `remainingComplicationUserInfoTransfers > 0`, the
  phone also sends it with `transferCurrentComplicationUserInfo`, which wakes the watch's widget
  to reload. A snapshot is about 10 KB, well inside the context limit.
- **Requests:** `sendMessage` when `isReachable`. This launches a suspended or terminated iPhone
  app in the background, and the reply carries the ack. On no reply within ten seconds, or when
  not reachable, the request goes by `transferUserInfo`, which the system queues across restarts
  of either app. The ack then comes back by `transferUserInfo` too.
- **Activation:** the phone activates `WCSession` in `application(_:didFinishLaunchingWithOptions:)`,
  beside the notification delegate, so a request that launched the app in the background is
  received. The watch activates it in its app delegate.

### 6.3 Exactly once

Both transports can carry the same request: a `sendMessage` whose reply was lost is resent by
`transferUserInfo`. The phone keeps handled request ids in a `watch_requests` table in its store
(id, outcome, message, handled_at), pruned after 30 days. A repeat id is acked with the recorded
outcome and writes nothing. The capture is made with `now` set to the watch's `at`, not the phone's clock, so its digest
(`captureDigest`) and record are the ones a single delivery would make.

### 6.4 On the phone

`WatchLink` hands each request to `AppModel.receive(_ request:)`. That method follows
`AppModel.answer(_:reminderID:due:)`, the background path a notification answer already takes:

1. `await boot()`. Give up with `refused` "Open Thock on your iPhone" if the phase is not `.ready`.
2. Run the write on the session directly rather than through `perform`, so the error becomes the
   ack's message instead of a toast nobody sees:
   - `VaultSessionError.readOnly` → *Thock Plus has ended. Captures are paused.*
   - `VaultSessionError.lineChanged` (a tick whose line is gone or changed) → *That line changed.
     Look again.*
   - anything else → *That didn't save. Try again.*, with the error logged.
3. Record the outcome under the request id, then reply.
4. For a reminder capture, `refreshAlerts()` at once, so it rings even if the round never finishes.
5. Sync under a background task, and stop the engine afterwards if the app is in the background.
   This tail is shared with `answer` by factoring it into one helper.

A capture from the watch remembers its chip under the watch's own entry (`chip.watch`), so the
phone's chips are unaffected.

## 7. Complications

One widget extension, `ThockWatchWidgets`, with two widgets.

### 7.1 Today (`accessoryRectangular`, `accessoryInline`)

- **Rectangular:** a header with the day (*WED 28*, in the accent) and the next timed item to its
  right (*14:00 Dentist*), or *3 left* when nothing timed remains. Under the header, *3 left ·
  2 anytime* in muted text. Then a track spanning five hours from the half hour before now, with
  hour labels under it:
  - tasks are filled blocks from `start` to `end`, and 30 minutes when untimed at the end;
  - calendar events are outlined blocks in the calendar colour;
  - reminders are a tick above the track and a line through it;
  - done items fade to a quarter of their strength;
  - "now" is an accent line with a dot.
- **Tinted and monochrome faces:** every distinction above is also made by shape or opacity, not
  only colour, and the accent parts are `widgetAccentable()`.
- **Inline:** *3 left · 14:00 Dentist*.
- **Tap:** `thock://day/today`.
- **Wrist down:** labels are `privacySensitive()`, as in the iPhone widget, so a locked watch shows
  blocks without words.
- **Empty states:** *Open Thock on your iPhone* (never received a snapshot or `not_connected`),
  *Nothing planned today* (no rows), and the header alone for a day with only done items.

### 7.2 Capture (`accessoryCircular`, `accessoryCorner`)

The Thock mic glyph in the accent, with a curved *CAPTURE* label in the corner. It shows no data,
so it never goes stale and spends no refreshes. Tap: `thock://capture?voice=1`.

### 7.3 The timeline provider

- `entries(rows:today:now:)` is a pure function in ThockKit: one entry every 15 minutes from now
  to midnight (at most 96), each with its window start, its "next" row and its counts. The window
  moves without a reload.
- The policy is `.after(midnight)`. The snapshot already holds tomorrow, so the new day draws
  without the phone.
- The widget reloads when the watch app writes a new snapshot (`reloadTimelines(ofKind:)`) and
  when a complication transfer arrives.
- A snapshot older than its `today` (the phone has been away over midnight) still draws: the
  provider picks the `WatchDay` for the watch's date when the snapshot holds it.

### 7.4 Capture control (watchOS 26)

A `ControlWidget` button that opens `thock://capture?voice=1`, for Control Center, the Smart
Stack and the Action button on Apple Watch Ultra. It is compiled behind
`#available(watchOS 26, *)`, and nothing else depends on it.

## 8. Capture

### 8.1 Listening

Capture opens from the complication, the control, or the mic button on the app's day screen.
Arriving with `voice=1`, the app presents system dictation as soon as its root view appears
(`presentTextInputController(withSuggestions: nil, allowedInputMode: .plain)`). The completion
gives the text, or nothing when the user cancelled:

- **Cancelled:** back to where the app was, and nothing is written. Arriving from a complication,
  that is the day screen.
- **Text:** the confirm screen.

Scribble and the keyboard are offered by the same system sheet, so a quiet place still works.
When the state is `not_connected` or `read_only` (§6.1), capture does not open dictation. It shows
*Open Thock on your iPhone to connect* or *Thock Plus has ended. Captures are paused.*

### 8.2 Confirm

- The words, in a tappable card that dictates again and replaces the text.
- The four chips in a two-by-two grid: *Today*, *Inbox*, *Backlog*, *Reminder*. The chip starts on
  the watch's remembered choice (`chip.watch`, also carried in the snapshot), and *Reminder* is
  never remembered (V45 §10.2).
- The phone's hint under the chips, word for word: *One line joins today's plan.* (a watch capture
  is always one paragraph), *Lands in your inbox. Triage sorts it at the desk.*, *Goes under Soon
  in your backlog.*, and for a reminder ThockKit's `ringsSentence` and *waits in reminders.md*,
  using the snapshot's settings.
- **Save:** a full-width button. Dismissing the screen without saving keeps the draft (§8.5).

### 8.3 Remind me

Choosing *Reminder* opens *Remind me*, with the phone's presets from the snapshot's settings:
*Later today 17:00* (only before 16:00), *Tomorrow* at `tomorrow_at`, *Next week Mon* at
`tomorrow_at`, and *Date and time…*. The last one is a date picker, then an hour-and-minute picker
in 24-hour time, with *No time* making a date-only reminder. The chip then reads its time
(`chipText`, *⏰ Thu 09:00*). Closing *Remind me* without a time returns the chip to where it was.
The spoken text is never read for dates.

### 8.4 Saved

Saving writes the request to the outbox, then sends it (§6.2). The confirmation reads the phone's
toast (*Saved to your inbox*, *Added to today*, *Added to Backlog · Soon*, or the reminder's
`ringsSentence`), with *Sending to your iPhone* under it, which becomes *On your iPhone* on the
ack. After two seconds the app returns to where capture was opened from.

**The outbox** is a row at the bottom of the day screen when anything is waiting: *2 waiting for
your iPhone ›*. It lists each request with its text and state. A refused request shows the phone's
message with *Try again* and *Delete*. Nothing is deleted without that tap.

### 8.5 The draft

While the confirm or *Remind me* screen is open, the text, chip and due are kept in `outbox.json`
as the draft. When the user lowers their wrist and comes back (by the watch's own *Return to app*
or by opening Thock), the confirm screen is restored with the draft. Saving or cancelling clears
it. A dictation that returned no text leaves no draft. This is V46 §5.3's rule on the phone: what
the user said is never lost and is written nowhere until saved.

## 9. Day screen

- **Title:** the day in Petrona (*Wed 28*) with *Today*, *Yesterday*, *Tomorrow* or the weekday
  under it. The chevrons on either side, a horizontal swipe, and `left`/`right` on a keyboard all
  move one day. A *Today* button appears on any other day.
- **Rows:** timed rows in order (the time in mono, then the label), calendar rows with the calendar
  edge, reminders with a bell and *from Tue* when carried. Then *Anytime* with the untimed rows.
  Done rows are struck through. A planner with notes ends with *+2 notes* in muted text.
- **Ticking:** a tap on a task's or reminder's box sends a `WatchTick` and draws the row done at
  once. A `refused` ack puts it back with the phone's message. Untick is not offered, as on the
  iPhone widget. A tick on a reminder's home line closes the reminder, which is what a tick does on
  the phone.
- **Range:** the snapshot covers seven days back and seven ahead. Past that, the watch sends a
  `WatchDayAsk` when the phone is reachable and shows *Open Thock on your iPhone to see this day*
  when it is not.
- **No note:** *Nothing planned*, above any reminders for that day. A vault with no planner heading
  or without the Timeline routine is the same empty state, never an error.
- **Freshness:** *As of 13:04* under the title when the snapshot is more than ten minutes old.
- **The mic:** a button at the bottom of the list starts capture with dictation (§8.1).
- **Opening:** `thock://day/today` from the timeline, and the app's last screen otherwise.

## 10. Reminders on the wrist

The watch schedules nothing. While the phone is locked and the watch is worn, the phone's local
alerts (V45 §10.5) appear on the watch with the `thock.reminder` category's actions. A background
action (*Done*, *In 1 hour*, *Tomorrow 09:00*) is handed to the phone's notification delegate, and
`AppModel.answer` runs as it does from the lock screen. The watch app's notification scene uses the
system long look; there is no custom interface in V48.

The phone's snapshot follows its own reminder changes, so a reminder answered from the wrist drops
off the day screen and the timeline after the next send.

**Needs a real-device check** (§12): that a mirrored alert's background actions reach the phone's
delegate when a companion watch app is installed, rather than being offered to the watch app.

## 11. Changes by file

| File | Change |
| --- | --- |
| `thock/ios/ThockKit/Package.swift` | Add `.watchOS(.v11)` to `platforms`. |
| `ThockKit/Sources/ThockKit/Watch/WatchMessages.swift` | New. The §6.1 types, `Codable`, and their `[String: Any]` dictionary form for WatchConnectivity. |
| `ThockKit/Sources/ThockKit/Watch/WatchSnapshot.swift` | New. `VaultSession.watchSnapshot(days:now:)`, the builder from the phone's readings, and `TimelineEntries.entries(rows:today:now:)` (§7.3). |
| `ThockKit/Sources/ThockKit/Store/Database.swift` | The `watch_requests` table (§6.3), with prune. |
| `ThockKit/Sources/ThockKit/Vault/VaultSession.swift` | `tick(hash:ordinal:day:)` returns silently today when the line is gone. It gains `VaultSessionError.lineChanged` for that case, which `TickLineIntent` ignores as before and the watch reports (§6.4). A watch capture calls the existing `capture(blocks:destination:due:now:)` with `Blocks.parse(text)` and `now` set to the request's `at`. |
| `thock/ios/Thock/WatchLink.swift` | New. The `WCSessionDelegate`: activation, coalesced snapshot sends, the complication budget, request decoding, replies and acks by `transferUserInfo`. |
| `thock/ios/Thock/AppModel.swift` | `receive(_:)` (§6.4), the snapshot send beside `scheduleWidgetReload()`, and the background tail shared with `answer`. |
| `thock/ios/Thock/ThockApp.swift` | Activate `WatchLink` in `didFinishLaunching`. |
| `thock/ios/ThockWatch/` | New watchOS app target: `ThockWatchApp.swift`, `WatchModel.swift` (snapshot, outbox, draft), `CaptureScreen.swift`, `RemindMeScreen.swift`, `DayScreen.swift`, `OutboxScreen.swift`, `WatchTheme.swift`. `Shared/Theme.swift` uses UIKit's dynamic colours, which watchOS lacks, so the watch has a dark-only palette from the phone's dark tokens, with Petrona for dates. |
| `thock/ios/ThockWatchWidgets/` | New widget extension: Today (rectangular, inline), Capture (circular, corner), the watchOS 26 Capture control. |
| `thock/ios/Config/` | `ThockWatch-Info.plist`, `ThockWatchWidgets-Info.plist`, and an entitlements file with the watch app group `group.com.thethock.ios.watch`. |
| `thock/ios/Thock.xcodeproj` | The two targets, embedded in the iOS app (*Embed Watch Content*). Bundle ids `com.thethock.ios.watchkitapp` and `com.thethock.ios.watchkitapp.widgets`. |
| `thock/ios/script/smoke` | `-thock-watch-script` (§12). |
| `thock/ios/README.md` | The new targets in the layout table, and how to run the watch scheme. |

No inherited crate, desk or service change.

## 12. Tests

- **ThockKit unit tests** (`swift test`):
  - Snapshot builder: planner groups become rows in planner order; calendar lines are `calendar`;
    a line with a token, and a mirror line, are `reminder` with tokens and ids gone from the
    label; a carried reminder has `carried_from`; a day with no note has `exists: false` and its
    reminders from the index; a note with no planner heading has no rows and no error; a
    read-only session yields `read_only`.
  - Timeline entries: entries every 15 minutes to midnight; the window starts on the half hour
    before now; "next" skips done rows and calendar rows already started; an empty day; now
    after the last item.
  - **A watch capture is a phone capture:** for each destination, and for a reminder with and
    without a time, the `PlannedWrite`s from a `WatchCapture` equal those from `capture(blocks:…)`
    with the same text, due and instant.
  - Exactly once: the same capture id handled twice writes once and acks twice with the same
    outcome; the table prunes after 30 days.
  - A tick on a line the desk changed is refused with its message; a tick on a calendar row is
    refused.
  - Message round-trips through the dictionary form for every type; an unknown `v` is rejected.
- **Smoke** (`script/smoke`, on palloma): `-thock-watch-script
  "capture:<destination>:<text>;tick:<label prefix>"` feeds requests through `WatchLink`'s
  decoder into `AppModel.receive`, and the run asserts they reach the phone's store and the
  practice desk like the existing `capture:` steps, plus a resend of the same id writing nothing.
  The watch scheme is built for a generic watchOS simulator in the same run.
- **By hand, once, on a real phone and watch** (the simulator does not cover these well): a capture
  with the iPhone app terminated; a capture in airplane mode, delivered on reconnect; the timeline
  updating after a desk edit; a mirrored reminder's *Done* answered on the wrist; the face in
  tinted mode.

## 13. Build order

1. ThockKit: the platform, the messages, the snapshot builder, the timeline entries and the
   `watch_requests` table, with their tests. This ships on its own and changes nothing on the
   phone.
2. The phone: `WatchLink`, `AppModel.receive`, the snapshot send, and the smoke script. Testable
   before a watch app exists.
3. The watch app: the day screen from a snapshot, then capture, *Remind me*, the outbox and the
   draft.
4. The complications, then the watchOS 26 control.
5. Signing on palloma: the two App IDs and the watch app group. The next TestFlight build carries
   the watch app inside the iPhone app.

## 14. Decisions (2026-10-28)

1. **D1 — Satellite, not a device.** The watch reaches the vault through the phone over
   WatchConnectivity. A device of its own needs a third role in V34, a key handover and coordinated
   reminder schedules, and it pays off only for cellular watches without their phone. It is a
   possible follow-up.
2. **D2 — The day screen ticks.** A tick is one more request type that reuses the widget's
   `TickLineIntent` path. A read-only list would leave the wrist unable to finish the thing it
   just reminded you of.
3. **D3 — Save goes through the confirm screen.** Dictate, *Done*, check the words and the chip,
   *Save*. Saving straight from dictation would be one tap faster, but it makes misfiling easy and
   puts the correction on a receipt.
4. **D4 — No audio in V48.** Text only. Keeping the clip (attached through V39's `put_file`, so
   the desk could transcribe again) waits until dictation proves to mishear often enough to matter.
5. **D5 — System dictation.** watchOS has no speech framework, so the listening screen is Apple's.
   Recording on the watch and transcribing on the phone would allow our own screen, but the text
   would not be visible until the phone answered, and nothing would work without the phone.
6. **D6 — The phone reads, the watch draws.** A snapshot of resolved rows, not Markdown, so the
   planner grammar, heading resolution and the reminder index have one reader on the phone.
7. **D7 — No alerts scheduled on the watch.** The phone's alerts are mirrored with their actions,
   and a second schedule would ring twice.
