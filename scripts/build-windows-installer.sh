#!/bin/bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Cross-compile the Windows binaries from macOS/Linux and pack the NSIS
# installer (scripts/windows/hydra-installer.nsi).
#
#   scripts/build-windows-installer.sh [x64|arm64]      (default: x64)
#   scripts/build-windows-installer.sh --no-build ...   (pack only)
#
# `amd64` is accepted as an alias for x64 (same x86_64-pc-windows-msvc
# target); the installer is always named -x64- for a single canonical
# artifact name.
#
# Needs: cargo-xwin, Homebrew LLVM (lld-link), makensis (brew install makensis),
# and python3 (or python) for packing the browser extensions.
#
# --cross-compiler clang (not the clang-cl default) is required: ring's
# build.rs force-switches to plain clang on windows-aarch64, which rejects
# the /imsvc flags clang-cl emits. .cargo/config.toml already pins
# +crt-static so the exes need no VC++ Redistributable on the target.
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH="x64"
NO_BUILD=0
for arg in "$@"; do
  case "$arg" in
    x64|amd64) ARCH="x64" ;;
    arm64) ARCH="arm64" ;;
    --no-build) NO_BUILD=1 ;;
    *) echo "usage: $0 [x64|amd64|arm64] [--no-build]" >&2; exit 2 ;;
  esac
done

case "$ARCH" in
  arm64) TARGET="aarch64-pc-windows-msvc" ;;
  x64)   TARGET="x86_64-pc-windows-msvc" ;;
esac

# The workspace product version ([workspace.package]), shared by the
# hydra-gui, hydra-cli and hydra-host bin crates this installer bundles.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

python3 scripts/plugins/stage-official.py --version "$VERSION"
export HYDRA_OFFICIAL_PLUGIN_DIR="$(pwd)/plugins/bundled"

if [ "$NO_BUILD" = 0 ]; then
  echo "building hydra-gui + hydra-host + hydra (cli) for $TARGET..."
  # cargo-xwin puts its sysroot -L in CFLAGS, which clang flags as unused when
  # embed-resource runs it as the .rc preprocessor.
  PATH="/opt/homebrew/opt/llvm/bin:$PATH" \
    TARGET_CFLAGS="-Wno-unused-command-line-argument" \
    cargo xwin build --release --target "$TARGET" --cross-compiler clang \
      -p hya-gui -p hya-host -p hya-cli
fi

for bin in hydra-gui.exe hydra-host.exe hydra.exe; do
  [ -f "target/$TARGET/release/$bin" ] || {
    echo "missing target/$TARGET/release/$bin (build failed or --no-build without a prior build?)" >&2
    exit 1
  }
done

command -v makensis >/dev/null || {
  echo "makensis not found - install with: brew install makensis" >&2
  exit 1
}

# The installer packs target/extensions (packed .zip/.xpi + the unpacked
# directories). Built here rather than inside the .nsi because makensis
# cannot zip, and --no-build only skips cargo -- the extensions are web
# sources and cost nothing to repack.
echo "packing browser extensions..."
./scripts/build-extensions.sh --quiet

# The extension carries its own version (extensions/*/manifest.json) and bumps
# on its own cadence, so it is usually BEHIND the product VERSION above. The
# .nsi needs it to spell the packed filenames in INSTALL.txt; read it back off
# the archive build-extensions.sh just produced rather than re-deriving it from
# the manifest, so the names in the text are the ones actually packed.
EXT_ZIP=""
for f in target/extensions/hydra-chrome-*.zip; do
  # --store also emits hydra-chrome-<v>-webstore.zip; not built here, but the
  # .nsi packs by the same glob and would be just as ambiguous, so skip it.
  case "$f" in *-webstore.zip) continue ;; esac
  [ -f "$f" ] || continue
  [ -z "$EXT_ZIP" ] || {
    echo "ambiguous: more than one target/extensions/hydra-chrome-*.zip" >&2
    exit 1
  }
  EXT_ZIP=$f
done
[ -n "$EXT_ZIP" ] || {
  echo "missing target/extensions/hydra-chrome-*.zip (extension build failed?)" >&2
  exit 1
}
EXT_VERSION=${EXT_ZIP##*/hydra-chrome-}
EXT_VERSION=${EXT_VERSION%.zip}

# makensis resolves File paths against the cwd, and the .nsi is written
# relative to its own directory.
cd scripts/windows
# Without a UTF-8 locale, Unicode makensis on macOS dies with std::bad_alloc
# while writing the output (NSIS bug #1165); non-login shells (CI, editors)
# often have no LC_ALL set.
LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
  makensis -DARCH="$ARCH" -DVERSION="$VERSION" \
    -DNUM_VERSION="${VERSION%%-*}" -DEXT_VERSION="$EXT_VERSION" \
    hydra-installer.nsi
echo "Built: target/hydra-$VERSION-windows-$ARCH-setup.exe"
