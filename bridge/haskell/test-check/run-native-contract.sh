#!/usr/bin/env bash
# Run from the repository root with the directory of real generated effect
# modules as the first argument. Outputs stay in this worktree.
set -euo pipefail

if [[ ${1:-} != --in-dev-shell ]]; then
  exec bash scripts/dev-shell.sh bash "$0" --in-dev-shell "$@"
fi
shift
support=${1:?provide the directory containing generated Tidepool/Effects/Core.hs}
scratch=${2:-target/async-wave/check-contract}
test -f "$support/Tidepool/Effects/Core.hs"
mkdir -p "$scratch/objects"
{
  git rev-parse HEAD
  command -v ghc
  ghc --version
  ghc-pkg latest freer-simple
  sha256sum "$support/Tidepool/Effects/Core.hs"
} > "$scratch/provenance.log"
ghc -O0 -Wall \
  -i"$support" -ibridge/haskell/lib -ibridge/haskell/actors \
  -outputdir "$scratch/objects" -o "$scratch/check-contract" \
  bridge/haskell/test-check/NativeContract.hs > "$scratch/build.log" 2>&1
"$scratch/check-contract" "$support" "$scratch/cells" | tee "$scratch/result.log"
