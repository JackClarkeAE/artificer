#!/usr/bin/env bash
# Builds the workbench and packages it as a Velopack release for this platform.
#
# Windows gets a `Setup.exe` and a per-user install; Linux gets an AppImage.
# Both carry the update feed that `apps/workbench/src/update.rs` reads from the
# GitHub releases page, so a build produced any other way — including
# `build-standalone.sh` — cannot update itself and says so in About.
#
# macOS is deliberately not packaged here. Velopack requires a signed and
# notarised bundle on that platform, and until the Apple Developer certificates
# exist the macOS asset stays the plain `.app` zip that `build-standalone.sh`
# produces. Adding macOS later is additive: a new `osx` channel, no migration
# for anyone already installed.
#
#     ./scripts/pack-release.sh [version]
#
# The version defaults to the workspace version and must be semver2 without a
# leading `v`. CI passes the tag it is releasing, having first checked that the
# two agree.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "${script_dir}/.." && pwd)"

crate_version="$(cargo pkgid --manifest-path "${workspace_dir}/Cargo.toml" \
    --package artificer-workbench | sed 's/.*[@#]//')"
version="${1:-${crate_version}}"
version="${version#v}"
if printf '%s' "${version}" | grep -Eq '^[0-9]+\.[0-9]+$'; then
    version="${version}.0"
fi

if ! printf '%s' "${version}" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+].+)?$'; then
    printf 'error: "%s" is not a semver2 version\n' "${version}" >&2
    exit 1
fi

output_dir="${ARTIFICER_RELEASE_DIR:-${workspace_dir}/artifacts/releases}"
publish_dir="${ARTIFICER_PUBLISH_DIR:-${workspace_dir}/artifacts/publish}"
icon_dir="${workspace_dir}/packaging/icons"

case "$(uname -s)" in
    Darwin)
        cat >&2 <<'MESSAGE'
error: macOS releases are not packaged with Velopack.

Velopack requires a signed and notarised bundle on macOS, which needs Apple
Developer certificates this project does not have yet. Build the unsigned
bundle that ships on the releases page instead:

    ./scripts/build-standalone.sh
MESSAGE
        exit 1
        ;;
    MINGW*|MSYS*|CYGWIN*)
        platform=windows
        binary="artificer-workbench.exe"
        main_exe="Artificer.exe"
        icon="${icon_dir}/artificer.ico"
        ;;
    *)
        platform=linux
        binary="artificer-workbench"
        main_exe="Artificer"
        icon="${icon_dir}/artificer.png"
        ;;
esac

# Checked after the platform, so a macOS developer is told what this script
# does not do rather than sent to install a tool it would not use.
if ! command -v vpk >/dev/null 2>&1; then
    cat >&2 <<'MESSAGE'
error: the `vpk` command line tool is not installed.

It is distributed as a .NET global tool. Install the .NET 8 SDK from
https://dotnet.microsoft.com/download/dotnet, then:

    dotnet tool install -g vpk
MESSAGE
    exit 1
fi

echo "Building Artificer ${version} for ${platform}..."
cargo build \
    --manifest-path "${workspace_dir}/Cargo.toml" \
    --locked \
    --release \
    --package artificer-workbench \
    --bin artificer-workbench

# A fresh staging directory every time: `vpk` packages the whole folder, so a
# file left behind by an earlier build would ship inside the release.
rm -rf "${publish_dir}"
mkdir -p "${publish_dir}" "${output_dir}"
rm -f "${output_dir}"/*"${version}"* "${output_dir}"/*"${version#v}"*
install -m 0755 "${workspace_dir}/target/release/${binary}" "${publish_dir}/${main_exe}"

# The pack id is permanent. It names the install directory, the update cache,
# and the channel feed, so changing it would orphan every existing install
# rather than update it.
pack_arguments=(
    --packId Artificer
    --packVersion "${version}"
    --packTitle Artificer
    --packAuthors "Jack Clarke"
    --packDir "${publish_dir}"
    --mainExe "${main_exe}"
    --icon "${icon}"
    --outputDir "${output_dir}"
)

if [ "${platform}" = linux ]; then
    # Where the AppImage files itself in a desktop menu.
    pack_arguments+=(--categories "Graphics;Engineering;")
fi

# Deltas are computed against whatever full package is already in the output
# directory, which is why CI runs `vpk download github --outputDir` into this
# same directory before packing. Locally the directory is usually empty and
# every release is a full one.
echo "Packing Velopack release ${version}..."
vpk pack "${pack_arguments[@]}"

echo "Release artifacts: ${output_dir}"
