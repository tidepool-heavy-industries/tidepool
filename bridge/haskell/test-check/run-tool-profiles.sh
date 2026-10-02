#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} != --in-dev-shell ]]; then
  exec bash scripts/dev-shell.sh bash "$0" --in-dev-shell "$@"
fi
shift
support=${1:?provide the generated Core, Authored and Effects include directory}
scratch=${2:-target/context-spec/tool-profiles}
for source in Tidepool/Effects/Core.hs Tidepool/Effects/Authored.hs Tidepool/Effects.hs; do
  test -f "$support/$source"
done
mkdir -p "$scratch/objects"
{
  git rev-parse HEAD
  command -v ghc
  ghc --version
  sha256sum "$support/Tidepool/Effects/Core.hs" "$support/Tidepool/Effects/Authored.hs" "$support/Tidepool/Effects.hs"
} > "$scratch/provenance.log"
includes=(-i"$support" -ibridge/haskell/lib -ibridge/haskell/actors)
ghc -O0 -Wall "${includes[@]}" -outputdir "$scratch/objects" \
  -o "$scratch/tool-profiles" bridge/haskell/test-check/ToolProfiles.hs > "$scratch/build.log" 2>&1
"$scratch/tool-profiles" | tee "$scratch/result.log"
for fixture in AsyncContext AsyncProfile UnsupportedProfile; do
  if ghc -fno-code "${includes[@]}" -outputdir "$scratch/objects" \
    "bridge/haskell/test-check/tool-profiles/$fixture.hs" > "$scratch/$fixture.log" 2>&1; then
    echo "unexpected compile acceptance: $fixture" >&2
    exit 1
  fi
  case "$fixture" in
    AsyncContext) expected='is not a member of the type-level list' ;;
    AsyncProfile) expected='requires a synchronous Haskell tool' ;;
    UnsupportedProfile) expected='No instance for.*Contains Commands' ;;
  esac
  rg -q "$expected" "$scratch/$fixture.log"
  echo "passed: compile rejection $fixture" | tee -a "$scratch/result.log"
done
