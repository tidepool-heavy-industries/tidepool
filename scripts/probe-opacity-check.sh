#!/usr/bin/env bash
# Recurring check for the "Optimizer-folded corpus probes" tech debt: dumps
# Tidy Core for the pure-eval cohort contracts and verifies each probe still
# shows the mechanism its cohort claims, per the committed manifest. Run
# standalone with `just probe-opacity-check`; wired into scripts/fixtures.sh
# via scripts/prepared-corpus.sh so `just verify` reaches it.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
contract_dir="haskell/test-prepared-stg"
manifest="$contract_dir/probe-opacity-manifest.json"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

modules="$(python3 -c '
import json, sys
print(" ".join(json.load(open(sys.argv[1]))["modules"]))
' "$manifest")"

dump_args=()
for module in $modules; do
  ghc -XGHC2024 -O2 -ddump-simpl -dsuppress-all -fforce-recomp \
    -outputdir "$work/$module.build" \
    -c "$contract_dir/$module.hs" \
    >"$work/$module.dump.txt"
  dump_args+=("$module=$work/$module.dump.txt")
done

python3 scripts/probe-opacity-check.py "$manifest" "${dump_args[@]}"
