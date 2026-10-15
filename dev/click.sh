#!/usr/bin/env bash
# Move/click the pointer inside the Darkmatter window, ONLY if it's focused.
# On a tiling desktop the window's tile moves, and focus follows the mouse,
# so the position is read fresh and the point must fall inside the tile.
#   dev/click.sh <pid> <x> <y> [move|click|right|scroll <steps>|drag <x2> <y2>]
# x/y are logical pixels from the window's top-left (screenshots from
# `grim -g` at scale 2 are twice that: halve their coordinates).
set -euo pipefail
pid=$1 x=$2 y=$3; shift 3
bin=$(dirname "$0")/pointer/target/release/vpointer
[[ -x $bin ]] || cargo build --release --manifest-path "$(dirname "$0")/pointer/Cargo.toml" -q

win=$(hyprctl activewindow -j)
if [[ $(jq -r .pid <<<"$win") != "$pid" ]]; then
  echo "refusing: focused window is '$(jq -r .title <<<"$win")', not pid $pid" >&2
  exit 1
fi
read -r wx wy ww wh < <(jq -r '"\(.at[0]) \(.at[1]) \(.size[0]) \(.size[1])"' <<<"$win")
if (( x < 0 || y < 0 || x >= ww || y >= wh )); then
  echo "refusing: ($x,$y) is outside the ${ww}x${wh} window" >&2
  exit 1
fi
# Floating windows (dialogs, popups) can sit above the tile: the point must not be under one.
ws=$(jq -r .workspace.id <<<"$win")
ax=$((wx + x)) ay=$((wy + y))
cover=$(hyprctl clients -j | jq -r --argjson ws "$ws" --argjson pid "$pid" --argjson x $ax --argjson y $ay \
  '[.[] | select(.workspace.id == $ws and .pid != $pid and .floating and .at[0] <= $x and $x < .at[0]+.size[0] and .at[1] <= $y and $y < .at[1]+.size[1])] | length')
if (( cover > 0 )); then
  echo "refusing: another window covers ($x,$y)" >&2
  exit 1
fi
# The virtual pointer's absolute coordinates span the whole monitor layout.
read -r lw lh < <(hyprctl monitors -j | jq -r '"\(map(.x + (.width/.scale|floor)) | max) \(map(.y + (.height/.scale|floor)) | max)"')
if [[ ${1:-} == drag ]]; then
  x2=$2 y2=$3
  if (( x2 < 0 || y2 < 0 || x2 >= ww || y2 >= wh )); then
    echo "refusing: drag end ($x2,$y2) is outside the window" >&2
    exit 1
  fi
  set -- drag $((wx + x2)) $((wy + y2))
fi
"$bin" "$ax" "$ay" "$lw" "$lh" "${@:-move}"
