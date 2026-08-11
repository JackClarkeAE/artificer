#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

echo "Artificer sketch CPU frame-budget evidence"
rustc -Vv
if command -v sw_vers >/dev/null 2>&1; then
  sw_vers
fi
if command -v sysctl >/dev/null 2>&1; then
  sysctl -n machdep.cpu.brand_string 2>/dev/null || true
fi
if command -v pmset >/dev/null 2>&1; then
  pmset -g batt 2>/dev/null || true
fi

ARTIFICER_PERF_REPORT=1 cargo test --release -p artificer-workbench --test frame_budget -- --nocapture
