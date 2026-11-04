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

- **Anchor.** The page is a column measured in characters, centred on the *window*, not on the
  space between the docks. It fills the room the docks leave, up to its measure of 80, and
  whatever is left on each side is margin. Opening or closing a panel may widen or narrow the
  page; tucking one away while you write never moves it (decision 19).
- **Tides.** While you type in a note, side panels *ebb*: they shrink to a 6px tide mark at the
  window's edge. They flow back when you pause, when you go to a panel from the keyboard, or when
  you ask. Because of Anchor, neither ebb nor flow moves a single word on the page.

Together they make the desk focus on the page while writing and show everything when you look up,
without modes or setup. They also replace V44's room cascade with one placement rule.

## 2. Goals & success criteria

- **G1: The page doesn't move while you write.** With one pane in the centre, a dock ebbing or
  flowing back leaves every glyph of the note where it was. Opening, closing or resizing a dock
  may widen, narrow or shift the page, because the page fills the room the docks leave
  (decision 19).
- **G2: Centred on the screen.** The page's centre is the window's centre whenever the margins
  allow it, with or without the rail. Keeping a panel open as a counterweight is no longer needed.
- **G3: Measured in characters.** The page's widest and narrowest are character counts of the body
  font at the current size. A larger font gives a wider page, not fewer characters per line, and
  the decision to tuck panels away is made in characters too.
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

The page is the centre pane: its tab bar, breadcrumb and editor together. Its widest is

```
column = measure × advance("0") + editor padding
```

and its narrowest `min_column` is the same with `min_measure`. Between the two, it takes whatever
room the docks leave (§4.2).

with `advance("0")` taken from the body font (V43: Plex Sans, or the buffer font when `"body_font":
"buffer"`) at the current buffer font size. The `0` advance is the CSS `ch` unit and a stable
stand-in for an average character. `measure` defaults to **80**, `min_measure` to **48** (§6).

Soft wrap stays `editor_width` (V43 §7.1). The editor *is* the column, so it wraps at the page's
width.

For a non-note item in the centre (the Inbox view, settings JSON, an image) the column is the same
width (decision 13).

The page has no header row and no lines at its sides (decision 20). Breadcrumbs are off by default,
since the title bar already names the note, so the toolbar is left with only its buttons (preview,
search, selections, editor settings). A toolbar with only right-side buttons and no search row floats
faintly over the item's top-right corner, brightening on hover, instead of taking a row. Opening
the search bar brings the row back while it's open. The margins beside the page are plain editor
background, with no border.

### 4.2 Placement

One pure function decides the whole row:

```rust
fn anchor(input: AnchorInput) -> AnchorLayout
// AnchorInput:  window width, left and right dock widths and policies, column,
//               min_column, thread room (Some only with messages)
// AnchorLayout: left padding, column, right padding, which docks ebb to fit,
//               whether the thread floats
```

`G` is a 24pt minimum gap between the column and anything beside it. The room is what the window
leaves between the open left dock plus `G` and the open right dock plus the thread room plus `G`.
A side ebbed while you type counts at its open width, so ebbing and flowing never move the page.
Steps run in order:

1. **Fill.** The column takes the room, up to `column`, as long as that is at least `min_column`.
2. **Centre, else shift.** The column sits at the window's centre when the margins allow it, and
   otherwise moves toward the side with room, as little as needed.
3. **Ebb to fit.** When not even `min_column` fits, docks with policy `ebb` ebb to their mark
   (§5.2): one side first (the right while a thread is open, otherwise the wider), then both. This
   is the compact rule, measured in characters: on a window too narrow for the page's narrowest
   and the panel, the panel waits at the edge until you call it.
4. **Float.** The thread floats over the page as in V44, and the column takes what's left, even
   below `min_column`.

A dock you bring back while step 3 is in force, or that comes back after a typing pause (§5.4),
flows back *as a dock*. While it is open, the other side ebbs to fit if it may, and otherwise the
page narrows below `min_column`. When it ebbs again (you type), the page returns to where it was.
You aren't writing at that moment, so this doesn't break G1 (decision 18).

### 4.3 The thread

V44's `room_for` cascade and its Desk collapse are replaced by §4.2:

- The thread's room is 408pt (V44 §5.3: the 360pt column plus its gaps), and it lives in the right
  margin.
