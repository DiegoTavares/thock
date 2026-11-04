#!/usr/bin/env bash
# Capture the frontmost on-screen window of a process to a PNG on macOS.
# Usage: screenshot.sh <pid|process-name> <output.png>
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <pid|process-name> <output.png>" >&2
  exit 2
fi

target=$1
output=$2

if [[ $target =~ ^[0-9]+$ ]]; then
  pid=$target
else
  pid=$(pgrep -n -x "$target" || true)
  if [[ -z $pid ]]; then
    echo "no running process matches '$target'" >&2
    exit 1
  fi
fi

# Layer 0 windows are normal app windows; menus and overlays sit on higher layers.
window_id=$(swift - "$pid" <<'EOF'
import CoreGraphics
import Foundation
let pid = Int(CommandLine.arguments[1])!
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
if let window = windows.first(where: {
    ($0[kCGWindowOwnerPID as String] as? Int) == pid && ($0[kCGWindowLayer as String] as? Int) == 0
}) {
    print(window[kCGWindowNumber as String]!)
}
EOF
)

if [[ -z $window_id ]]; then
  echo "process $pid has no on-screen window (minimized, or on another Space?)" >&2
  exit 1
fi

if ! screencapture -x -o -l "$window_id" "$output"; then
  echo "screencapture failed: grant Screen Recording to the app hosting this shell" \
    "(System Settings > Privacy & Security > Screen & System Audio Recording), then restart it" >&2
  exit 1
fi

echo "$output"
