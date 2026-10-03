#!/usr/bin/env bash
# Run the pinned Buck CLI and declared action tools; resource admission is external.
set -euo pipefail
cd "$(dirname "$0")/.."
mountpoint -q "$PWD/buck-out" || { echo 'Buck requires the per-checkout buck-out bind mount' >&2; exit 1; }
[[ -f .buckconfig.local ]] || { echo 'Run scripts/buck2-configure.sh first' >&2; exit 1; }
if [[ ${TIDEPOOL_BUCK_SHELL:-} != ready ]]; then
  exec bash scripts/dev-shell.sh env TIDEPOOL_BUCK_SHELL=ready bash scripts/buck2-run.sh "$@"
fi
buck_bin=$(command -v buck2)
action_path=$(awk -F ' = ' '$1 == "action_path" {print $2}' .buckconfig.local)
[[ -n $action_path ]] || { echo 'Missing declared Buck action PATH; reconfigure' >&2; exit 1; }
export PATH="$action_path:/run/current-system/sw/bin"
exec "$buck_bin" "$@"
