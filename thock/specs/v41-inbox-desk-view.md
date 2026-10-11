# Thock V41 - The Inbox at the desk: a keyboard-first view with a suggested destination

**Status:** Planned (2026-10-10)
**Owner:** Diego · **Date:** 2026-10-10
**Design reference:** the design notes with the Jev study, the gpui-kit evaluation, the component
map and the decision cards this spec resolves: <https://claude.ai/artifact/FoeWmrEjqsMiTKvZKCvfBp>.
**Companion docs:** `v13-inbox-routine.md` §4, §6, §9 (what an inbox item is, the note format, the
ritual and its log), `v32-sync-status-indicator.md` §5 (the Inbox row this re-points),
`v39-vault-images.md` §4 (how a picture line resolves), `v40-phone-inbox-gestures.md` §5 and §6
(the writes this view shares with the phone, and `move_file`), `v25-thock-plus-hosted-agent.md`
§5 (the gateway grant the suggestion rides on)

---

## 1. Summary

V13 gave the vault a front door and a ritual to clear it. The ritual is the part that does not get
used: it opens an agent session, reads the queue, and interviews the person about every item. For a
stack of twelve links and three emails that is the wrong shape; the decisions are small and the
person knows most of them before the agent finishes talking.

V41 adds an **Inbox view** that opens in the centre pane like a note: the waiting items on the
left, the selected item rendered on the right, and one key per decision — **Today**, **Soon**,
**Someday**, **Archive** — along the bottom. The writes are V40's, so a desk decision and a phone
swipe leave the same traces. For Thock Plus vaults, each row carries a **suggested destination**
from Jev, TypeSafe's decision model on OpenRouter, and `space` takes it. A suggestion only ever
pre-selects a key; nothing is filed without a keystroke.

The ritual stays for the two decisions that need the vault's folders and some judgement: file a
note into a folder, append to a project page.

## 2. Goals & success criteria

- Open the Inbox with one chord, walk the list with `j`/`k`, clear it with `space` `space` `space`
  where the suggestion is right and one other key where it is not. No mouse, no agent session.
- Every item can be read before it is decided on: the whole note, pictures included, is on the
  right. A subject line is never the only evidence.
- A decision is three writes identical to the phone's (V40 §5): the task line, the `move_file`
  to `archives/inbox/`, the triage-log line in the ritual's format. The Triage Inbox ritual and the
  phone's receipts agree with the view.
- `u` reverses the last decision completely.
- A suggestion is a highlighted default with its probability visible, never an action. A vault
  without a Plus grant gets the same view with no chips and the same keys.
- A pass over twenty items costs well under a cent and the view says what it cost.
- Nothing outside `crates/thock/` beyond a keymap block per platform and the one-line item
  registration.

## 3. Non-goals

- **File as a note in a folder, append to a project page, defer.** The ritual's calls, with the
  policy file behind them. `shift-r` hands what is left to the ritual.
- **Auto-filing at any confidence**, including an opt-in for noise. Inbox items are untrusted
  input (emails, shared links); TypeSafe documents that adversarial content can steer Jev's answer.
  Revisit only with `decisions.jsonl` (§7.6) in hand.
- **Choosing a Day planner section, a backlog category or a time.** Today lands under the planner
  heading, Soon and Someday under their headings; the Backlog panel moves it from there.
- **Editing the item in the view.** `o` opens it in the editor; the reader is read-only.
- **Reopening the tab after a restart.** It is one chord away (decision 7).
- **Suggestions on the phone.** V33 §4 stands: nothing decides on the phone, and a chip there is a
  later question.
- **Adopting gpui-kit as a dependency.** Evaluated in the design notes; see §9.

## 4. The view

### 4.1 A workspace item

`InboxView` implements `workspace::Item` (the precedent is `ExtensionsPage` in
`crates/extensions_ui` and `Onboarding` in `crates/onboarding`): a tab in the centre pane titled
`Inbox · 7` with the live count, `IconName::Envelope`, no toolbar, not splittable, a singleton per
workspace. `thock::OpenInbox` opens it in the active pane or focuses the existing tab; `escape`
from the list returns to the item that was active before.

The key context is `ThockInboxView` plus `menu`, so the `menu::` bindings apply; `filtering` is
set while the filter box has focus and `reader` while the reader has it.

