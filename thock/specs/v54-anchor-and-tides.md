# Thock V54 - Anchor and Tides: a page that stays put

**Status:** Approved (2026-11-02); implementing
**Owner:** Diego · **Date:** 2026-11-02
**Design reference:** the small-screen desk study, round 2, ideas i (Anchor) and ii (Tides), with
to-scale mocks on a 14″ MacBook and a 24″ monitor: <https://claude.ai/artifact/2inkiuRBTfCHt6KiBAHRR3>
**Depends on:** `v43-almanac.md` (the Desk in the right dock, the body font, `soft_wrap:
editor_width` for notes), `v44-passing-notes.md` (the thread and its room, §5.3)
**Companion docs:** a separate spec for per-screen text size (decided in the same study). Anchor
measures the page in characters so the two compose without either knowing about the other.

---

## 1. Summary

On a 14″ MacBook (1512 × 982 pt), the desk spends most of its width on chrome. Measured from a
screenshot with the Desk open and the agent's composer up: rail 17%, note text 22%, an empty strip
held for a thread with no messages 27%, Desk 29%. The note wrapped at about 30 characters. People
keep the left rail open even when they don't use it, because it pushes the page toward the
centre of the screen.

V54 changes two rules:

- **Anchor.** The page is a column of a fixed number of characters, pinned to the centre of the
  *window*, not to the space between the docks. Whatever is left on each side is margin, and the
  docks and the thread live in it. Opening or closing a panel changes the margin, never the page.
- **Tides.** While you type in a note, side panels *ebb*: they shrink to a 6px tide mark at the
  window's edge. They flow back when you pause, when you go to a panel from the keyboard, or when
  you ask. Because of Anchor, neither ebb nor flow moves a single word on the page.

Together they make the desk focus on the page while writing and show everything when you look up,
without modes or setup. They also replace V44's room cascade with one placement rule.

## 2. Goals & success criteria

- **G1: The page doesn't move.** With one pane in the centre, opening, closing, ebbing or resizing a
  dock leaves every glyph of the note where it was. Only an agent thread with messages may push
  the page, and only when the margin can't hold it (§4.3).
- **G2: Centred on the screen.** The page's centre is the window's centre whenever the margins
  allow it, with or without the rail. Keeping a panel open as a counterweight is no longer needed.
- **G3: Measured in characters.** The page's width is a character count of the body font at the
  current size. A larger font gives a wider page, not fewer characters per line, and the decision
  to tuck panels away is made in characters too.
- **G4: One thing at a time while writing.** Typing in a note tucks away the side panels the person
  allowed to ebb. Nothing else does.
- **G5: Yours to arrange.** Each side is `ebb` or `stay`, and docks keep their usual open, closed
  and left/right choices. Tides can be turned off entirely.
- **G6: Keyboard-complete.** Every way to bring a panel back has a key. Reaching a panel with its
  existing focus action brings it back and focuses it in one step.
- **G7: An empty thread takes no room.** The composer floats over the page until there's a message
  to show (round 1 of the study).

## 3. Non-goals

- **Per-screen text size.** Decided, but in its own spec. V54 reads the body font size wherever it
  comes from.
- **Animation.** Ebb and flow change a dock's width instantly. With the page anchored, nothing
  under your eyes jumps, so motion is polish for later if the change feels abrupt.
- **Splits.** With more than one pane in the centre, the layout is Zed's ordinary one and Tides is
  inactive (§5.6).
- **The bottom dock.** Thock puts no panel there (V43). It is never anchored around and never ebbs.
- **Marginalia, Island and Day phases** from the study. They can build on Anchor later.

## 4. Anchor

### 4.1 The page

The page is the centre pane: its tab bar, breadcrumb and editor together. Its width is

```
column = measure × advance("0") + editor padding
```

with `advance("0")` taken from the body font (V43: Plex Sans, or the buffer font when `"body_font":
"buffer"`) at the current buffer font size. The `0` advance is the CSS `ch` unit and a stable
stand-in for an average character. `measure` defaults to **68**, `min_measure` to **48** (§6).

Soft wrap stays `editor_width` (V43 §7.1). The editor *is* the column, so it wraps at the measure.

For a non-note item in the centre (the Inbox view, settings JSON, an image) the column is the same
width (decision 13).

### 4.2 Placement

One pure function decides the whole row:

