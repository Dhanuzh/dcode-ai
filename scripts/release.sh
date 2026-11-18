#!/bin/sh
# One step from a finished change to a release:
#   scripts/release.sh 1.0.82 "what this release does"
# Bumps the workspace version, the dcode-ai crates in Cargo.lock (no build
# needed), checks CHANGELOG.md has a "## 1.0.82" section, commits, tags and
# pushes — CI builds, tests and publishes from the tag.
# Set NO_VERIFY=1 to skip the local pre-commit checks (they compile).
set -e
cd "$(dirname "$0")/.."
version="$1"
summary="$2"
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "usage: scripts/release.sh <x.y.z> \"summary\"" >&2; exit 2 ;;
esac
[ -n "$summary" ] || { echo "give a one-line summary" >&2; exit 2; }
if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
  echo "v$version already exists" >&2
  exit 1
fi
grep -q "^## $version " CHANGELOG.md || {
  echo "CHANGELOG.md has no \"## $version — …\" section yet" >&2
  exit 1
}
old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# The workspace version (the first `version =` in Cargo.toml).
awk -v new="$version" '!done && /^version = "/ { sub(/"[^"]*"/, "\"" new "\""); done = 1 } { print }' \
  Cargo.toml > Cargo.toml.new && mv Cargo.toml.new Cargo.toml
# The dcode-ai crates' entries in Cargo.lock.
awk -v old="$old" -v new="$version" '
  /^name = "dcode-ai/ { ours = 1; print; next }
  ours && /^version = / { sub("\"" old "\"", "\"" new "\""); ours = 0 }
  /^name = / { ours = 0 }
  { print }' Cargo.lock > Cargo.lock.new && mv Cargo.lock.new Cargo.lock
echo "version $old → $version"

git add Cargo.toml Cargo.lock CHANGELOG.md
if [ "${NO_VERIFY:-0}" = 1 ]; then
  git commit -q --no-verify -m "release: v$version — $summary"
else
  git commit -q -m "release: v$version — $summary"
fi
git fetch -q origin
git rebase -q origin/main
git tag "v$version"
git push -q origin main
git push -q origin "v$version"
echo "pushed v$version — CI builds and publishes it"
