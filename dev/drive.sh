#!/usr/bin/env bash
# Send keystrokes to the running mm-rust window via wtype, but ONLY if it is
# the focused window. wtype types into whatever has focus, so never skip this.
#   dev/drive.sh <pid> <wtype args...>
set -euo pipefail
pid=$1; shift
active=$(hyprctl activewindow -j | jq -r .pid)
if [[ $active != "$pid" ]]; then
  echo "refusing: focused window is pid $active, not mm-rust ($pid)" >&2
  exit 1
fi
wtype "$@"
