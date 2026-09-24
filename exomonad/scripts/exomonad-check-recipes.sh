#!/usr/bin/env bash
# Run every configured recipe check of a workspace as its own `exomonad check
# --recipe` process, several at a time, all compiling through one warm
# compile daemon. The recipes are independent resident sessions, so the wall
# clock is the longest recipe rather than the sum. Usage:
#   exomonad-check-recipes.sh <workspace> [parallelism, default 1: recipe turns
#   carry fixed timeouts that three sessions on one daemon already exceed]
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
workspace="${1:?usage: exomonad-check-recipes.sh <workspace> [parallelism]}"
parallelism="${2:-1}"
config="$workspace/.exomonad/config.toml"

mapfile -t entries < <(python3 - "$config" <<'PY'
import re, sys, tomllib
with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)
for entry in config.get("haskell", {}).get("checks", []):
    print(entry)
PY
)
if [ "${#entries[@]}" -eq 0 ]; then
  echo "error: no [haskell] checks configured in $config" >&2
  exit 1
fi

# One build and one daemon for every recipe process; exomonad-run.sh keeps an
# inherited socket for `check`.
source exomonad/scripts/exomonad-build.sh
start_battery_daemon
trap teardown_battery_daemon EXIT

# Logs live under the checkout, not TMPDIR: the dev shell removes its TMPDIR
# on exit, and a failure's reason must outlive the run.
logs="$PWD/target/tidepool-test-runs/recipes-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$logs"
echo "==> ${#entries[@]} recipes, $parallelism at a time; logs in $logs"
status=0
printf '%s\n' "${entries[@]}" | xargs -P "$parallelism" -I{} bash -c '
  entry="$1"; log="$2/$entry.log"
  if "$PWD/target/debug/exomonad" check --workspace "$3" --recipe "$entry" >"$log" 2>&1; then
    echo "passed  $entry ($(grep -c "^  passed" "$log") assertions)"
  else
    echo "FAILED  $entry — see $log"; exit 1
  fi
' _ {} "$logs" "$workspace" || status=1
exit "$status"
