#!/usr/bin/env bash
# Native Haskell contract check using the production-rendered effect vocabulary.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ -z ${TIDEPOOL_DEV_SHELL:-} ]]; then
  exec bash scripts/dev-shell.sh bash scripts/check-model-turns.sh "$@"
fi
check_root=${1:?Pass the directory containing production Tidepool/Effects/Core.hs}
case "$check_root" in /*) ;; *) check_root="$PWD/$check_root" ;; esac
[[ -f "$check_root/Tidepool/Effects/Core.hs" ]] || {
  echo 'First run model_turn::tests::export_production_effect_core_for_haskell_check with TIDEPOOL_MODEL_HASKELL_CHECK_DIR set to this directory' >&2
  exit 1
}
# Contract.hs reaches Effects.Row for inspection. This test has no concrete
# resident row; use the actual generated vocabulary, without adding effects.
cat > "$check_root/Tidepool/Effects.hs" <<'HS'
module Tidepool.Effects (module Tidepool.Effects.Core) where
import Tidepool.Effects.Core
HS
flake=${TIDEPOOL_DEV_FLAKE:-${TIDEPOOL_DEV_SHELL%%#*}}
jev_source=$(nix flake archive --json --no-write-lock-file "$flake" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["inputs"]["jev-dsl"]["path"])')
jev_core="$jev_source/core"
[[ -f "$jev_core/Jev/Core/Json.hs" ]] || {
  echo "error: configured jev-dsl core is missing: $jev_core" >&2
  exit 2
}
ghc -O0 -i"$check_root" -i./bridge/haskell/lib \
  -outputdir "$check_root/out" bridge/haskell/test-model-turn/Main.hs \
  -o "$check_root/model-contract"
"$check_root/model-contract"
ghc -fno-code -i"$check_root" -i./bridge/haskell/lib -i./bridge/haskell/actors \
  -i./exomonad/examples/workspace/.exomonad \
  -i"$jev_core" \
  -outputdir "$check_root/examples" \
  bridge/haskell/examples/model-turns/Coordination.hs \
  bridge/haskell/examples/model-turns/ContextWorkflow.hs
