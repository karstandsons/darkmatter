#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md (the release notes):
#   packaging/release-notes.sh 0.1.0
set -euo pipefail
version=$1
notes=$(awk -v v="$version" '
  /^## \[/ { if (on) exit; on = index($0, "## [" v "]") == 1; next }
  on' "$(dirname "$0")/../CHANGELOG.md")
if [[ -z ${notes//[[:space:]]/} ]]; then
  echo "CHANGELOG.md has no section for $version" >&2
  exit 1
fi
printf '%s\n' "$notes"