```rust
fn anchor(input: AnchorInput) -> AnchorLayout
// AnchorInput:  window width, left and right dock widths and policies, column,
//               min_column, thread room (Some only with messages)
// AnchorLayout: left padding, column, right padding, which docks ebb to fit,
//               whether the thread floats
```

`G` is a 24pt minimum gap between the column and anything beside it. Steps run in order until
the column fits:

1. **Centred.** Column at `(window - column) / 2`. It fits if it clears the open left dock by `G`
   and the open right dock plus the thread room by `G`.
2. **Ebb to fit.** Docks with policy `ebb` on a side that doesn't fit ebb (§5.2), right side first
   while a thread is open, otherwise the side that overflows more. This is the compact rule,
   measured in characters: on a window too narrow for the page and the panel, the panel waits at
   the edge until you call it.
3. **Shift.** The column moves toward the side with room, as little as needed.
4. **Narrow.** The column shrinks toward `min_column`.
5. **Float.** The thread floats over the page as in V44, and the column takes what's left, even
   below `min_column`.

A dock you bring back while step 2 is in force flows back *as a dock*. While it is open, steps 3
and 4 make room for it. When it ebbs again, the page returns to the centre. You're looking at the
panel at that moment, not the page, so the shift doesn't break G1's promise while writing.

### 4.3 The thread

V44's `room_for` cascade and its Desk collapse are replaced by §4.2:

- The thread's room is 408pt (V44 §5.3: the 360pt column plus its gaps), and it lives in the right
  margin.
- **It is requested only when the thread has at least one message to show.** With only the
  composer open, the composer and the pebble float in the bottom-right of the centre, as a
  Needs-you bubble does today.
- V44's "collapse the Desk" becomes step 2: the Desk *ebbs*, which is not persisted, and flows back
  when the thread folds.
- A right dock with policy `stay` never ebbs for the thread. The column shifts or narrows, and the
  thread floats as a last resort.

### 4.4 Worked numbers

These are approximate: Plex Sans at the shipped 17px gives a `0` of about 10px, so 68 characters
plus padding is about 740pt.

| Window | Panels | Result |
| --- | --- | --- |
| 1512 (14″ MacBook) | rail 255, Desk 320 | Margins ≈ 386 each. Both fit: centred. |
| 1512 | rail 255, Desk 440 (resized) | Desk overflows: it ebbs to fit (step 2) until called. |
| 1512 | rail, Desk, thread with messages | Desk ebbs; mark + thread + gap ≈ 438 > 386, so the column shifts left ≈ 52pt (step 3). |
| 1920 (24″ 1080p) | rail, Desk, thread | Margins ≈ 590. Desk ebbs for the thread; centred. |
| 1920 | Desk set to `stay`, thread | Desk 320 + thread 408 + gap ≈ 752 > 590: shift ≈ 162pt; the rail still clears, so no narrowing. |

Shift before narrow keeps the line length and costs the centre only while a thread with messages
is open (decision 10).

## 5. Tides

### 5.1 States

Each side (left, right) of a window with an anchored page is **flowing** or **ebbed**. Ebbed is a
render state of an open dock: the dock is still open and its panel entity, selection and scroll
position are untouched. It is not persisted, so after a restart every open dock starts flowing.

