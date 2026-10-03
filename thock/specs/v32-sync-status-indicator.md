# Thock V32 — One sync indicator: status out of the way until it needs you

**Status:** Shipped (2026-10-01)
**Owner:** Diego · **Date:** 2026-10-01
**Companion docs:** `v8-calendar-sync.md` §10.3 (the status-row grammar this keeps),
`v13-inbox-routine.md` §10.4 (the Inbox row and its triage entry point),
`v15-unified-gmail-sync.md` §7.3, `v26-planner-heading-resolution.md` §7.3 (the Calendar hold
buttons), `v31-readwise-sync.md` §8.5 (the Readwise row)

---

## 1. Summary

Every connector reports its health as a row at the top of a panel: Calendar in the Day Planner;
Gmail, Inbox, and Readwise in the Backlog. With all four connected, the Backlog spends three rows
(about a fifth of its height) saying "everything is fine" before showing a single task, and each
new connector makes it worse.

V32 moves routine status into **one icon in the bottom status bar**. Clicking it, or running
`thock: show sync status`, opens a popover with one row per connector and that row's action
(Sync now, Retry, Reconnect, …). The panels keep a row **only when it needs the user**: a
failure, a lost sign-in, a hold the user can fix, or Inbox items waiting. While anything needs
attention, the icon carries a dot, so nothing broken is hidden behind a click.

```
Healthy                              Something needs you
┌────────────────────────────┐       Backlog panel
│ Calendar   synced 2m ago    ⟳│       ┌────────────────────────────────┐
│ Gmail      checked just now ⟳│       │ Readwise · token rejected  Reconnect │
│ Inbox      empty            ⟳│       ├────────────────────────────────┤
│ Readwise   synced 35m ago   ⟳│       │ Soon 20 │ Someday 19 │ Completed │
└────────────────────────────┘
   status bar:  … ⟳ ▣ ✦ ☰               status bar:  … ⟳• ▣ ✦ ☰
```

## 2. Goals & success criteria

- With every connector healthy, the Backlog and Day Planner panels show no status rows at all.
- Anything the user must act on is still visible without opening anything: an inline row in its
  panel, plus the dot on the icon.
- Every row that exists today keeps its action, in the popover and (when actionable) inline.
  Nothing becomes reachable only by mouse.
- Adding a connector means adding one entry to a list, not a third copy of a row renderer.
- The upstream footprint is one status-bar registration in `crates/zed/src/zed.rs` and keymap
  entries.

## 3. Non-goals

- Changing what any service does, how often it polls, or its states. `SyncState` and `HoldReason`
  are untouched.
- Toasts or system notifications for state changes.
- A connections or settings screen (connect/disconnect management). V31 §14 #1's question stays
  open; this spec only consolidates *status*.
- Status text in the status bar itself. The bar gets an icon, not words.

## 4. Core concepts

### 4.1 One status model, rendered twice

Today four near-identical `render_*_status_row` functions build their own labels and buttons.
V32 adds one pure description per connector:

```rust
pub struct ConnectorStatus {
    pub connector: Connector,          // Calendar | Gmail | Inbox | Readwise, in that order
    pub summary: SharedString,         // "synced 2m ago", "token rejected", "3 waiting"
    pub detail: Option<SharedString>,  // tooltip: the error text, the hold detail
    pub attention: Attention,          // None | Waiting | Warning | Error
    pub actions: Vec<ConnectorAction>, // first one is primary
}
```

`ConnectorAction` names an existing action (`SyncCalendarNow`, `SyncGmailNow`, `SyncInboxNow`,
`SyncReadwiseNow`, `ConnectGoogleWorkspace`, `ConnectReadwise`, `AddPlannerHeading`,
`ChoosePlannerHeading`, triage) with its button label. Both the popover and the inline panel
rows render from this one value, through one `render_connector_row` in the new module. The
summary strings are today's strings with the `Name · ` prefix removed (the popover has a name
column; inline rows add the prefix back).

### 4.2 Attention decides where a row shows

| State | Attention | Inline in panel | Icon dot |
| --- | --- | --- | --- |
| `NoConfig` | — | hidden everywhere | — |
| `Synced`, `Idle`, `Connecting` | None | no | no |
| Inbox `Synced` with items waiting | Waiting | yes (`Inbox · 3 waiting — triage`) | accent |
| `Holding` with nothing to fix (Calendar `HoldReason::Waiting`: no note today, no calendars chosen) | None | no | no |
| `Holding` the user can fix (Calendar `NoPlannerHeading` / `PlannerHeadingTooDeep`; Gmail/Inbox label or list not found) | Warning | yes | warning |
| `NeverConnected`, Readwise config error | Warning | yes | warning |
| `Failing`, `Disconnected` | Error | yes | error |

Gmail and Inbox holds are always fixable (each names a missing label or list), so they're
Warning. Calendar's `Waiting` holds clear on their own.

The dot takes the most severe attention across connectors. The inline rows keep their current
homes: Calendar in the Day Planner; Gmail, Inbox, and Readwise in the Backlog.

## 5. What the user sees

