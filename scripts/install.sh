#!/bin/sh
#
# install.sh — one-command product channel installer (plan U12, R20/KD1).
# Thin wrapper over the GitHub Releases binary facts source: detect platform,
# download the release archive, verify it against SHA256SUMS (mandatory), and
# install BOTH binaries (omt-daemon + omt) into a user bin dir.
#
# Artifact contract consumed (single source of truth:
# scripts/assemble-release-archive.sh, U11):
#   asset   : omt-<triple>-v<version>.tar.gz
#   layout  : single top-level folder omt-<triple>-v<version>/ containing
#             omt-daemon, omt, README.md
#   sums    : sibling SHA256SUMS with relative-path entries
# Version source of truth: [workspace.package] version in root Cargo.toml
# (KTD1); release tags are v<version>.
#
# Supply-chain hygiene (KTD8):
#   - plain curl to fixed release URLs only; nothing downloaded is ever
#     eval'd, piped to a shell, or executed before verification;
#   - checksum verification is mandatory — there is no --no-verify escape;
#   - on any mismatch the script exits non-zero and installs nothing.
#
# Usage:
#   sh install.sh [--version V] [--bin-dir DIR] [--from-dir DIR] [--help]
#
#   --version V    Install a specific release, e.g. --version v0.2.0 or 0.2.0.
#                  Default: latest GitHub Release (via api.github.com).
#   --bin-dir DIR  Target directory for both binaries.
#                  Default: $OMT_INSTALL_DIR if set, else ~/.local/bin.
#                  Created only AFTER the archive verifies.
#   --from-dir DIR Offline mode: treat a local directory as the release root
#                  that already contains <archive>.tar.gz + SHA256SUMS (as
#                  produced by scripts/assemble-release-archive.sh). No
#                  network access; used for local verification.
#   --help         This text.

set -eu

ORG="rqwj"
REPO="oh-my-ticket"
REPO_SLUG="${ORG}/${REPO}"
RELEASES_URL="https://github.com/${REPO_SLUG}/releases"
API_LATEST_URL="https://api.github.com/repos/${REPO_SLUG}/releases/latest"
DEFAULT_BIN_DIR="${HOME:-$(pwd)}/.local/bin"

usage() {
  sed -n '2,36p' "$0" | grep -E '^#' | sed 's/^# \{0,1\}//'
}

die() {
  printf 'install.sh: error: %s\n' "$*" >&2
  exit 1
}

info() {
  printf '[install] %s\n' "$*"
}

warn() {
  printf '[install] warning: %s\n' "$*" >&2
}

VERSION=""
BIN_DIR=""
FROM_DIR=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value (e.g. --version v0.2.0)"
      VERSION="$2"; shift 2 ;;
    --bin-dir)
      [ "$#" -ge 2 ] || die "--bin-dir requires a value"
      BIN_DIR="$2"; shift 2 ;;
    --from-dir)
      [ "$#" -ge 2 ] || die "--from-dir requires a value"
      FROM_DIR="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

command -v curl >/dev/null 2>&1 || [ -n "$FROM_DIR" ] || \
  die "curl is required to download releases (or use --from-dir for offline install)"

# --- platform detection ------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) : ;;
  Linux)
    die "prebuilt Linux binaries are not published yet; see ${RELEASES_URL} — meanwhile build from source: cargo install --git https://github.com/${REPO_SLUG}" ;;
  *)
    die "unsupported operating system: ${os} (macOS only today); see ${RELEASES_URL}" ;;
esac
case "$arch" in
  arm64|aarch64) TRIPLE="aarch64-apple-darwin" ;;
  x86_64)
    TRIPLE="x86_64-apple-darwin"
    warn "x86_64-apple-darwin assets may not exist yet (first releases target" \
      "aarch64-apple-darwin); see available assets at ${RELEASES_URL}" ;;
  *)
    die "unsupported architecture: ${arch}; supported today: arm64/aarch64, x86_64; see ${RELEASES_URL}" ;;
esac

# --- version resolution ------------------------------------------------------
if [ -n "$VERSION" ]; then
  VERSION="${VERSION#v}"
  case "$VERSION" in
    *[!A-Za-z0-9._-]*) die "invalid version string: ${VERSION}" ;;
  esac
  info "installing pinned version v${VERSION}"
elif [ -z "$FROM_DIR" ]; then
  # Offline (--from-dir) mode adopts the version of the archive found on disk.
  info "resolving latest release from ${API_LATEST_URL}"
  json="$(curl -fsSL --retry 3 "${API_LATEST_URL}")" || \
    die "could not query ${API_LATEST_URL} (no releases yet? rate-limited? offline?) — pass --version vX.Y.Z explicitly, or browse ${RELEASES_URL}"
  tag="$(printf '%s\n' "$json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || die "could not parse tag_name from the GitHub API response — pass --version vX.Y.Z explicitly"
  VERSION="${tag#v}"
  info "latest release: v${VERSION}"
fi

ARCHIVE="omt-${TRIPLE}-v${VERSION}.tar.gz"
FOLDER="omt-${TRIPLE}-v${VERSION}"

# --- acquire archive + SHA256SUMS into a scratch dir -------------------------
TMP="$(mktemp -d "${TMPDIR:-/tmp}/omt-install.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM

