#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-steps.sh

run_step "cargo fmt --all -- --check" cargo fmt --all -- --check
# --keep-going: report every target's lint errors in one run instead of
# stopping at the first failing target and hiding the rest.
run_step "cargo clippy --workspace --all-targets --keep-going -- -D warnings" \
  cargo clippy --workspace --all-targets --keep-going -- -D warnings

finish_steps "lint"