### 4.2 Three regions

```
┌ Inbox · 7 ┐ daily/2026-10-10.md   backlog.md
──────────────────────────────────────────────────────────────────────────
 Inbox   7 waiting · suggestions on · 3 filed today · $0.0002      / ?
────────────────────────────────┬─────────────────────────────────────────
 NEW · 4                        │ google-tasks · captured 2026-10-09 18:42
▸ Ship it — a practical guide…  │
  gtasks               → Someday│ Ship it — a practical guide to shipping
  Re: Q4 planning doc           │
  gmail  Marta       → Soon 71% │ https://example.com/ship-it
  Call Ana about the keys       │
  phone                 → Today │ (no content)
 CARRIED OVER · 3               │
  idea: routines could carry…   │
  phone  3 days            —    │
────────────────────────────────┴─────────────────────────────────────────
 Someday ████████▌ 82%   Soon ▉ 9%   Today ▌ 5%   Archive ▌ 4%
 [space] Someday  [t] Today  [s] Soon  [S] Someday  [e] Archive  [o] Open  [u] Undo
```

**Header.** The count, whether suggestions are on, what this session filed and what it cost
(§7.5), and the two affordances a mouse user needs to find the rest: the filter box (`/`) and the
keys sheet (`?`). A vault without a gateway grant shows a dismissible banner in place of the cost:
*Suggestions come with Thock Plus.*

**List.** Two lines per item: the title; then the source as a chip, the sender when there is one,
the age for carried-over items, a picture glyph when the note has an image line, and the
suggestion chip right-aligned. Two sections, **New** and **Carried over**, split by the ritual's own
rule (`triage-inbox.md` §1: `captured:` or file mtime against `.thock/state/inbox/last-triage`),
so the two surfaces agree on what is old. The list is a `uniform_list`; the selection is kept by
stem and survives refresh, re-parse and vault file events. When the selected row is filed or
disappears underneath, the selection moves to the next row, so `space` `space` `space` walks the
list.

**Reader.** The selected note's body rendered with `markdown::MarkdownElement` and
`MarkdownStyle::themed`, as the chat panel renders the agent's replies. Frontmatter is not shown;
its fields become the meta line above the body (source, sender, captured, due, link). Image lines
resolve by V39 §4's rule before rendering so `![alt](/images/x.png)` draws the vault's picture. The
reader is read-only. The split between list and reader is draggable (§9) and remembered per vault
in `.thock/state/inbox/layout.json` (`{"list_width": 0.38}`), defaulting to 38% of the pane.

**Action bar.** The probability meter for the selected item (§7.4) and the keycaps, rendered with
`ui::KeyBinding::for_action_in` so a rebinding shows up without a code change. The first keycap
always reads what `space` will do for the selected item — the suggested destination, or *Open*
when there is none — so the default is never a surprise.

### 4.3 Marks

`x` marks the selected row; marked rows show a check in the gutter and the header reads `3
marked`. While any row is marked, the four verbs apply to every marked row in list order, and
`space` applies each row's own suggestion (rows without one are skipped and stay marked). Marks
clear when the list refreshes from disk. `escape` clears marks before it leaves the view.

## 5. Keys and focus

Focus lives in the list. Every verb is a named `thock::` action; the action bar, the row menu and
the keys sheet render from the bindings.

