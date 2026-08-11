#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "${script_dir}/.." && pwd)"
build_dir="${ARTIFICER_BUILD_DIR:-${workspace_dir}/artifacts/standalone}"
target_dir="${build_dir}/cargo-target"
release_dir="${build_dir}/release"

mkdir -p "${release_dir}/bin"

echo "Building Artificer release executable..."
CARGO_TARGET_DIR="${target_dir}" \
    cargo build \
    --manifest-path "${workspace_dir}/Cargo.toml" \
    --locked \
    --release \
    --package artificer-workbench \
    --bin artificer-workbench

case "$(uname -s)" in
    Darwin)
        native_binary="${target_dir}/release/artificer-workbench"
        raw_executable="${release_dir}/bin/Artificer"
        app_bundle="${release_dir}/Artificer.app"

        install -m 0755 "${native_binary}" "${raw_executable}"
        mkdir -p "${app_bundle}/Contents/MacOS" "${app_bundle}/Contents/Resources"
        install -m 0755 "${native_binary}" "${app_bundle}/Contents/MacOS/Artificer"
        install -m 0644 \
            "${workspace_dir}/packaging/macos/Info.plist" \
            "${app_bundle}/Contents/Info.plist"

        if command -v codesign >/dev/null 2>&1; then
            codesign --force --deep --sign - "${app_bundle}" >/dev/null
        fi

        echo "Standalone app: ${app_bundle}"
        echo "Raw executable: ${raw_executable}"
        ;;
    MINGW*|MSYS*|CYGWIN*)
        native_binary="${target_dir}/release/artificer-workbench.exe"
        raw_executable="${release_dir}/bin/Artificer.exe"
        install -m 0755 "${native_binary}" "${raw_executable}"
        echo "Standalone executable: ${raw_executable}"
        ;;
    *)
        native_binary="${target_dir}/release/artificer-workbench"
        raw_executable="${release_dir}/bin/Artificer"
        install -m 0755 "${native_binary}" "${raw_executable}"
        echo "Standalone executable: ${raw_executable}"
        ;;
esac
