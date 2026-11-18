#!/bin/sh
# Keeps the build folder small (it once grew past 100 GB):
#  1. incremental caches nobody used for 7 days are deleted (always safe);
#  2. if a profile's folder is still over the limit (30 GB), it is cleared
#     whole — the next build makes it again from scratch.
# Usage: scripts/sweep.sh [days] [limit-in-GB]
cd "$(dirname "$0")/.." || exit 0
target="${CARGO_TARGET_DIR:-target}"
[ -d "$target" ] || exit 0
days="${1:-7}"
limit_gb="${2:-30}"
for profile in "$target"/debug "$target"/release; do
  [ -d "$profile" ] || continue
  if [ -d "$profile/incremental" ]; then
    find "$profile/incremental" -mindepth 1 -maxdepth 1 -mtime "+$days" -exec rm -rf {} + 2>/dev/null
  fi
  kb=$(du -sk "$profile" 2>/dev/null | cut -f1)
  if [ -n "$kb" ] && [ "$kb" -gt $((limit_gb * 1024 * 1024)) ]; then
    echo "sweep: $profile is over ${limit_gb} GB, clearing it"
    rm -rf "$profile"
  fi
done
du -sh "$target" 2>/dev/null | awk '{print "build folder: " $1}'
