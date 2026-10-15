#!/usr/bin/env bash
# Send keystrokes via wtype, but ONLY if the intended window is focused.
# wtype types into whatever has focus, so never skip this check.
#   dev/drive.sh <pid> <wtype args...>             target the mm-rust window by pid
#   dev/drive.sh title=<exact title> <wtype args>  target e.g. a file dialog
set -euo pipefail
target=$1; shift
win=$(hyprctl activewindow -j)
if [[ $target == title=* ]]; then
  want=${target#title=}; got=$(jq -r .title <<<"$win")
else
  want=$target; got=$(jq -r .pid <<<"$win")
fi
if [[ $got != "$want" ]]; then
  echo "refusing: focused window is '$got', not '$want'" >&2
  exit 1
fi
wtype "$@"
