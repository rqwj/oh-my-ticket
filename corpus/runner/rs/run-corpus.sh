#!/usr/bin/env bash
# Rust behavioral-corpus leg (plan U3).
# The runner lives in crates/omt-domain/tests (workspace members = crates/*);
# this wrapper is the documented entry point. See corpus/runner/rs/README.md.
set -euo pipefail
cd "$(dirname "$0")/../../.."
export CARGO_HOME="$PWD/.cargo-home"
exec cargo test -p omt-domain --test corpus -- --nocapture "$@"