| Key | Action | Does |
| --- | --- | --- |
| `ctrl-alt-i` (global) | `thock::OpenInbox` | Opens or focuses the Inbox tab |
| `j` `k` `down` `up` `g g` `shift-g` `home` `end` | `menu::SelectNext` … `menu::SelectLast` | Moves the selection; vim letters gated on vim mode |
| `space` | `thock::AcceptInboxSuggestion` | Files to the chip; opens the note when there is none |
| `t` | `thock::SendInboxItemToToday` | Task under today's Day planner heading |
| `s` | `thock::SendInboxItemToSoon` | Task under **Soon** |
| `shift-s` | `thock::SendInboxItemToSomeday` | Task under **Someday** |
| `e` | `thock::ArchiveInboxItem` | Out of the inbox, nothing else |
| `o` `enter` | `thock::OpenInboxItem` (`menu::Confirm`) | The note in the editor |
| `u` | `thock::UndoInboxMove` | Reverses the last decision (§6.2) |
| `x` | `thock::MarkInboxItem` | Toggles a mark (§4.3) |
| `r` | `thock::SuggestInboxDestinations` | Re-asks for the selected (or marked) items; with none selected, for every row without a suggestion |
| `/` | `thock::FilterInbox` | Focuses the filter box; narrows by title, sender, source; `escape` clears and returns |
| `tab` | `thock::FocusInboxReader` | Moves focus to the reader, where `j`/`k`/`ctrl-d`/`ctrl-u` scroll; `tab` or `escape` returns |
| `.` | `thock::ShowInboxItemMenu` | The row menu: the verbs above with their bindings; also right-click |
| `?` | `thock::ShowInboxKeys` | The keys sheet over the reader; any key closes it |
| `shift-r` | `thock::TriageInbox` | Hands what is left to the ritual (unchanged action) |
| `escape` | `menu::Cancel` | Clears marks or the filter; otherwise returns to the previous item |

Bindings go in the Thock blocks of `assets/keymaps/default-linux.json` and `default-macos.json`
under `ThockInboxView`, `ThockInboxView && !filtering && !reader` and `ThockInboxView && reader`,
and the vim letters in `assets/keymaps/vim.json` under `ThockInboxView && !filtering`, mirroring
the Backlog panel's blocks. `ctrl-alt-i` joins the global Thock toggles.

Rows are also clickable (select), double-clickable (open) and right-clickable (menu), and the
header's filter and keys controls are buttons, so the view is complete for a mouse user too.

## 6. The writes

### 6.1 Filing

A decision is V40 §5's writes, applied by the desk in the same order, through the project `Fs`:

| Key | 1 | 2 | 3 |
| --- | --- | --- | --- |
| `t` | `- [ ] <title>[ [[stem]]]` appended under the Day planner heading of today's note (`notes::ensure_note` creates it from the template; `backlog::append_block_to_note_edit` places it) | `move_file` `inbox/<name>` → `archives/inbox/<name>` | the log line |
| `s` / `shift-s` | the same task line under **Soon** / **Someday** via `backlog::append_to_section_edit` | the same `move_file` | the same log line |
| `e` | — | the same `move_file` | the same log line |

- The task line, the `[[stem]]` rule and the archive name are V40 §5's, byte for byte; the desk
  calls the same `thock_sync_core` helpers the phone's builders mirror, so one fixture covers both.
- The move applies `Operation::MoveFile` through `thock_sync_core::apply` and its V40 §6.1 rules:
  nothing at `path` is a `noop` (the ritual or the phone got there first; the row just leaves),
  a file at both is a `noop` surfaced in the header (*Already archived · open the folder*), never an
  overwrite.
- The log line is the ritual's format (V13 §9.5) with V40's three destination keys, written by
  the same formatter the fixtures pin: `- 2026-10-10 · Call Ana → Backlog · Soon <!--inbox:…-->`,
  marker omitted when the note has no `capture:` digest. `Someday` writes `→ Backlog · Someday`.
- The decision is appended to `.thock/state/inbox/decisions.jsonl` (§7.6) after the writes land.
- A failed write surfaces in the header with the error and leaves the row in place; the partial
  order is V40's (task first) so the worst case is a task whose note is still in the inbox.
- The view does not touch `.thock/state/inbox/last-triage`; that watermark is the ritual's.

### 6.2 Undo

The writes land at once and the row leaves; a toast (`workspace.show_toast`) reads *Added to Soon
· Undo* with the action bound. `u` or the toast reverses the last decision: `move_file` back,
`remove_line` for the task line it appended (matched by text and heading, a no-op if the person
edited it meanwhile), `remove_line` for the log line (matched by its marker, or its full text when
there is none), and a `"undone": true` field on the `decisions.jsonl` record. One level; a second
decision replaces it. Marked-batch decisions undo as a batch, in reverse order.

## 7. Suggestions

### 7.1 What Jev is, for this spec

