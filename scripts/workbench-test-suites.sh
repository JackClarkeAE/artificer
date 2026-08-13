#!/usr/bin/env bash
# Enumerates the workbench's integration-test targets as `--test NAME` flags.
#
# CI used to name each suite by hand, which meant a newly added suite was in CI
# only if someone remembered to add it — and several were not. Deriving the
# list from the tree inverts that default: a suite is in CI the moment its file
# exists, and the only split is the one the runner genuinely needs, between the
# logic suites (any runner) and the pixel suites (the software-rasteriser job
# with the recorded baselines).
#
# The split is the `*visual` filename convention. A suite named for pixels runs
# in the pixel job; everything else runs in the logic job. Nothing can fall
# through both.
set -euo pipefail

selection="${1:-all}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
suites=()

for path in "$root"/apps/workbench/tests/*.rs; do
    [ -e "$path" ] || continue
    name="$(basename "$path" .rs)"
    case "$selection" in
        logic) [[ "$name" == *visual ]] && continue ;;
        pixels) [[ "$name" == *visual ]] || continue ;;
        all) ;;
        *)
            printf 'usage: %s [logic|pixels|all]\n' "$(basename "$0")" >&2
            exit 2
            ;;
    esac
    suites+=("--test" "$name")
done

if [ "${#suites[@]}" -eq 0 ]; then
    printf 'error: no %s workbench test suites found\n' "$selection" >&2
    exit 1
fi

printf '%s\n' "${suites[*]}"
