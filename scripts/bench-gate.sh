#!/usr/bin/env bash
# Runs the kernel benches and compares them with the committed baselines.
#
# Optimisation without instruments is guesswork, but a benchmark gate tuned to
# noise is worse than none: it trains everyone to ignore it. Shared runners
# vary by a factor of two on a quiet day, so this gate fires only at 2x — it is
# there to catch an accidental quadratic or a lost early-out, not to chase
# percentages. The raw numbers are always printed, so a real slowdown is
# visible long before it trips the gate.
#
#   scripts/bench-gate.sh            compare against the committed baselines
#   scripts/bench-gate.sh --record   re-record them (state the machine in the
#                                    commit message; they are not portable)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
baseline="$root/docs/architecture/geometry-kernel/evidence/kernel-bench-baseline.json"
tolerance="${ARTIFICER_BENCH_TOLERANCE:-2.0}"
record=0
[ "${1:-}" = "--record" ] && record=1

cd "$root"
cargo bench --package artificer-kernel

python3 - "$baseline" "$record" "$tolerance" <<'PY'
import json, pathlib, sys

baseline_path, record, tolerance = pathlib.Path(sys.argv[1]), sys.argv[2] == "1", float(sys.argv[3])
measured = {}
for estimates in sorted(pathlib.Path("target/criterion").glob("**/new/estimates.json")):
    name = str(estimates.parent.parent.relative_to("target/criterion"))
    if name.endswith("/base") or name == "report":
        continue
    with estimates.open() as handle:
        measured[name] = json.load(handle)["mean"]["point_estimate"]

if not measured:
    sys.exit("error: no criterion estimates found; did the bench run?")

if record:
    import platform

    baseline_path.write_text(
        json.dumps(
            {
                "unit": "nanoseconds",
                "recorded_on": platform.platform(),
                "benches": measured,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    print(f"recorded {len(measured)} baselines to {baseline_path}")
    sys.exit(0)

if not baseline_path.exists():
    sys.exit(f"error: {baseline_path} is missing; run scripts/bench-gate.sh --record")

baseline = json.loads(baseline_path.read_text())
recorded = baseline["benches"]

# A timing baseline is a property of one machine. Comparing a shared Linux
# runner against numbers recorded on an arm64 laptop would fail on the hardware
# difference and teach everyone to ignore the gate, so a foreign machine gets
# the numbers printed and no verdict. Record a baseline on the runner to arm it
# there.
import platform

recorded_on = baseline.get("recorded_on")
same_machine = recorded_on is None or recorded_on == platform.platform()

failures, missing = [], []
for name, nanoseconds in sorted(measured.items()):
    if name not in recorded:
        missing.append(name)
        print(f"  {name}: {nanoseconds / 1000:.1f} us (no baseline)")
        continue
    ratio = nanoseconds / recorded[name]
    print(f"  {name}: {nanoseconds / 1000:.1f} us ({ratio:.2f}x baseline)")
    if ratio > tolerance:
        failures.append(f"{name} is {ratio:.2f}x its baseline")

for name in sorted(set(recorded) - set(measured)):
    print(f"  {name}: not measured this run")

if missing:
    print(f"note: {len(missing)} bench(es) have no baseline yet; re-record when they settle")
if not same_machine:
    print(
        f"note: baselines were recorded on {recorded_on}, this is {platform.platform()};\n"
        "      reporting timings only. Run scripts/bench-gate.sh --record here to arm the gate."
    )
    sys.exit(0)
if failures:
    sys.exit("bench gate failed:\n  " + "\n  ".join(failures))
print(f"bench gate passed: {len(measured)} benches within {tolerance}x")
PY
