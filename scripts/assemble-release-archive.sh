#!/usr/bin/env bash
#
# assemble-release-archive.sh — build the binary release artifact (plan U11,
# R19/KD1): ad-hoc codesign the release-built omt-daemon + omt binaries, pack
# them with README.md into a versioned tar.gz, and emit SHA256SUMS beside it.
#
# Archive layout contract (single source of truth for U12 install.sh / brew):
#   archive name : omt-<triple>-v<version>.tar.gz
#   extracts to  : one top-level folder omt-<triple>-v<version>/ containing
#                    omt-daemon    daemon binary (ad-hoc signed before packing)
#                    omt           operator CLI binary (ad-hoc signed)
#                    README.md     copy of the repository root README.md
#
# Version source of truth: [workspace.package] version in the root Cargo.toml
# (KTD1). Release tags follow v<version>; .github/workflows/release.yml fails
# a pushed tag whose name does not match.
#
# Supply-chain hygiene (KTD8): this script performs no network access at all
# (no curl|bash anywhere); SHA256SUMS is always emitted alongside the archive.
# Third-party action pinning policy is documented in
# .github/workflows/release.yml.
#
# Usage:
#   scripts/assemble-release-archive.sh [--target TRIPLE] [--bin-dir DIR]
#                                       [--out-dir DIR] [--print-version]
#
#   --target TRIPLE  Rust target triple baked into names (default
#                    aarch64-apple-darwin).
#   --bin-dir DIR    Directory holding built omt-daemon + omt. Default:
#                    target/<triple>/release when both binaries exist there,
#                    else the host-build fallback target/release.
#   --out-dir DIR    Where archive + SHA256SUMS land. Default:
#                    target/release-dist (kept out of git via target/).
#   --print-version  Print the workspace product version and exit.

set -euo pipefail

usage() {
  sed -n '2,40p' "${BASH_SOURCE[0]}" | grep -E '^#' | sed 's/^# \{0,1\}//'
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="aarch64-apple-darwin"
BIN_DIR=""
OUT_DIR=""
PRINT_VERSION=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target) TARGET="${2:?--target requires a value}"; shift 2 ;;
    --bin-dir) BIN_DIR="${2:?--bin-dir requires a value}"; shift 2 ;;
    --out-dir) OUT_DIR="${2:?--out-dir requires a value}"; shift 2 ;;
    --print-version) PRINT_VERSION=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "assemble-release-archive: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

workspace_version() {
  local v
  v="$(awk '
    /^\[workspace\.package\]/ { ws = 1; next }
    /^\[/                     { ws = 0 }
    ws && $1 == "version"     { v = $3; gsub(/"/, "", v); print v; exit }
  ' "$REPO_ROOT/Cargo.toml")"
  if [ -z "$v" ]; then
    echo "assemble-release-archive: could not read [workspace.package] version from $REPO_ROOT/Cargo.toml" >&2
    return 1
  fi
  printf '%s\n' "$v"
}

VERSION="$(workspace_version)"

if [ "$PRINT_VERSION" = "1" ]; then
  printf '%s\n' "$VERSION"
  exit 0
fi

if [ -z "$BIN_DIR" ]; then
  if [ -x "$REPO_ROOT/target/$TARGET/release/omt-daemon" ] && \
     [ -x "$REPO_ROOT/target/$TARGET/release/omt" ]; then
    BIN_DIR="$REPO_ROOT/target/$TARGET/release"
  else
    BIN_DIR="$REPO_ROOT/target/release"
  fi
fi
OUT_DIR="${OUT_DIR:-$REPO_ROOT/target/release-dist}"
mkdir -p "$OUT_DIR"

for bin in omt-daemon omt; do
  if [ ! -x "$BIN_DIR/$bin" ]; then
    echo "assemble-release-archive: missing executable $BIN_DIR/$bin — build first:" \
      "cargo build --release --target $TARGET -p omt-runtime --bins" >&2
    exit 1
  fi
done
if [ ! -f "$REPO_ROOT/README.md" ]; then
  echo "assemble-release-archive: repository root README.md not found" >&2
  exit 1
fi
if ! command -v codesign >/dev/null 2>&1; then
  echo "assemble-release-archive: codesign not found — macOS release packaging requires it" >&2
  exit 1
fi

ARCHIVE="omt-${TARGET}-v${VERSION}.tar.gz"
FOLDER="omt-${TARGET}-v${VERSION}"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/omt-release.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/$FOLDER"
cp "$BIN_DIR/omt-daemon" "$BIN_DIR/omt" "$STAGE/$FOLDER/"
cp "$REPO_ROOT/README.md" "$STAGE/$FOLDER/"

echo "[assemble] ad-hoc codesigning binaries ..."
for bin in omt-daemon omt; do
  codesign --sign - --force "$STAGE/$FOLDER/$bin"
  codesign --verify --strict "$STAGE/$FOLDER/$bin"
done

echo "[assemble] packing $ARCHIVE ..."
tar -czf "$OUT_DIR/$ARCHIVE" -C "$STAGE" "$FOLDER"

sha256_file() {
  # shasum on macOS runners/hosts, sha256sum elsewhere; same digest either way.
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "assemble-release-archive: need shasum or sha256sum for checksums" >&2
    return 1
  fi
}

HASH="$(sha256_file "$OUT_DIR/$ARCHIVE")"
printf '%s  %s\n' "$HASH" "$ARCHIVE" > "$OUT_DIR/SHA256SUMS"

echo "[assemble] version : $VERSION"
echo "[assemble] archive : $OUT_DIR/$ARCHIVE"
echo "[assemble] sums    : $OUT_DIR/SHA256SUMS ($HASH)"
