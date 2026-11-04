---
name: mac-screenshot
description: >-
  Use when asked to take a screenshot of a running app on macOS (for example a
  Thock build started with `cargo run` or `./target/debug/thock`), given a PID
  or process name, to see what the UI currently looks like.
---

# Screenshot a running app on macOS

Thock is a native GPUI app, so browser tools can't see it. Capture its window with `screencapture` and
read the PNG back with the Read tool.

## Steps

1. Find the process if you only have a name or are unsure what a PID is:
   `ps -p <pid> -o pid,args` or `pgrep -fl thock`.
2. Capture its window:

   ```sh
   .agents/skills/mac-screenshot/screenshot.sh <pid|process-name> "$CLAUDE_JOB_DIR/tmp/thock.png"
   ```

   A name is matched exactly against the executable name (`thock`), newest process first. Pass the
   PID when several builds are running.

   Write to a scratch dir (`$CLAUDE_JOB_DIR/tmp` in background jobs, otherwise a temp dir), never into
   the repo or the vault.
3. Read the PNG with the Read tool to view it, then describe what's on screen.

The script resolves the PID's first normal (layer 0) on-screen window through CoreGraphics and runs
`screencapture -x -o -l <window-id>`: no shutter sound, no shadow, just that window even if others
overlap it.

## Traps

- **`could not create image from window` / `from display`** means the app hosting the shell lacks
  Screen Recording permission. Ask the user to enable it under System Settings → Privacy & Security →
  Screen & System Audio Recording and restart that app. Don't retry until they confirm.
- **No on-screen window**: the window is minimized or on another Space. Ask the user to bring it up;
  don't activate or move it yourself.
- Only capture. Don't drive the app with clicks or keystrokes; the user drives the GUI.
