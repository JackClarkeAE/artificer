#!/usr/bin/env bash
# Deletes stale object files from the dev build directory.
#
# The dev and test profiles use `split-debuginfo = "unpacked"`, which leaves
# each build's `.o` files in `target/debug/deps` so a relink can point at them
# instead of copying debug info into two dozen test executables. Nothing ever
# removes the previous build's objects, so the directory grows by every edit —
# it reached 1.3 million files and 93 GB here. On macOS that is not just disk:
# Gatekeeper assesses a freshly built executable together with the directory
# it sits in before letting it start, and against a directory that size every
# new test binary spent 30 s to 3 min at 0 % CPU in `_dyld_start`. A full test
# run took 40 minutes of which the tests themselves were a few.
#
# Objects older than a day belong to binaries that have since been rebuilt;
# only a debugger attached to one of those old binaries could miss them.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
deps="${CARGO_TARGET_DIR:-$root/target}/debug/deps"
age_days="${1:-1}"

if [ ! -d "$deps" ]; then
  echo "nothing to prune: $deps does not exist"
  exit 0
fi

before=$(find "$deps" -name '*.o' | wc -l | tr -d ' ')
find "$deps" -name '*.o' -mtime +"$age_days" -delete
after=$(find "$deps" -name '*.o' | wc -l | tr -d ' ')
echo "pruned $((before - after)) object files older than $age_days day(s); $after remain in $deps"
