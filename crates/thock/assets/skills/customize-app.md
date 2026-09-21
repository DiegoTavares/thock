# Customize App

> A ritual for the agent, launched from **thock: customize app** in the
> command bar — and the reference to read whenever the user asks for a
> different look, a different font size, or a different shortcut. A human
> reading this: it's everything your agent needs to change Thock's settings
> and keys without guessing. Edit it if you'd like it done differently.

You are changing how Thock itself looks and behaves. These files live
**outside the vault**, in the app's own config folder, so the vault ground
rules about appending to notes don't apply here — but everything below is a
file the user may have hand-edited, so the rule that does apply is: **change
the keys you were asked about and leave every other byte alone.**

Both files are watched. A saved change takes effect immediately — no
restart, no reload command. If the user says "nothing happened", the change
is wrong, not pending.

## The four files, and which one you want

| File | Holds | Use it when |
|---|---|---|
| `~/.config/thock/settings.json` | How the app looks and behaves: theme, fonts, editor behavior | "make the text bigger", "switch to a light theme", "turn on vim mode" |
| `~/.config/thock/keymap.json` | Keyboard shortcuts | "change my backlog shortcut", "bind a ritual to a key" |
| `<vault>/.thock/config.toml` | Vault behavior: parsed headings, language, Day Planner, memory | Owned by other rituals — see below |
| `<vault>/.zed/settings.json` | Per-vault overrides of *editor* settings only | Rarely. Only when the user wants a setting in this vault but not others |

Paths on other platforms — the shape is the same, only the folder moves:

- **macOS / Linux:** `~/.config/thock/` (Linux honors `$XDG_CONFIG_HOME`)
- **Windows:** `%APPDATA%\Thock\`

Prefer `~/.config/thock/settings.json` over `.zed/settings.json`: the user
has one Thock, and a vault-local file is a surprise waiting in six months.
Theme, fonts and vim mode are global-only anyway — setting them in
`.zed/settings.json` does nothing.

**Don't reach for `.thock/config.toml` from here.** It belongs to the
rituals that own its sections: Set Language writes `[language]` and the
heading names, the Day Planner writes `[day_planner]`. If what the user
asked for lives there, say which ritual does it and offer to run that one.

## Editing the files safely

Both files are **JSON with comments** (JSONC): `//` comments and trailing
commas are legal and the shipped files use both.

1. **Read the file first, every time.** Never write one from an assumption
   about what it contains.
2. **If it doesn't exist**, create it with the starter content below —
   settings.json as `{}` with your key inside, keymap.json as `[]` with
   your one block inside.
3. **Merge, don't replace.** Add or change the one key you were asked
   about. Keep every other key, every comment, and the file's existing
   indentation (two spaces).
4. **Never run a strict JSON validator on these files** — `jq`,
   `python3 -m json.tool` and friends reject the comments and trailing
   commas that are supposed to be there. To check your work, read the file
   back and confirm the braces balance and your key is where you put it.
5. **Say what you changed**, in the user's terms: *"Your note text is 19pt
   now — it's already applied, look at the note behind me."*

A syntax error means the change silently doesn't apply and Thock keeps
running on the last good version of the file. That's the usual cause of
"nothing happened."

## settings.json

Starter file, if there is none:

```json
// Thock settings
{
  "buffer_font_size": 19,
}
```

### The settings people actually ask for

Everything here is a **top-level** key in that object.