A closed dock (`cmd-b`, `cmd-r`, or a panel's toggle) is unaffected by Tides. Hiding stays what it
is today.

### 5.2 The tide mark

An ebbed dock renders as a 6pt strip at the window's edge in `panel.background`, the full height
of the dock. Clicking it flows that side back. A 2pt `text.accent` line at its inner edge appears
while the panel has something waiting: the Desk while a timed entry starts within 10 minutes or a
reminder is ringing. Panels opt in through a method on the Thock panel side with a default of
nothing waiting.

### 5.3 Ebb

A side ebbs when **text is typed into an editor in the centre pane** (the editor's
`EditorEvent::InputHandled`) while its policy is `ebb` and it is flowing. Only typed text counts:

- Vim normal-mode motions, scrolling, selecting and moving the caret don't ebb. Reading isn't
  writing.
- An edit made from a panel (ticking a task in the Desk, a ritual's writes, sync) doesn't ebb.
- Typing into a panel's own field (Backlog *Add*, the chat composer) doesn't ebb.

### 5.4 Flow

A side flows back when any of these happens:

1. **Focus enters its dock**, by any route: a panel's focus action (`cmd-2`, `cmd-alt-u`,
   `cmd-alt-p`, `cmd-alt-k`, `cmd-alt-t`), a click on the tide mark, or region navigation. One
   keystroke brings the panel back and focuses it.
2. **`thock::ToggleTide`** (§7): flows every ebbed side without moving focus. Pressed again while
   flowing, it ebbs every `ebb` side.
3. **A pause:** no typed text in the centre for `flow_back_after` seconds (default 8). Setting it
   to `null` turns this off, so panels come back only when asked.
4. **The page goes away:** the active centre item changes or the centre splits.

A dock ebbed to fit (§4.2 step 2) doesn't flow back on a pause; it would only cover the page.
Triggers 1, 2 and 4 bring it back.

### 5.5 Focus and escape

`escape` in a panel returns focus to the editor (CLAUDE.md "Panels"). The panel stays flowing
until you type. Focus never lands in an ebbed dock: an action that would focus an ebbed panel
flows it first (§5.4 rule 1).

### 5.6 When Tides is inactive

Tides is active only while the page is anchored: centred layout on, one pane in the centre, an
item open. With splits or centred layout off, every side is flowing and nothing ebbs. Ebbing
there would reflow the panes, which breaks G1.

## 6. Settings

In `settings.json` beside V43's font settings, because layout is a person's preference, not a
property of the vault:

```jsonc
"thock": {
  "page": {
    // Characters per line of the page, in the body font.
    "measure": 68,
    // The page narrows to this before an agent thread floats over it.
    "min_measure": 48
  },
  "tides": {
    // "ebb" tucks the side away while you type; "stay" keeps it open.
    "left": "ebb",
    "right": "ebb",
    // Seconds without typing before ebbed panels come back, or null for never.
    "flow_back_after": 8
  }
}
```

`"tides": false` turns Tides off. Anchor follows `workspace::ToggleCenteredLayout`: centred layout
in Thock *means* the anchored page. Zed's fractional `centered_layout.left_padding` /
`right_padding` settings are deleted (fork discipline: delete what Thock doesn't use).

## 7. Keys and actions

| Action | macOS | Linux | Palette text |
| --- | --- | --- | --- |
| `thock::ToggleTide` | `ctrl-cmd-\` | `ctrl-alt-\` | "Show or tuck away the side panels" |
| Existing panel focus actions | unchanged | unchanged | Also bring an ebbed panel back |
| `workspace::ToggleCenteredLayout` | unbound (unchanged) | unbound | "Keep the page centred" (Thock doc text) |

Both new bindings are free in `default-macos.json`, `default-linux.json` and `vim.json` today. No
pane key context changes, so `vim.json` needs nothing.

## 8. Architecture

### 8.1 Inherited crate: `workspace`

The placement has to run during the workspace's own render, with the docks' widths of that frame.
Computing it in Thock and pushing it in would lag by a frame and make the page jitter, which is
exactly what G1 forbids. So it belongs in `crates/workspace/src/workspace.rs`, extending the hooks
V44 added (§7.2 there):

1. **`set_center_anchor(Option<CenterAnchor>)`** replaces `set_center_inset`. `CenterAnchor { column,
   min_column, thread: Option<Pixels> }`. `render_center_group` calls `anchor()` with
   `window.viewport_size().width` and the docks' current widths, and renders absolute-width
   paddings in place of the fractional ones. The `center_overlay` slot is unchanged.
2. **Ebbed docks.** `Dock` gains a render-only `ebbed` flag, with `set_ebbed` and `is_ebbed`. An
   ebbed dock renders its tide mark instead of the panel, keeps `is_open`, isn't serialized, and
   reports its width as the mark's to `anchor()`. Docks ebbed to fit are decided inside `anchor()`
   on each render and never stored.
3. `adjust_padding` and the fractional settings go.

`anchor()` is a pure function in the workspace crate with its own table tests (§9). The PR calls
out the workspace edit and why it can't live in `crates/thock`.

### 8.2 `crates/thock`

A new `desk_layout.rs` owns both rules on the Thock side. It's one logical component:

- **Measure:** reads `thock.page`, the body font and the buffer font size, computes `column` and
  `min_column` through the text system, and calls `set_center_anchor`. It recomputes on settings
  and font-size changes, which is where per-screen text size will plug in.
- **Thread room:** the chat panel reports `thread_room: Option<Pixels>` (Some while a message is
  shown) instead of running `room_for` and collapsing the Desk. `room_for`, `centred_margins`,
  `collapsed_desk` and `MIN_NOTE_WIDTH` leave `chat_panel.rs`, along with their tests, which are
  rewritten against `anchor()`.
- **Tides:** subscribes to `InputHandled` on the active centre editor (re-subscribing on active-item
  change), keeps the pause timer as a stored task (replacing it is the intended cancel), handles
  `thock::ToggleTide`, and flows a side when focus enters its dock. Focus actions run while the
  workspace is being updated, so dock changes go through `cx.defer` (CLAUDE.md, GPUI panels).
- **Waiting signal:** a `has_waiting()` hook the Desk implements for the tide mark's accent line.

### 8.3 Order of work

1. `anchor()` and its tests; the workspace renders absolute paddings; fractions deleted.
2. Measure in `desk_layout.rs`; V43 notes look the same at 1920 and better at 1512.
3. Thread room through `anchor()`; V44 cascade removed; the composer floats with no messages.
4. Ebbed docks in the workspace; Tides in `desk_layout.rs`; `ToggleTide` and keymaps.
5. Tide mark's waiting line.

Each step ships on its own: Anchor without Tides is already the main fix.

## 9. Tests

- **`anchor()` table** (workspace): windows of 1280, 1512 and 1920; rail and Desk open, closed,
  `stay` and `ebb`; thread none, composer-only (no room) and with messages. Assert centred column,
  which docks ebb to fit, shift amount, narrowing to `min_column`, and float.
- **Page doesn't move** (thock, `VisualTestContext`): record the editor's text origin; toggle the
  right dock, ebb it, resize it; the origin is unchanged each time.
- **Ebb on typing:** `simulate_keystrokes` text into a note, and the right dock is ebbed while
  `is_open()` stays true. Vim normal-mode `j j k` doesn't ebb. Ticking a task from the Desk
  (`x` in `ThockBacklogPanel`) doesn't ebb.
- **Flow:** advance the executor timer past `flow_back_after`, and the dock flows. `null` never
  flows. `cmd-alt-u` while ebbed flows the right side, focuses the Backlog, and keeps its selection.
  `ctrl-cmd-\` flows both sides and leaves focus in the editor; pressed again, it ebbs them.
- **Policies:** a `stay` side never ebbs on typing or for the thread.
- **Thread:** composer-only requests no room; the first message requests 408; at 1512 the Desk ebbs
  and flows back when the thread folds.
- **Inactive:** with a split centre, typing doesn't ebb.
- **Not persisted:** serialize the workspace while ebbed and restore it; the dock is open and
  flowing.

Manual round, with Diego driving, on the MacBook and the 24″: write for a few minutes with
everything open, check the plan, talk to Thock, then move the window between screens.

## 10. Risks

- **Pause tuning.** Eight seconds of re-reading brings panels back while you're still with the
  page. If it's distracting, raise the default or make it `null`. The setting exists for that.
- **Tide marks as clutter.** Two 6pt strips may read as a rendering glitch. Fallback: draw them
  only on hover of the edge, with the keys as the main way back.
- **Workspace churn.** Replacing the centred-layout paddings touches four `BottomDockLayout` arms.
  V44 already merged them into one helper, which keeps the edit in one place.

## 11. Open questions

None.

## 12. Decision log (2026-11-02)

1. **Anchor plus Tides** chosen from five round-2 ideas; Marginalia stays a candidate for later.
2. **Per-screen text size** is its own spec. V54 only reads the font size.
3. **The page is pinned to the window's centre,** not to the centre pane's.
4. **The compact rule is in characters:** a panel ebbs to fit when it and a full measure don't fit.
5. **Ebbed isn't closed:** render-only, not persisted, selection and scroll kept.
6. **Only typed text ebbs.** Motions, panel edits and sync don't.
7. **An empty thread takes no room.**
8. **No animation in V54;** Anchor removes the motion that animation would hide.
9. **Zed's fractional centred-layout paddings are deleted,** not kept behind a setting.
10. **Shift before narrow:** when the margin can't hold a thread, the page keeps its 68 characters
    and moves off-centre.
11. **Both sides default to `ebb`,** the rail included: with Anchor it no longer has to centre the
    page.
12. **One `measure` for every body font.** A monospace body font gets a wider column, since the
    page is measured in characters.
13. **Same measure everywhere:** the Inbox view, settings and any other single item use the page's
    column too.
