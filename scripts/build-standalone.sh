#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "${script_dir}/.." && pwd)"
build_dir="${ARTIFICER_BUILD_DIR:-${workspace_dir}/artifacts/standalone}"
# A private target directory by default, so a delivery build cannot be served
# stale artefacts from a development one. CI overrides it to reuse the cache it
# already warmed, which is the one place the two builds are known to agree.
target_dir="${ARTIFICER_TARGET_DIR:-${build_dir}/cargo-target}"
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
        rm -rf "${app_bundle}"
        mkdir -p "${app_bundle}/Contents/MacOS" "${app_bundle}/Contents/Resources"
        install -m 0755 "${native_binary}" "${app_bundle}/Contents/MacOS/Artificer"
        install -m 0644 \
            "${workspace_dir}/packaging/macos/Info.plist" \
            "${app_bundle}/Contents/Info.plist"
        install -m 0644 \
            "${workspace_dir}/packaging/icons/artificer.icns" \
            "${app_bundle}/Contents/Resources/artificer.icns"

        # The bundle's version is the crate's version, written here rather than
        # maintained by hand in the plist: the committed template drifted from
        # the workspace by two releases the last time it was a manual step.
        crate_version="$(cargo pkgid --manifest-path "${workspace_dir}/Cargo.toml" \
            --package artificer-workbench | sed 's/.*[@#]//')"
        /usr/libexec/PlistBuddy \
            -c "Set :CFBundleShortVersionString ${crate_version}" \
            -c "Set :CFBundleVersion ${crate_version}" \
            "${app_bundle}/Contents/Info.plist" >/dev/null

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