`typesafe/jev-1.13` (alias `~typesafe/jev-latest`) answers typed questions about a `state` and
returns probabilities, not text: `POST https://openrouter.ai/api/alpha/decisions`, bearer the
OpenRouter key, `{ model, state, questions }` in, `{ model, answers, usage }` out. A `choice`
answer carries `choice`, `probabilities` per option and `confidence` (0–1, the peakedness of the
distribution, normalised for the number of options). Input is priced per token (currently $0.042
per million), output is free, and every response has `usage.cost` in USD. The limit is 32,000
input tokens. Jev returns no explanation; the view shows probabilities, not reasons.

### 7.2 The request

Built by `inbox_suggest::build_request(item, policy, today)`, pure, no I/O:

```json
{
  "model": "typesafe/jev-1.13",
  "state": {
    "item": {
      "title":   "Ship it — a practical guide to shipping",
      "source":  "google-tasks",
      "from":    "",
      "url":     "https://example.com/ship-it",
      "excerpt": "",
      "due_in":  "none",
      "has_image": false
    },
    "policy": [
      "A link or article to read → someday, as a task carrying the link",
      "A raw idea or thought → someday",
      "Something with a due date within a week, or clearly urgent wording → soon",
      "Something that reads like today's business (an errand, a call, a follow-up) → today",
      "Obvious noise (a test capture, an empty item, a duplicate) → archive"
    ]
  },
  "questions": {
    "destination": {
      "type": "choice",
      "instructions": "Where should `item` go, following `policy`?",
      "criteria": {
        "today":   "Something to do, call, or read today, or a line for today's page.",
        "soon":    "A task for the coming days: `item.due_in` is within a week, or the wording is urgent.",
        "someday": "A link, article, idea or task with no date pressure.",
        "archive": "Noise with nothing to act on: a notification, receipt, newsletter, test capture or duplicate.",
        "unsure":  "None of the above fits, or the item is a note that belongs in a folder of its own."
      }
    }
  }
}
```

- **State is the item and the policy, nothing else.** TypeSafe documents that unrelated material
  in `state` costs accuracy. No backlog, no daily note, no memory, no history (decision 6).
- **`excerpt`** is the body after the heading, trimmed to 1,200 characters at a line boundary.
- **`due_in`** is computed in Rust from `due:` and today — `"within a week"`, `"later"`, `"none"` —
  because Jev reads dates as text and does not compare them.
- **`policy`** is the *Proposals* table of `routines/inbox/triage-policy.md`, each row flattened
  to `<looks like> → <destination word>` where the destination maps to one of the five options
  (`Backlog · Someday` → `someday`, `Today's note…` → `today`, `Discard` → `archive`, folder and
  append rows → `unsure`). A missing or unparseable policy file sends the shipped defaults. One
  editable file steers the ritual and the suggestion.
- **Criteria are full sentences** with boundary cases, because Jev reads literally. `unsure` is
  last and the order is fixed (Jev leans to the first option); `decisions.jsonl` is how the bias
  gets measured rather than a second reversed call (decision 4).
- A `has_image` note sends only its alt text; the picture itself is not evidence Jev can see.

### 7.3 When it runs, and where

The `InboxService` asks only while an Inbox view is open, only for items with no cached
suggestion, at most four requests in flight, newest items first (decision 3). The call is made
from the desk with the Plus entitlement's gateway key when `gateway.provider == "openrouter"`
(decision 2); any other provider, or no grant, means no suggestions. The endpoint is a constant in
`inbox_suggest.rs`; the desk grant has no base URL and does not need one.

This is the first model call the desk makes itself rather than through an agent harness. It is a
single HTTP request in the service, not a second agent path. The sentence in `agent.rs` saying
Thock never speaks to a model is amended to say it never speaks to a *chat* model.

Failures are per item and quiet: a 429 or 5xx retries with backoff, a 402 means the allowance is
spent (`Entitlement::is_exhausted` already gates the chat panel the same way), anything else marks
the item *—* with the reason in the chip's tooltip. The header shows one line, *Suggestions paused ·
Retry*, while any item is in that state. Never a modal, never a blocked key.

### 7.4 Tiers

`parse_response` yields `Suggestion { destination, confidence, probabilities, cost }`. The tier
changes what the row shows, never what the view does on its own:

| Confidence | Chip | `space` |
| --- | --- | --- |
| ≥ 0.85 | filled, `→ Soon` | files to the suggestion |
| 0.50 – 0.85 | outlined, `→ Soon · 62%` | files to the suggestion |
| < 0.50, or `choice == "unsure"` | none; the row's second line ends with `—` | opens the note |

