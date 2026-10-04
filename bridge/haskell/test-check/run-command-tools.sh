#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} != --in-dev-shell ]]; then
  exec bash scripts/dev-shell.sh bash "$0" --in-dev-shell "$@"
fi
shift
support=${1:?provide the generated effects include directory}
scratch=${2:-target/tool-result-check/command-tools}
test -f "$support/Tidepool/Effects/Core.hs"
mkdir -p "$scratch/objects"
{
  git rev-parse HEAD
  command -v ghc
  ghc --version
  sha256sum "$support/Tidepool/Effects/Core.hs"
} > "$scratch/provenance.log"
ghc -O0 -Wall -i"$support" -ibridge/haskell/lib -outputdir "$scratch/objects" \
  -o "$scratch/command-tools" bridge/haskell/test-check/CommandTools.hs > "$scratch/build.log" 2>&1
"$scratch/command-tools" | tee "$scratch/result.log"