- **It is requested only when the thread has at least one message to show.** With only the
  composer open, the composer and the pebble float in the bottom-right of the centre, as a
  Needs-you bubble does today. A bubble that can't clear the caret's line takes the room only
  when the right margin already has it free; it never ebbs the Desk or moves the page.
- V44's "collapse the Desk" becomes step 2: the Desk *ebbs*, which is not persisted, and flows back
  when the thread folds.
- A right dock with policy `stay` never ebbs for the thread. The column shifts or narrows, and the
  thread floats as a last resort.
- With centred layout off (or a split centre) there is no page to anchor and nothing ebbs: the
  thread keeps its room on the right of the centre when the centre, as Zed's row draws it, still
  holds `min_column` beside it, and floats otherwise. The docks are left as they are (decision
  15).

### 4.4 Worked numbers

These are approximate: Plex Sans at the shipped 17px gives a `0` of about 10px, so 80 characters
plus padding is about 860pt and 48 about 540pt.

| Window | Panels | Result |
| --- | --- | --- |
| 1512 (14″ MacBook) | rail 255, Desk 320 | Room 889: the full 80 characters, shifted left ≈ 18pt. |
| 1512 | rail 255, Desk 440 (resized) | Room 769: the page takes it all (≈ 71 characters), 24pt from each dock. |
| 1512 | rail, Desk, thread with messages | Room 481 < 540: the Desk ebbs (step 3), and the page takes the 795 left. |
| 1920 (24″ 1080p) | rail, Desk | Centred at 80 characters. |
| 1920 | rail, Desk, thread | Room 889: the Desk stays, and the page shifts left ≈ 222pt. |

Shift before narrow keeps the line length (decision 10), and narrowing before ebbing keeps the
panels you opened in view (decision 19).

## 5. Tides

### 5.1 States

Each side (left, right) of a window with an anchored page is **flowing**, **ebbed** or **held**:

- **Flowing:** drawn whole, or as its mark when it and the page's narrowest don't fit (§4.2
  step 3).
- **Ebbed:** tucked away while you write, drawn as its mark.
- **Held:** brought back while it was a mark, by hand or by a typing pause. It is never ebbed to
  fit, so the other side or the page makes room for it (§4.2), until you type again or the page
  goes away.

Ebbed and held are render states of an open dock: the dock is still open and its panel entity,
selection and scroll position are untouched. They are not persisted, so after a restart every open
dock starts flowing. A side holding the focus is always drawn whole, whatever its state.