The meter in the action bar draws all four filing options for the selected item as
`ui::ProgressBar`s (`unsure` is folded into the label *no suggestion*), so an outlined chip's
uncertainty is visible as a spread, not just a number. TypeSafe's own three-tier guidance is
0.9 / 0.5; this view sits lower on the top tier because a person is looking at every row.

### 7.5 The cache and the cost

`.thock/state/inbox/suggestions.jsonl`, one record per asked item:

```json
{"hash":"3f1c…","stem":"2026-10-09-1842-ship-it","model":"typesafe/jev-1.13",
 "choice":"someday","confidence":0.91,
 "probabilities":{"today":0.05,"soon":0.03,"someday":0.91,"archive":0.01,"unsure":0.00},
 "cost":0.0000231,"asked_at":"2026-10-10T09:14:02Z"}
```

`hash` is the sha256 of the note's bytes, so an edited item is re-asked and the same bytes never
cost twice. The file is a cache: deleting it costs one pass. The header's cost is the sum of
`cost` for records written this session; the sync popover's Inbox row gains *$0.01 this month*
from the file's records.

### 7.6 The decision log

`.thock/state/inbox/decisions.jsonl`, one record per filing:

```json
{"stem":"2026-10-09-1842-ship-it","suggested":"someday","confidence":0.91,
 "chosen":"someday","via":"space","at":"2026-10-10T09:14:40Z"}
```

`suggested` and `confidence` are null without a grant; `via` is the key (`space`, `t`, `s`, `S`,
`e`, `menu`); `undone` is added by §6.2. This is the labelled sample TypeSafe's cookbooks use to
set thresholds: after a few weeks it says whether 0.85 is the right line, whether the first-option
bias shows, and where misses cluster. It is also the only material a later policy-personalisation
could draw on (decision 6). Nothing reads it in V41 beyond a `thock: inbox suggestion report`
line in the sync popover's tooltip (*suggestion taken 83% of the time, 212 decisions*).

## 8. Items and the service

### 8.1 `InboxItem`

`inbox.rs` gains the parser that mirrors `render_inbox_note`:

```rust
pub struct InboxItem {
    pub path: String,            // vault-relative, inbox/<name>.md
    pub stem: String,
    pub title: String,           // frontmatter title, else first heading, else the stem
    pub source: Option<String>,
    pub from: Option<String>,
    pub url: Option<String>,
    pub link: Option<String>,
    pub captured: Option<String>, // RFC 3339 as written
    pub due: Option<String>,      // YYYY-MM-DD as written
    pub digest: Option<String>,   // capture:
    pub body: String,             // everything after the frontmatter
    pub has_body: bool,           // V40 §5's rule: a non-blank line after the heading
    pub has_image: bool,          // an image line in the body
}
pub fn parse_inbox_note(path: &str, content: &str) -> InboxItem
```

A note with no frontmatter is a valid item (V13 §4.2); every field is optional. A round-trip test
pins `parse_inbox_note(render_inbox_note(item))` for every `CapturedItem` shape.

### 8.2 `InboxService::items()`

The service already scans the landing folder for its depth. It keeps the parsed items alongside
(`Vec<InboxItem>` plus the watermark-derived `is_new` per item), refreshed by the same latest-wins
task that `refresh_queue_depth` runs on any event under `config.dir`, and emits
`InboxEvent::ItemsChanged` so the view re-renders without polling. `queue_depth()` becomes
`items().len()`. The suggestion cache (§7.5) is loaded once and kept in memory; `suggestion_for
(hash)` answers the view, and `request_suggestions(hashes)` is the view's way to ask while it is
open.

## 9. Components

The design notes evaluated gpui-kit (Longbridge, Apache-2.0) as a component source. It is built
against `gpui-pre`, a published weekly snapshot of Zed's gpui, and needs its own theme global,
asset source and window root; none of that fits inside a Zed fork's `Workspace` without shim
crates outside `crates/thock/` and a bridge that every upstream sync re-opens. Decision 1: Zed's
own `ui` and `markdown` crates for everything that exists, and a port of what they lack into
`crates/thock/src/ui/`, with gpui-kit as the visual reference.