- **Status bar.** One icon at the right, before the dock buttons. It shows a spinner while any
  connector is `Connecting` or mid-import, and a dot coloured by the worst attention otherwise.
  The tooltip is one line: "All synced", or the first connector needing attention ("Readwise ·
  token rejected"). It's hidden when no connector has a config, so a fresh vault or a non-vault
  window shows nothing.
- **Popover.** Opens upward from the icon. One row per configured connector, in fixed order
  (Calendar, Gmail, Inbox, Readwise), so rows don't jump around. Each row has the name, the
  summary (with `detail` as a tooltip), and its primary action as a small button. Rows needing
  attention are tinted with their attention colour. The Google connect button shows once: when
  Gmail and Inbox are both `NeverConnected`, Inbox's row says "via Google Workspace" with no
  button (today's dedupe rule, kept).
- **Panels.** Unchanged layout. A status row appears at the top only for Warning, Error, or
  Waiting states, rendered by the shared row function.

## 6. Keyboard

The popover is a focusable view with key context `ThockSyncStatus` and `"menu"` added, so the
default bindings apply:

- `thock::ToggleSyncStatus` (palette: "Show sync status") opens the popover focused on its first
  row, or closes it. Running it twice returns focus to where it was.
- `up`/`down` (and `j`/`k` in vim mode) move between rows. `enter` runs the row's primary action.
  `right`/`left` (`l`/`h`) move between a row's actions when it has more than one (Calendar's
  Add heading / Use another…).
- `escape` closes and returns focus to the editor.
- The row actions are the existing named actions, so each stays bindable and palette-reachable.
- The inline panel rows join their panel's existing selection model, as V13 §10.4 already
  requires.

A default chord for `ToggleSyncStatus` goes in both `default-{macos,linux}.json` Thock blocks,
chosen at implementation after a conflict check (§10 #1).

## 7. Architecture

```
sync_status.rs   ConnectorStatus/Attention/ConnectorAction, one pure `status_for_*` per service,
                 render_connector_row, SyncStatusIndicator (StatusItemView), SyncStatusPopover
```

- **`status_for_*`** are pure functions of `(&SyncState, service extras)`. The extras are Inbox
  queue depth, Readwise `importing_library` / `last_landed` / `config_error`, and Gmail/Inbox
  "the other one already offers connect". They replace the match arms in today's four row
  functions.
- **`SyncStatusIndicator`** implements `StatusItemView`. It finds the active workspace's project,
  looks up each service through the existing `service_for_project` functions, and `observe`s
  them, so it re-renders on any state change. Services can appear after the indicator is built
  (vault opened later), so it re-resolves on project worktree events, the same trigger the
  services use.
- **`SyncStatusPopover`** is the content of a `PopoverMenu` anchored to the icon,
  holding a `FocusHandle` and a selection index over the visible rows. Selection is by
  `Connector`, not index, so a row appearing or disappearing while it's open doesn't move the
  cursor to a different connector.
- **Relative times.** "synced 2m ago" goes stale between service notifications. While the popover
  is open, a stored timer task re-renders it every 30 s and is dropped on close. Inline rows only
  show attention states, which carry no relative time.
- **Panels.** `backlog_panel.rs` and `day_planner_panel.rs` each replace their status-row
  functions with: build the `ConnectorStatus`, render it inline when `attention != None`. That's
  roughly 300 lines deleted for about 20 added.

### 7.1 Upstream touch

`crates/zed/src/zed.rs`, in the status-bar block that already registers the right items:

```rust
let thock_sync_status = cx.new(|cx| thock::SyncStatusIndicator::new(workspace, window, cx));
status_bar.add_right_item(thock_sync_status, window, cx);
```

Two lines, mechanical, re-applied trivially after a rebase. Plus the keymap entries (§6).

## 8. Tests

- Pure: every `SyncState` × connector → expected `ConnectorStatus` (summary text, attention,
  actions), including the Inbox-waiting and Gmail/Inbox connect-dedupe cases, and Calendar's
  fixable vs. waiting holds. These replace the implicit coverage of the deleted row functions.
- Worst-attention fold across connectors.
- GPUI: the indicator hides with no configs; it shows a dot when one service fails and clears it
  on recovery; the popover's `enter` dispatches the row's primary action; selection survives a
  row appearing above the cursor; `escape` restores focus.
- Panels: a healthy service renders no inline row; a failing one renders exactly one.

## 9. Docs

- `ROUTINES.md` / skill docs that tell the user to "look at the status row in the Backlog panel"
  (`connect-google-workspace.md`, `connect-readwise.md`, the Inbox doc) change to "the sync icon
  in the bottom bar; problems also show in the panel".
- VISION §12 gets an entry when this ships.

## 10. Open questions

1. **Default chord** for `thock::ToggleSyncStatus`. _Resolved:_ `ctrl-alt-o` (Linux) /
   `cmd-alt-o` (macOS), free in both keymaps and under vim mode, next to the other panel toggles.
2. **Icon glyph.** _Resolved:_ `ArrowCircle`, which also spins while a connector is in flight.
   Revisit with a screenshot if a cloud-style glyph lands in `assets/icons/`.

## 11. Decision log (2026-10-01)

1. **Status bar over the editor toolbar.** One per window, visible with no note open, and where
   sync indicators conventionally live; the toolbar would repeat per pane.
2. **Inline only when action is needed.** Healthy status moves out of the panels; anything the
   user must act on stays visible without a click, and the icon's dot points at it.
3. **All four connectors**, Calendar included, so status lives in one place.
4. **One status model.** The consolidation also removes four hand-rolled copies of the same row
   grammar.