A closed dock (`cmd-b`, `cmd-r`, or a panel's toggle) is unaffected by Tides. Hiding stays what it
is today.

### 5.2 The tide mark

An ebbed dock renders as a 6pt strip at the window's edge on the page's ground, the full height
of the dock, with a short grip centred in it: a 3pt by 88pt rounded line in the `text` ink, so it is easy to find on a dark ground and at 1x, so it reads
as something to grab rather than the window's frame, and both sides draw the same mark at the same
inset. The mark answers the pointer 10pt wide: the extra 4pt overlays the page's margin instead
of taking room from it, so the page never moves (G1). Hovering it names the panel and the
`ctrl-cmd-\` shortcut; clicking it flows that side back. The grip turns `text.accent` while the
panel has something waiting:
the Desk while a timed entry starts within 10 minutes or a reminder is ringing. Panels opt in
through a method on the Thock panel side with a default of nothing waiting.

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

Triggers 1, 2 and 3 bring a side back held, so a dock too wide to sit beside the page (§4.2 step 3)
comes back too and the page gives way until you type again (decision 18). The one exception is a
thread with messages: while it is open, a pause brings the right side back only as far as the
thread's room allows, so the Desk stays tucked away for it rather than floating the thread over
the page. Calling the Desk by its key or the tide key still brings it back, and then the thread
floats. Trigger 4 sets every side flowing, so a new page opens centred with a dock too wide for
it waiting at the edge.

### 5.5 Focus and escape

`escape` in a panel returns focus to the editor (CLAUDE.md "Panels"). The panel stays flowing
until you type. Focus never lands in an ebbed dock: an action that would focus an ebbed panel
flows it first (§5.4 rule 1).

### 5.6 When Tides is inactive

Tides is active only while the page is anchored: centred layout on, one pane in the centre, an
item open. With splits or centred layout off, typing ebbs nothing and every side is flowing:
Tides would reflow the panes, which breaks G1. A side ebbed while the page was anchored flows
when the page stops being anchored, so it never comes back ebbed.

## 6. Settings

In `settings.json` beside V43's font settings, because layout is a person's preference, not a
property of the vault:

```jsonc
"thock": {
  "page": {
    // The page's widest, in characters per line of the body font. It fills
    // the room between the side panels up to this.
    "measure": 80,
    // The page narrows to this before a side panel tucks away to fit or an
    // agent thread floats over it.
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

Centred layout is **on by default** for a vault (decision 14). The workspace's saved
`centered_layout` stays unset until it is chosen. While it is unset and the folder is a local
vault (its `.thock/config.toml` is in the project's worktree), Thock turns centred layout on and
saves that as the workspace's choice, before the window's first frame. A folder that becomes a
vault while it is open is centred when its marker appears. Later launches restore the saved
value before anything is drawn, and a later toggle replaces it, so turning it off sticks.

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
   min_column, thread: Option<ThreadRoom>, left: TideSide, right: TideSide }`; each `TideSide`
   carries the side's policy into step 2 (`may_ebb`, off for `stay`) and its `Tide` (§5.1), and
   `ThreadRoom` says whether the room may move the page or ebb a dock (a thread with messages) or
   only take free room (a bubble). The workspace calls `anchor()` on each render with the
   workspace's own width and the window's centre within it (so a sidebar or window decorations
   are left out), and the docks at their stored size or their panel's default, never under the
   panel's minimum. A flexible panel is drawn at that size too beside an anchored page, not from
   its flex, and resizing it there, by its handle or the dock size keys, stores a pixel size and
   leaves its flex for Zed's own row (decision 16). Beside an anchored page each side dock is drawn at exactly the width
   `anchor()` gives it, marks included, and docks drawn whole shrink together when even they
   don't fit the window. The row it placed (`CenterRow`) is kept for the docks to draw their
   marks; it is never an input to placement or to a tide transition. `place_center_row` places a
   row the same way for any anchor, so Thock can ask what the row would be with its tides as they
   are now, and with a side as if it didn't hold the focus. The row width is the workspace's
   bounds as last painted, so on a resize the first frame is placed with the old width and
   corrected on the next (the bounds canvas notifies). Without an anchored page the thread's room
   is checked against what Zed's row leaves between the docks in the same frame, a flexible dock
   sized by its flex as `dock_size` resolves it. The `center_overlay` slot is unchanged.
2. **Tide marks.** `Dock` keeps no tide (decision 17): Thock's `DeskLayout` owns each side's
   `Tide` and sends it in the anchor. A side drawn as a mark renders its tide mark over its
   panel, which stays laid out at its own width and keeps `is_open`. A dock holding the focus is
   never drawn as a mark. Docks ebbed to fit are decided inside `anchor()` on each render. A click
   on the mark only focuses the panel; focus entering the dock brings it back (§5.4 rule 1). The
   dock also takes a `tide_waiting` flag for the mark's accent grip, which `desk_layout.rs` sets
   from the Desk's `has_waiting()`.
3. `adjust_padding` and the fractional settings go.
4. **Centred layout left unset.** The saved `centered_layout` is `None` until it is chosen, and
   the workspace says whether it was (`centered_layout_chosen`). `set_centered_layout` (which the
   toggle uses) marks it chosen, and the workspace's save writes a chosen value, so it survives a
   restart even when the workspace had no database id when it was chosen.

`anchor()` is a pure function in the workspace crate with its own table tests (§9). The PR calls
out the workspace edit and why it can't live in `crates/thock`.

### 8.2 `crates/thock`

A new `desk_layout.rs` owns both rules on the Thock side. It's one logical component:

- **Measure:** reads `thock.page`, the body font and the buffer font size, computes `column` and
  `min_column` through the text system, and calls `set_center_anchor`. It measures again only on
  settings and font-size changes, which is where per-screen text size will plug in. Every change
  made before the deferred sync runs reaches the workspace in one anchor.
- **Thread room:** the chat panel reports `thread_room: Option<Pixels>` (Some while a message is
  shown) instead of running `room_for` and collapsing the Desk. `room_for`, `centred_margins`,
  `collapsed_desk` and `MIN_NOTE_WIDTH` leave `chat_panel.rs`, along with their tests, which are
  rewritten against `anchor()`.
- **Tides:** owns each side's `Tide` and every transition between them (§5.1, §5.4). It subscribes
  to `InputHandled` on the active centre editor (re-subscribing on active-item change), keeps the
  pause timer as a stored task (replacing it is the intended cancel), handles `thock::ToggleTide`,
  holds a side when focus enters its dock if it would be a mark without the focus (ebbed, or
  ebbed to fit), and sets a side flowing when its dock closes. `ToggleTide` brings back every
  side that is a mark, and tucks away every open `ebb` side not holding the focus when none is.
  Both decide with `place_center_row` from the layout's own tides, never from the last frame
  drawn, so a second press before a repaint sees the first. Focus actions run while the
  workspace is being updated, so the anchor reaches the workspace through `cx.defer`, and each
  `ToggleTide` press is decided after the ones before it (CLAUDE.md, GPUI panels).
- **Centred by default:** turns centred layout on for a local vault whose workspace never chose
  it (§6), once the restored workspace has its own setting and again when a path under `.thock/`
  changes. It reads the marker from the project's worktree, with no file read of its own.
- **Waiting signal:** a `has_waiting()` hook the Desk implements for the tide mark's accent grip.

### 8.3 Order of work

1. `anchor()` and its tests; the workspace renders absolute paddings; fractions deleted.
2. Measure in `desk_layout.rs`; V43 notes look the same at 1920 and better at 1512.
3. Thread room through `anchor()`; V44 cascade removed; the composer floats with no messages.
4. Ebbed docks in the workspace; Tides in `desk_layout.rs`; `ToggleTide` and keymaps.
5. Tide mark's waiting line.

Each step ships on its own: Anchor without Tides is already the main fix.

## 9. Tests

- **`anchor()` table** (workspace): windows of 1280, 1512 and 1920; rail and Desk open, closed,
  `stay` and `ebb`; flowing, ebbed, held and focused; thread none, a bubble and with messages.
  Assert the column filling the room up to `column`, centred or shifted, which docks ebb to fit
  and only below `min_column`, and float. An ebbed side places the page where it was flowing. A held dock is never ebbed to fit; an ebbed one is a mark even where it fits, unless it
  holds the focus; a bubble never moves the page; marks keep their width when the window
  squeezes the docks. Without an anchored page the thread takes its room or floats.
- **Flexible panels** (workspace and thock): beside an anchored page a flexible panel is drawn at
  its default size, and resizes to the pixel size its handle or the dock size keys give it.
  Without an anchored page the thread's room is decided from the docks of the same frame, a
  flexible one by its flex.
- **Page doesn't move while you write** (thock, `VisualTestContext`): record the editor's text
  origin; ebb the docks and let them flow back; the origin and width are unchanged each time. On
  the MacBook with a 258 rail and a 439 Desk the page fills the room between them.
- **Ebb on typing:** `simulate_keystrokes` text into a note, and the right dock is ebbed while
  `is_open()` stays true. Vim normal-mode `j j k` doesn't ebb. Ticking a task from the Desk
  (`x` in `ThockBacklogPanel`) doesn't ebb.
- **Flow:** advance the executor timer past `flow_back_after`, and the dock flows. `null` never
  flows. `cmd-alt-u` while ebbed flows the right side, focuses the Backlog, and keeps its selection.
  Clicking the tide mark brings the Desk back focused. `ctrl-cmd-\` flows both sides and leaves
  focus in the editor; pressed again, it ebbs them. One press brings back a Desk ebbed for the
  thread, and one press from inside a panel tucks away the other side; two presses before a
  repaint both count. A Desk ebbed to fit or for the thread and called by its key or a click
  stays whole after `escape`. A new page opens centred with a wide Desk waiting at the edge,
  even after it was held. A burst of tide changes reaches the workspace in one anchor. With a
  258 rail, a 439 Desk and a monospace page, at 1512 one press of the tide key brings the Desk
  back (macOS keys under the JetBrains keymap too), and at 1920 it tucks both sides away. After a
  pause a Desk ebbed to fit comes back with the page giving way, and typing centres it again;
  with a thread with messages open, the pause leaves the Desk tucked away for the thread.
- **Policies:** a `stay` side never ebbs on typing or for the thread.
- **Thread:** composer-only requests no room; the first message requests 408; at 1512 the Desk ebbs
  and flows back when the thread folds. With centred layout off the thread keeps its room at 1440
  and floats at 760, with the Desk left alone.
- **Centred by default:** a vault on the project's worktree is centred before its window's first
  frame and saved as chosen; turned off by hand, it stays off. A folder that becomes a vault is
  centred, and only changes under `.thock/` look again. The saved value is unset until chosen, a
  restored value counts as chosen, and a choice made before the workspace has an id is saved.
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
4. **The compact rule is in characters:** a panel ebbs to fit when it and the page's narrowest
   don't fit (decision 19; it was the full measure).
5. **Ebbed isn't closed:** render-only, not persisted, selection and scroll kept.
6. **Only typed text ebbs.** Motions, panel edits and sync don't.
7. **An empty thread takes no room.**
8. **No animation in V54;** Anchor removes the motion that animation would hide.
9. **Zed's fractional centred-layout paddings are deleted,** not kept behind a setting.
10. **Shift before narrow:** when the margin can't hold a thread, the page keeps its width
    and moves off-centre.
11. **Both sides default to `ebb`,** the rail included: with Anchor it no longer has to centre the
    page.
12. **One `measure` for every body font.** A monospace body font gets a wider column, since the
    page is measured in characters.
13. **Same measure everywhere:** the Inbox view, settings and any other single item use the page's
    column too.
14. **Centred layout is on by default** for a vault: the first time its workspace opens, Thock
    saves "on" as the workspace's own `centered_layout`, so every later launch restores it before
    the first frame and a later toggle wins. The anchored page is the normal state and the
    uncentred row the opt-out; without this most people would never see Anchor or Tides.
15. **No tides without an anchored page.** With centred layout off or a split, the thread keeps
    its room when the centre holds `min_column` beside it and floats otherwise; no dock ebbs or
    closes for it. V44's Desk collapse needed a persisted dock change and a second placement
    rule for a case that is now an opt-out on a small window.
16. **Flexible panels beside an anchored page are drawn at their stored or default size,** never
    from their flex. The flex depends on the other dock's width, which depends on whether it
    ebbs, which depends on this width. Resizing it beside an anchored page stores a pixel size,
    which Zed's own row ignores for a flexible panel; outside an anchored page Zed's resize is
    unchanged.
17. **One owner of tide state:** `DeskLayout` in `crates/thock` owns each side's tide and sends it
    to the workspace in the anchor. The workspace places the row as a pure function of the
    anchor, the docks' widths and the focus, and never writes tide state; the dock keeps none.
18. **A pause brings back a side ebbed to fit, too** (Diego, after the MacBook round). A pause
    holds every side that typing ebbed, so a Desk too wide for the margin comes back and the page
    shifts, then narrows toward `min_column`, until you type again. `Held` already means exactly
    that, so no new state. While a thread with messages is open, the thread's room wins: the
    right side only flows on a pause, so the Desk stays tucked away for the thread you're reading,
    and calling it back by hand floats the thread instead.
19. **The page fills the room, up to 80 characters** (Diego, 2026-11-03, after the MacBook round:
    the fixed 68-character page left dead bands between it and the docks). `measure` becomes the
    page's widest and defaults to 80; between it and `min_measure` the page takes whatever the
    docks leave, centred when the margins allow it. A panel ebbs to fit only when not even
    `min_measure` fits, so on the MacBook the rail and the Desk stay in view beside a ≈ 71
    character page. Ebbing while you type still moves nothing: an ebbed side counts at its open
    width, so the tucked-away panel leaves its room empty rather than reflow the note under the
    caret. Opening, closing or resizing a dock now resizes the page, which G1 gives up.
20. **No header, no side lines** (Diego, 2026-11-03). The breadcrumb row repeated the note's path,
    which the title bar shows, and the heading, which the page shows. Breadcrumbs are off by
    default, and a toolbar left with only its right-side buttons floats over the page at low
    contrast rather than taking a row (`Toolbar::floats`, in the inherited `workspace` crate,
    because the pane draws its toolbar). They stay visible while you type, faint rather than
    hidden, so they are always there to find. The borders at the page's edges went with it.
