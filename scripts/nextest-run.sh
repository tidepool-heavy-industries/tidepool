#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-nextest.sh
on_signal() {
  [[ -z "$nextest_pid" ]] || { kill -TERM "$nextest_pid" 2>/dev/null || true; wait "$nextest_pid" 2>/dev/null || true; }
  exit 130
}
trap on_signal INT TERM
nextest_run_checked "$@"
