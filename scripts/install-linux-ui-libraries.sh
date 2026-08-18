#!/usr/bin/env bash
# Installs the native UI build libraries on an Ubuntu CI runner. Extra
# package names may follow (the native job adds ripgrep).
#
# `apt-get` against a degraded mirror does not fail — it hangs. On 2026-08-18
# the Linux packaging job of the v0.4.0 release sat forty minutes on
# `apt-get update`, two minutes after the Azure mirror had timed out onto
# archive.ubuntu.com, while the two other Ubuntu jobs of the same run got
# through the same line in seconds; its re-run on a fresh runner did exactly
# the same. So every transfer carries a timeout that turns a stall into a
# failure, apt retries the transfer, and a failed round gets two more rounds,
# each of which may land on a healthier mirror.
set -uo pipefail

packages=(
    libegl1-mesa-dev libgl1-mesa-dri libvulkan1 mesa-vulkan-drivers
    libwayland-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev
    libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
    "$@"
)
apt_options=(
    -o Acquire::Retries=3
    -o Acquire::http::Timeout=30
    -o Acquire::https::Timeout=30
)

for attempt in 1 2 3; do
    if sudo apt-get "${apt_options[@]}" update \
        && sudo apt-get "${apt_options[@]}" install --yes "${packages[@]}"; then
        exit 0
    fi
    printf 'apt round %s failed; retrying in 20 s\n' "$attempt" >&2
    sleep 20
done

printf 'error: the UI build libraries could not be installed in three rounds\n' >&2
exit 1
