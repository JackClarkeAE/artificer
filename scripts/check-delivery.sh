#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "${script_dir}/.." && pwd)"

cd "${workspace_dir}"

echo "Checking Rust formatting..."
cargo fmt --all -- --check

echo "Running the complete workspace test suite..."
cargo test --locked --workspace --all-targets

echo "Checking all workspace targets with warnings denied..."
cargo clippy --locked --workspace --all-targets -- -D warnings

echo "Building warning-free workspace documentation..."
RUSTDOCFLAGS="-Dwarnings" cargo doc --locked --workspace --no-deps

echo "Checking architecture and native-kernel boundaries..."
"${script_dir}/check-architecture-boundaries.sh"

echo "Checking release-profile sketch frame budgets..."
"${script_dir}/check-sketch-frame-budget.sh"

git diff --check

echo "Creating the independently runnable delivery build..."
"${script_dir}/build-standalone.sh"

echo "Artificer delivery verification and packaging completed."