| Element | Built with |
| --- | --- |
| Tab, singleton, pane host | `workspace::Item`, `add_item_to_active_pane`, `items_of_type` |
| List, sections, virtualisation | `gpui::uniform_list`, `ui::ListItem`, `ui::ListHeader` |
| Source chip, suggestion chip | `ui::Chip` (filled vs outlined by tier) |
| Probability meter | `ui::ProgressBar` per option |
| Keycaps | `ui::KeyBinding::for_action_in` |
| Reader | `markdown::MarkdownElement`, `MarkdownStyle::themed`, V39 image resolution |
| Row menu | `ui::ContextMenu` with `.action()` entries |
| Filter box | `editor::Editor::single_line` |
| Toast with Undo | `workspace::Toast` with an action |
| Empty state, Plus banner, paused line | `ui::Callout`, `ui::Banner` |
| **List / reader split** | **ported:** `crates/thock/src/ui/split.rs`, a horizontal two-pane element with a draggable handle, min widths and a `Resized` event, modelled on gpui-kit's `h_resizable` |
| **Sectioned list delegate** | **ported shape:** `crates/thock/src/ui/sectioned_list.rs`, a `SectionedListDelegate` trait (`sections`, `items_in`, `render_item`, `render_header`) over `uniform_list`, so the Inbox and a future Backlog refresh share one list spine |

Ported files carry the Apache-2.0 notice and the gpui-kit source path they derive from. Nothing
else from gpui-kit is vendored, and nothing is added to the workspace's dependencies.

## 10. Plumbing

- **Actions.** `thock::OpenInbox` changes meaning: it opens the view. Its old job — reveal the
  folder in the project panel — becomes `thock::RevealInboxFolder`, kept bindable and used by the
  header's *Already archived* line. The new actions in §5 live in `inbox_view.rs`'s `actions!`
  block with doc comments written for a note-taker. `dispatch_triage`'s fallback (when the skill is
  not installed) now opens the view instead of revealing the folder.
- **The sync indicator's Inbox row** (V32 §5): *N waiting* keeps `Attention::Waiting` and its
  actions become **Open** (`thock::OpenInbox`) and **Sync now**; **Triage** moves into the view's
  `shift-r`. The Backlog panel's inline row follows, since both render from the one
  `ConnectorStatus`.
- **Config.** Nothing new in `.thock/inbox.toml`. `[inbox] suggestions = false` in the vault's
  `.thock/config.toml` turns suggestions off for a Plus vault that does not want them; absent means
  on.
- **The routine.** `routines/inbox/routine.toml` gains a `[[link]]` *Inbox* that dispatches
  `thock::OpenInbox` (group *Today*), replacing the comment that said there is deliberately no row
  that opens `inbox/`. The routine doc (`doc.md`, materialised as `routines/inbox/Inbox.md`) gains a
  paragraph on the view and its keys.
- **Registration.** One `InboxView` registration call in `crates/zed/src/zed.rs` next to the panel
  loads, the only line outside `crates/thock/` beyond the keymaps.

## 11. Implementation notes

- `crates/thock/src/inbox_view.rs` — the item, its state (items, selection by stem, marks, filter,
  reader focus, last decision for undo), rendering, actions. Writes go through the same helpers the
  Backlog panel's `send_to_today` uses (`notes::ensure_note`, `open_local_buffer`, apply, save),
  then `thock_sync_core::apply` for the move, then the log append, each awaited before the next.
- `crates/thock/src/inbox_suggest.rs` — `build_request`, `parse_response`, `tier`, the policy
  flattener, the cache and decision record types; no I/O, no GPUI.
