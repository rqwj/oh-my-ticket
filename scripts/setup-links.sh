#!/usr/bin/env bash
# One-time local setup: package.json references the DSH packages as
# link:./.dsh-checkout/... so no machine-specific path is committed.
# This script creates the .dsh-checkout symlink pointing at YOUR local
# deepseek-harness checkout.
#
# Usage:
#   pnpm run setup /path/to/deepseek-harness
#   DSH_CHECKOUT=/path/to/deepseek-harness pnpm run setup
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECKOUT="${1:-${DSH_CHECKOUT:-}}"

if [[ -z "$CHECKOUT" ]]; then
  echo "error: pass the deepseek-harness checkout path as an argument or set DSH_CHECKOUT" >&2
  echo "  pnpm run setup /path/to/deepseek-harness" >&2
  exit 1
fi

if [[ ! -d "$CHECKOUT" ]]; then
  echo "error: '$CHECKOUT' is not a directory" >&2
  exit 1
fi
CHECKOUT="$(cd "$CHECKOUT" && pwd)"

# Sanity-check that this really is a deepseek-harness checkout with the
# packages referenced from package.json.
for sub in \
  vendor/cordis \
  vendor/cosmokit \
  vendor/schemastery \
  packages/client/connection \
  packages/client/store \
  packages/client/ui-input-trigger \
  packages/skill/skill \
  packages/core/tools; do
  if [[ ! -d "$CHECKOUT/$sub" ]]; then
    echo "error: '$CHECKOUT/$sub' not found — is this a deepseek-harness checkout?" >&2
    exit 1
  fi
done

ln -sfn "$CHECKOUT" "$ROOT/.dsh-checkout"
echo "linked .dsh-checkout -> $CHECKOUT"
echo "now run: pnpm install"
