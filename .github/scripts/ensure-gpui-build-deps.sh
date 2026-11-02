#!/usr/bin/env bash
# Ensure the native libraries required by the unconditionally built GPUI
# desktop window are available on Linux build hosts.
#
# macOS and Windows use their platform frameworks; this script is a no-op
# there. On Linux it verifies the pkg-config entries and installs the
# development packages when a supported package manager is available.
set -euo pipefail

if [ "$(uname -s)" != "Linux" ]; then
  echo "GPUI native libraries: not Linux, nothing to install"
  exit 0
fi

need=()
if ! command -v pkg-config >/dev/null 2>&1; then
  need+=(pkg-config)
else
  for pkg in fontconfig xkbcommon xkbcommon-x11; do
    if ! pkg-config --exists "$pkg" 2>/dev/null; then
      need+=("$pkg")
    fi
  done
fi
if [ "${#need[@]}" -eq 0 ]; then
  echo "GPUI native libraries: fontconfig, xkbcommon, xkbcommon-x11 present"
  exit 0
fi

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "::error::GPUI native libraries are missing and sudo is unavailable: ${need[*]}" >&2
    exit 1
  fi
  SUDO="sudo -n"
fi

if command -v apt-get >/dev/null 2>&1; then
  $SUDO apt-get update
  $SUDO apt-get install -y pkg-config libfontconfig1-dev libxkbcommon-dev libxkbcommon-x11-dev
elif command -v dnf >/dev/null 2>&1; then
  $SUDO dnf install -y pkgconf-pkg-config fontconfig-devel libxkbcommon-devel libxkbcommon-x11-devel
elif command -v yum >/dev/null 2>&1; then
  $SUDO yum install -y pkgconf-pkg-config fontconfig-devel libxkbcommon-devel libxkbcommon-x11-devel
else
  echo "::error::GPUI native libraries are missing (${need[*]}) and no supported package manager is available" >&2
  exit 1
fi

for pkg in fontconfig xkbcommon xkbcommon-x11; do
  if ! pkg-config --exists "$pkg"; then
    echo "::error::pkg-config still cannot find $pkg after installation" >&2
    exit 1
  fi
done