- `crates/thock/src/inbox.rs` — `InboxItem`, `parse_inbox_note`, `is_new(item, watermark)`.
- `crates/thock/src/inbox_service.rs` — items, the suggestion requests (the HTTP client is the
  project's `http_client::HttpClient`, as `plus.rs` uses), cache and decision-log persistence.
- `crates/thock/src/ui/split.rs`, `crates/thock/src/ui/sectioned_list.rs` — §9's ports.
- `crates/thock/src/thock.rs` — `inbox_view::init(cx)`.
- The triage-log formatter and the task-line builder move into `thock_sync_core` if they are not
  already shared, so the phone's fixtures pin the desk's output.

**Outside `crates/thock/`:** the keymap blocks in `assets/keymaps/default-linux.json`,
`default-macos.json` and `vim.json`; one registration line in `crates/zed/src/zed.rs`.

**Order of work:** `InboxItem` and `items()` → the view with keys, writes and undo, no suggestions
→ the split → `inbox_suggest.rs`, the chips and the meter. The view is useful at step two; Jev is
step four.

## 12. Tests

- **`inbox.rs`:** `parse_inbox_note` round-trips every `render_inbox_note` shape; a note with no
  frontmatter; `has_body` and `has_image` edge cases (heading only, blank lines, an image line
  alone); `is_new` against the watermark with `captured:` and with mtime.
- **`inbox_suggest.rs`:** `build_request` against a golden JSON for a link, an email with a body
  over 1,200 characters, a `due:` tomorrow, a hand-written note; the policy flattener on the
  shipped `triage-policy.md` and on a file with a folder row; `parse_response` on a captured Jev
  response fixture (`crates/thock/src/test_fixtures/jev_choice.json`), on `unsure`, on a missing
  `confidence`; `tier` at 0.85, 0.5 and below.
- **Writes:** the log line and the task line for each key equal the V40 fixtures byte for byte;
  `move_file` through `apply` on the fake `Fs` for the applied, both-present and missing cases;
  undo restores the three files to their prior bytes; a failed task append leaves the row and the
  inbox file.
- **View (`VisualTestContext` + `simulate_keystrokes`):** open with `ctrl-alt-i`; `j` `j` `space`
  files the third item to its suggestion and the selection lands on the next row; `t` on a row with
  no suggestion appends under the planner heading; `x` `x` `e` archives two marked rows in order;
  `u` after `s` restores; `/` `ana` `escape` filters and clears; `tab` `j` scrolls the reader and
  `escape` returns; a file event removing the selected item moves the selection without a panic;
  with no gateway grant, no chip is rendered and `space` opens the note; with vim mode on, `j`/`k`
  move and with it off they do not.
- **Service:** suggestions are requested only while a view is open, only for uncached hashes, at
  most four at a time; a 402 pauses without retry; the cache file is appended after each answer and
  loaded on start; the decision record is written after the writes, with `via`.
- **Sync status:** the Inbox row's actions read Open and Sync now; the Backlog panel's inline row
  matches.
- **Skill:** `triage-inbox.md` is unchanged; a test asserts the view's log line parses with the
  ritual's marker scanner (`scan_triage_log_markers`), so the rebuild scan sees desk decisions.

## 13. Decision log (2026-10-10)

1. **Components: Zed-native, port what Zed lacks** (the split and the sectioned-list shape) into
   `crates/thock/src/ui/`. gpui-kit as a dependency needs five `[patch.crates-io]` shim crates, a
   theme bridge and asset chaining, all outside `crates/thock/` and re-opened on every upstream
   sync, for two components' worth of gain.
2. **Jev is called from the desk**, with the gateway key the Plus entitlement already carries, not
   through a backend route. One HTTP call, no deploy, the cost stays local; the policy has one
   home, the vault.
3. **Jev runs while the view is open**, not in the capture poll. No spend on items never looked
   at, no machinery rewriting a note's frontmatter, hand-dropped files treated the same.
4. **Ask once and measure.** A second call with reversed options would halve false confident chips
   at double a negligible cost, but the bias is a hypothesis; `decisions.jsonl` tests it on real
   decisions first.
5. **Immediate writes, `u` undoes.** The phone delays its writes behind a toast because a sync
   write undone must leave no trace on the wire; the desk writes locally and the three writes are
   reversible, so the list stays honest with the files.
6. **State is the policy only.** Recent corrections as examples are the only personalisation Jev
   offers and the first thing TypeSafe warns costs accuracy; the policy file is where the person's
   rules are meant to live.
7. **No `SerializableItem` in V41.** The Inbox is a tab you open to clear and `ctrl-alt-i` is one
   chord.
8. **Centre pane, not a dock.** Rev 1 of the design notes compared a right-dock panel, a column in
   the Backlog and a one-at-a-time review; a triage session deserves the whole pane, and a list
   beside a reader is the shape that makes a suggestion checkable and a keystroke fast.