if [ -n "$FROM_DIR" ]; then
  # Offline path: the local dir IS the release root (assemble output layout).
  FROM_DIR="${FROM_DIR%/}"
  [ -d "$FROM_DIR" ] || die "--from-dir: not a directory: ${FROM_DIR}"
  if [ -z "$VERSION" ]; then
    # No pinned version: adopt the version of the archive found on disk.
    discovered=""
    for f in "${FROM_DIR}"/omt-"${TRIPLE}"-v*.tar.gz; do
      [ -f "$f" ] || continue
      [ -z "$discovered" ] || die "multiple ${TRIPLE} archives in ${FROM_DIR}: pass --version to disambiguate"
      discovered="$f"
    done
    [ -n "$discovered" ] || die "no omt-${TRIPLE}-v*.tar.gz found in ${FROM_DIR}"
    base="$(basename "$discovered")"
    VERSION="${base#"omt-${TRIPLE}-v"}"
    VERSION="${VERSION%.tar.gz}"
    info "discovered offline release v${VERSION} (${base})"
  fi
  ARCHIVE="omt-${TRIPLE}-v${VERSION}.tar.gz"
  FOLDER="omt-${TRIPLE}-v${VERSION}"
  [ -f "${FROM_DIR}/${ARCHIVE}" ] || die "archive ${ARCHIVE} not found in ${FROM_DIR}"
  [ -f "${FROM_DIR}/SHA256SUMS" ] || die "missing SHA256SUMS beside the archive in ${FROM_DIR} — refusing to install unverified artifacts"
  cp "${FROM_DIR}/${ARCHIVE}" "${TMP}/${ARCHIVE}"
  cp "${FROM_DIR}/SHA256SUMS" "${TMP}/SHA256SUMS"
  info "acquired (offline) ${ARCHIVE} from ${FROM_DIR}"
else
  BASE_URL="https://github.com/${REPO_SLUG}/releases/download/v${VERSION}"
  curl -fsSL --retry 3 -o "${TMP}/${ARCHIVE}" "${BASE_URL}/${ARCHIVE}" || \
    { [ "$TRIPLE" = "x86_64-apple-darwin" ] && warn "download failed — x86_64 assets likely not published yet"; \
      die "could not download ${BASE_URL}/${ARCHIVE} — check version exists and browse available assets: ${RELEASES_URL}"; }
  curl -fsSL --retry 3 -o "${TMP}/SHA256SUMS" "${BASE_URL}/SHA256SUMS" || \
    die "could not download ${BASE_URL}/SHA256SUMS — refusing to install without checksums"
  info "downloaded ${ARCHIVE} + SHA256SUMS"
fi

# --- verify checksums (mandatory; relative-path entries -> verify in place) --
if command -v shasum >/dev/null 2>&1; then
  CHECKSUM_CMD="shasum -a 256"
elif command -v sha256sum >/dev/null 2>&1; then
  CHECKSUM_CMD="sha256sum"
else
  die "need shasum (macOS) or sha256sum (Linux) to verify SHA256SUMS"
fi
info "verifying SHA256 checksums ..."
if ! ( cd "$TMP" && $CHECKSUM_CMD -c SHA256SUMS ); then
  die "SHA256 verification FAILED for ${ARCHIVE} — nothing was installed. If this persists, re-download; if you suspect tampering, report it at ${RELEASES_URL}"
fi

# --- extract -----------------------------------------------------------------
mkdir -p "${TMP}/extract"
tar -xzf "${TMP}/${ARCHIVE}" -C "${TMP}/extract"
SRC="${TMP}/extract/${FOLDER}"
if [ ! -d "$SRC" ]; then
  ls -la "${TMP}/extract" >&2 || true
  die "unexpected archive layout: expected top-level folder ${FOLDER}/ (contract of scripts/assemble-release-archive.sh)"
fi
for entry in omt-daemon omt README.md; do
  [ -e "${SRC}/${entry}" ] || die "archive is missing expected file: ${FOLDER}/${entry}"
done

# --- install (only reached after verification succeeded) ---------------------
BIN_DIR="${BIN_DIR:-${OMT_INSTALL_DIR:-${DEFAULT_BIN_DIR}}}"
mkdir -p "$BIN_DIR"
case "$BIN_DIR" in
  /*) : ;;
  *) BIN_DIR="$(cd "$BIN_DIR" && pwd)" ;;
esac
cp "${SRC}/omt-daemon" "${SRC}/omt" "$BIN_DIR/"
chmod 755 "${BIN_DIR}/omt-daemon" "${BIN_DIR}/omt"
info "installed omt-daemon and omt into ${BIN_DIR}"

# --- PATH guidance (advisory only; rc files are never auto-edited) -----------
case ":${PATH}:" in
  *":${BIN_DIR}:"*) : ;;
  *)
    warn "${BIN_DIR} is not on your PATH"
    printf 'Add it to your shell profile:\n\n    export PATH="%s:$PATH"\n\n' "$BIN_DIR"
    detected=""
    for rc in .zshrc .zprofile .bashrc .bash_profile .profile; do
      if [ -f "${HOME:?}/${rc}" ]; then
        printf 'detected rc file: %s/%s\n' "$HOME" "$rc"
        detected=1
      fi
    done
    [ -n "$detected" ] || printf 'no common shell rc file detected in %s\n' "$HOME"
    ;;
esac

# --- post-install smoke ------------------------------------------------------
if smoke_out="$("${BIN_DIR}/omt" --version 2>&1)"; then
  info "smoke test: ${smoke_out}"
else
  warn "binaries installed but smoke test failed:"
  printf '%s\n' "$smoke_out" >&2
  die "post-install smoke test (\`${BIN_DIR}/omt --version\`) did not exit cleanly"
fi
info "done. For runtime diagnosis run: ${BIN_DIR}/omt doctor"
