#!/usr/bin/env bash
# Build this checkout and run one Exomonad subcommand (init, new, check, ...)
# with its matched local extractor and compiler worker. Bare
# `target/debug/exomonad` would resolve `tidepool-extract` from $PATH, where an
# installed copy from another checkout can shadow the one just built.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# `just` preserves its option separator as the first variadic recipe
# argument. It separates Just flags, not Exomonad's clap input.
if [[ "${1:-}" == "--" ]]; then
  shift
fi
subcommand="${1:?usage: exomonad-run.sh <subcommand> [args...]}"
shift

# exomonad-build.sh drops any inherited compile-daemon socket while resolving
# the selected producer; a `check` may reuse the caller's resident daemon.
inherited_daemon_socket="${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}"
source exomonad/scripts/exomonad-build.sh

if [ "$subcommand" = "check" ]; then
  # Reuse a live daemon (an inherited socket, else the `just daemon-start`
  # one when its producer matches this build), or start a per-run one and
  # tear it down on exit; either way every recipe turn after the first
  # skips the cold GHC and stdlib load.
  if [ -n "$inherited_daemon_socket" ]; then
    export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$inherited_daemon_socket"
  fi
  prepare_battery_artifacts exomonad-check exomonad/scripts/exomonad-run.sh check "$@"
  cleanup_check() {
    local status=$?
    finalize_battery_artifacts "$status"
    teardown_battery_daemon
    return "$status"
  }
  trap cleanup_check EXIT
  start_battery_daemon
  "$PWD/target/debug/exomonad" "$subcommand" "$@"
  exit $?
fi

exec "$PWD/target/debug/exomonad" "$subcommand" "$@"
