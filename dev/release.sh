#!/usr/bin/env bash
# Cut a release locally: set the version, turn CHANGELOG's "Unreleased" into
# that version, commit and tag. Pushing the tag (git push --follow-tags)
# starts the release workflow.
#   dev/release.sh 0.2.0
set -euo pipefail
cd "$(dirname "$0")/.."
version=${1:?usage: dev/release.sh <version>}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "not a version: $version" >&2; exit 1; }
[[ -z $(git status --porcelain) ]] || { echo "commit or stash your changes first" >&2; exit 1; }
cargo=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")

sed -i "0,/^version = \".*\"/s//version = \"$version\"/" Cargo.toml
"$cargo" update --workspace -q
today=$(date +%Y-%m-%d)
if grep -q "^## \[$version\]$" CHANGELOG.md; then
  # Already written up (like the first release): just date it.
  sed -i "s/^## \[$version\]$/## [$version] - $today/" CHANGELOG.md
elif ! grep -q "^## \[$version\]" CHANGELOG.md; then
  # Unreleased becomes this version, and a fresh Unreleased goes on top.
  sed -i "s/^## \[Unreleased\]/## [Unreleased]\n\n## [$version] - $today/" CHANGELOG.md
fi
packaging/release-notes.sh "$version" >/dev/null
"$cargo" test --workspace -q --locked

git diff --quiet || git commit -qam "Release v$version"
git tag -a "v$version" -m "Darkmatter $version"
echo "Tagged v$version. Push it to release: git push --follow-tags"