| The ask | Key and value |
|---|---|
| "bigger / smaller note text" | `"buffer_font_size": 17` (Thock's default; a point or two at a time) |
| "bigger / smaller menus and panels" | `"ui_font_size": 16` |
| "a different note font" | `"buffer_font_family": "JetBrains Mono"` — must be installed |
| "a different UI font" | `"ui_font_family": "Inter"` |
| "more air between lines" | `"buffer_line_height": "comfortable"` \| `"standard"` \| `{ "custom": 1.8 }` |
| "light theme" / "dark theme" | see **Themes** below |
| "different file icons" | `"icon_theme": "Thock (Default)"` |
| "vim keys" | `"vim_mode": true` |
| "keys like VS Code / Sublime / JetBrains" | `"base_keymap": "VSCode"` \| `"Atom"` \| `"JetBrains"` \| `"SublimeText"` \| `"TextMate"` \| `"Emacs"` \| `"None"` |
| "wrap my long lines" | `"soft_wrap": "editor_width"` \| `"bounded"` \| `"none"`, with `"preferred_line_length": 80` for `bounded` |
| "stop the cursor blinking" | `"cursor_blink": false` |
| "a thinner / wider writing column" | `"centered_layout": { "left_padding": 0.2, "right_padding": 0.2 }` (0.0–0.4 each) — only once **workspace: toggle centered layout** is on |
| "stop saving constantly" | `"autosave": "off"` — Thock ships `"on_focus_change"` on purpose; warn that the agent reads what's on disk |
| "ask before quitting" | `"confirm_quit": true` |
| "don't reopen my last vault" | `"restore_on_startup": "none"` \| `"last_workspace"` \| `"last_session"` |
| "show line numbers relative to the cursor" | `"relative_line_numbers": true` |
| "stop auto-updating" | `"auto_update": false` |

Thock's own defaults differ from stock Zed in three places worth knowing:
`buffer_font_size` is `17` (notes read better than code), `autosave` is
`"on_focus_change"`, and `icon_theme` is `"Thock (Default)"`.

### Themes

`theme` takes either a single name or a light/dark pair that follows the
system:

```json
{
  "theme": {
    "mode": "system",   // or "light" / "dark" to pin one
    "light": "One Light",
    "dark": "One Dark",
  },
}
```

Bundled theme names — **use these exactly, and don't invent others:**
`One Light`, `One Dark`, `Ayu Light`, `Ayu Mirage`, `Ayu Dark`,
`Gruvbox Light`, `Gruvbox Light Soft`, `Gruvbox Light Hard`,
`Gruvbox Dark`, `Gruvbox Dark Soft`, `Gruvbox Dark Hard`.

The user may have more from extensions or from `~/.config/thock/themes/` —
list that folder before telling someone a theme doesn't exist. If they want
to *browse* rather than name one, don't edit anything: tell them `⌘ K ⌘ T`
opens a live theme picker that saves the choice itself.

### A setting that isn't on the list

Every setting Thock understands is in its default settings file, with a
comment explaining it. You can't read that file from disk — it's compiled
into the app — so when you don't know a key, don't guess one: tell the user
to open the command bar (`⌘ ⇧ P`) and run **zed: open default settings**,
which opens the whole annotated list read-only, and offer to make the change
once they paste the key. A key Thock doesn't recognize is ignored in
silence, which looks exactly like a broken change.

## keymap.json

Starter file, if there is none:

```json
// Thock keymap
[
  {
    "context": "Workspace",
    "bindings": {
      "cmd-alt-b": "thock::ToggleBacklogFocus",
    },
  },
]
```

The file is an **array of blocks**. Each block has an optional `context`
and a `bindings` map of keystroke → action.

### Writing a keystroke

A binding key is a sequence of keypresses separated by spaces; each press
is modifiers then a key:

- `ctrl-` control · `alt-` alt/option · `shift-` shift · `fn-` function
- `cmd-` the platform key (Command on macOS, Windows key, Super on Linux)
- `secondary-` resolves to `cmd` on macOS and `ctrl` elsewhere — reach for
  this when writing a binding meant to work on both

Examples: `"cmd-alt-b"` (one chord), `"cmd-k cmd-t"` (press `⌘K`, then
`⌘T`), `"g space"` (type `g` then space), `"shift shift"` (tap shift
twice — fires on release).

Two traps:

- `shift-` only combines with a **letter**, to mean its uppercase. `shift-(`
  never matches; bind the character itself.
- On macOS `alt-c` types `ç`. Write it as `alt-c` by convention; both match.

Mind the platform the user is on: Thock's own defaults are `cmd-` on macOS
and `ctrl-` on Linux and Windows, so `cmd-alt-u` and `ctrl-alt-u` are the
same shortcut described from two machines. Use `secondary-` if you don't
know which they're on, or ask.

### Contexts

A block with no `context` is always active. With one, it applies only where
that context matches. Contexts nest, root `Workspace` on the outside down to
the focused thing.

- `X && Y`, `X || Y`, `!X`, `(X)`
- `X > Y` — an ancestor matches `X` and this layer matches `Y`
- Attributes read on the node that defines them: `mode == full`,
  `vim_mode == normal`

The contexts you'll want:

| Context | Where it's live |
|---|---|
| `Workspace` | Anywhere in the window — the right home for a panel toggle or a ritual |
| `Editor` | Any text input, including small inline ones |
| `Editor && mode == full` | A note being written, and not the little inputs |
| `Editor && vim_mode == normal` | Vim normal mode in a note (also `insert`, `visual`) |
| `ThockRoutinesPanel` | The Routines rail (`⌘ 2`) |
| `ThockBacklogPanel`, `... && !editing` | The Backlog; `!editing` excludes rows being typed into |
| `ThockDayPlannerPanel` | The Day Planner rail |
| `ThockAgentPanel` | The terminal agent panel |
| `ThockChatPanel`, `ThockChatPanel > Editor` | The Thock Agent chat, and its message box |
| `Editor && ThockMarkdownConceal` | A vault note with concealed Markdown |

Zed's key-context debug view (`dev::OpenKeyContextView`) is **hidden from
Thock's command bar** — the whole `dev` namespace is. Don't send the user
looking for it; pick the context from this table instead.

### Writing an action

Most actions are a plain string: `"cmd-alt-b": "thock::ToggleBacklogFocus"`.

Actions that take data are an array of the name and its argument:

```json
{
  "bindings": {
    "cmd-alt-w": ["thock::RunSkill", { "skill": "wrap-today" }],
    "cmd-alt-1": ["thock::OpenLink", { "routine": "timeline", "link": "today" }],
    "cmd-1": ["workspace::ActivatePane", 0],
  },
}
```

`null` as the action disables a key in that context:

```json
{ "context": "Workspace", "bindings": { "cmd-r": null } }
```

### Thock's actions

Rituals and Routine links don't each get their own action — they go through
the two data-carrying actions above. `skill`, `routine` and `link` are the
ids in `routines/<id>/routine.toml`; read that file for the exact strings
rather than guessing from the visible name.

**Panels and notes** (bind in `Workspace`):

| Action | Does | Ships as |
|---|---|---|
| `thock::ToggleFocus` | The Routines rail | `cmd-2` |
| `thock::ToggleBacklogFocus` | The Backlog | `cmd-alt-u` |
| `thock::ToggleDayPlannerFocus` | The Day Planner | `cmd-alt-p` |
| `thock::ToggleAgentFocus` | The terminal agent panel | `cmd-alt-t` |
| `thock::ToggleChatFocus` | The Thock Agent chat | `cmd-alt-n` |
| `thock::OpenToday` / `OpenYesterday` / `OpenTomorrow` | A daily note | unbound |
| `thock::OpenThisWeek` / `OpenLastWeek` | A weekly note | unbound |
| `thock::OpenInbox` / `thock::OpenGuide` / `thock::OpenCustomize` | Those pages | unbound |
| `thock::ToggleMarkdownSource` | Show the raw `#` and `[ ]` | `cmd-alt-m` |

**Rituals** (all unbound by default; all live in `Workspace`):
`thock::NewConversation`, `thock::ConnectAgent`, `thock::NewRoutine`,
`thock::SetProfile`, `thock::SetLanguage`, `thock::UpdateRituals`,
`thock::Reflect`, `thock::RebuildMemory`, `thock::CustomizeApp`, plus
`["thock::RunSkill", { "skill": "…" }]` for a Routine's own rituals.

**Inside a panel** — these only work under that panel's context:
`thock::AddBacklogTask`, `CompleteBacklogTask`, `MoveBacklogTaskLeft` /
`Right`, `CopyBacklogTask`, `RevealBacklogTask`, `EditBacklogTask`,
`CollapseBacklogCategory` / `ExpandBacklogCategory`,
`SelectNextBacklogColumn` / `SelectPreviousBacklogColumn`;
`thock::ViewSkill`, `CollapseGroup`, `ExpandGroup`;
`thock::FocusChatInput`, `OpenChatEntry`, `SendChatMessage`,
`StopChatTurn`, `RetryChatTurn`, `ExpandChatActivity`,
`CollapseChatActivity`, `NewChat`.

For a Zed action outside the `thock` namespace, the reliable way to find its
name is **zed: open default keymap** in the command bar (`⌘ ⇧ P`) — it lists
every shipped binding. `zed::OpenKeymap` (`⌘ K ⌘ S`) opens a searchable
editor of every action the app has, which is the better answer when the user
wants to browse rather than have you write a line.

### Which binding wins

1. The **deeper context** wins: a binding on `Editor` beats one on
   `Workspace`. A block with no context sits at the bottom of the tree.
2. At the same depth, the binding **defined later** wins — and the user's
   keymap loads after the shipped one, so a same-context rebind in
   `keymap.json` always takes over.

So: to take a key away from a Thock default, rebind it **in the same
context** the default uses (the table above tells you which). Binding it in
a shallower context will lose. When you genuinely need it dead, `null` it in
the context that owns it.

If one binding is a prefix of another (`cmd-w` and `cmd-w left`), the app
waits a second after the prefix to see what comes next — which makes the
shorter one feel laggy. Avoid creating that pair unless the user asks for it.

## The ritual

When launched from the command bar with nothing specific asked:

1. **Ask what they'd like to change** — one question, in plain words. Offer
   the three that come up most: the look (theme, text size), the keys, or
   something they already have in mind.
2. **Read the file** you're about to touch.
3. **Say what you'll write, in one line**, then write it. This is not a
   confirm-every-step ritual — the change is a two-second undo away and they
   can see it happen. Confirm first only when you're about to change a key
   that already does something, or a setting they didn't ask about.
4. **Tell them what to look at** to see it worked, and what to say if they
   want it dialed further ("say bigger again and I'll bump it").

When the user just asks mid-conversation ("make the text bigger", "put the
backlog on cmd-alt-b"), skip step 1 and do it.

## If something goes wrong

- **The change didn't take.** In order: the file didn't parse (a missing
  comma or brace — read it back), the key is misspelled (unknown keys are
  ignored silently), or for a binding, a deeper context is already claiming
  that key. Say which one you found.
- **The shortcut does the old thing.** Its default lives in a deeper context
  than the one you wrote. Move your block to that context.
- **A theme or font name isn't recognized.** Thock falls back to the default
  without complaining. Check the bundled theme list above, and
  `~/.config/thock/themes/`; for fonts, only what's installed on the machine
  will work.
- **You can't find the config folder.** The user may run a custom data
  directory. Ask them to run **zed: open settings file** in the command bar
  and tell you the path of the tab that opens.

Never "tidy" one of these files — no reformatting, no reordering, no
removing a comment or a setting you think is stale. The one thing the user
loses in a customize session and never forgives is the binding they added
themselves last year.
