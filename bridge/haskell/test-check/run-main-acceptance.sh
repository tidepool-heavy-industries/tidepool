#!/usr/bin/env bash
# Regenerate main's effect support, then run the narrow Exomonad acceptance
# contracts with isolated GHC and prepared-runtime outputs.
set -euo pipefail

if [[ ${1:-} != --in-dev-shell ]]; then
  exec bash scripts/dev-shell.sh bash "$0" --in-dev-shell "$@"
fi
shift

dependencies=${1:?matched Cargo dependency directory required}
scratch=${2:?fresh acceptance output directory required}
shift 2

[[ ! -e "$scratch" ]] || {
  echo "error: acceptance output directory already exists: $scratch" >&2
  exit 2
}
[[ -d "$dependencies" ]] || {
  echo "error: Cargo dependencies are missing: $dependencies" >&2
  exit 2
}
: "${TIDEPOOL_EXTRACT:?set the matched main extractor executable}"
: "${TIDEPOOL_EXTRACT_WORKER:?set the matched main extractor worker}"

repo=$(pwd)
mkdir -p "$scratch"
scratch=$(cd "$scratch" && pwd)
mkdir -p "$scratch/xdg-effects"
export XDG_CACHE_HOME="$scratch/xdg-effects"
git rev-parse HEAD > "$scratch/source-oid"
jev_source=$(nix flake archive --json --no-write-lock-file "$TIDEPOOL_DEV_FLAKE" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["inputs"]["jev-dsl"]["path"])')
jev_core="$jev_source/core"
[[ -f "$jev_core/Jev/Core/Json.hs" ]] || {
  echo "error: configured jev-dsl core is missing: $jev_core" >&2
  exit 2
}
printf '%s\n' "$jev_source" > "$scratch/jev-source-path"
sha256sum \
  bridge/protocol/src/effects/commands.rs \
  bridge/mcp/src/generated/commands.rs \
  bridge/facade/src/actor_host/effect_vocabulary.rs \
  "$jev_core/Jev/Core/Json.hs" \
  > "$scratch/effect-source-sha256"

# Compile helpers call the configured compiler endpoint directly. Use the
# repository's extractor resolver, while keeping its authority manifest inside
# this run's private evidence directory.
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
export TIDEPOOL_COMPILER_DEPLOYMENT="$scratch/compiler-deployment.json"
"$TIDEPOOL_EXTRACT" --compiler-deployment-manifest "$TIDEPOOL_COMPILER_DEPLOYMENT"
source "$repo/scripts/lib-extract.sh"
resolve_tidepool_extract

fixtures="$scratch/fixtures"
bash bridge/haskell/test-check/build-prepared-contracts.sh \
  --in-dev-shell "$dependencies" "$fixtures" "$@"

support="$scratch/support"
"$fixtures/effect-support" "$support" | tee "$scratch/effect-support.log"
sha256sum "$support/Tidepool/Effects/Core.hs" > "$scratch/effect-core-sha256"

export TIDEPOOL_PRELUDE_DIR="$repo/bridge/haskell/lib"
workspace="$repo/exomonad/examples/workspace/.exomonad"
workspace_fixture="$scratch/workspace-fixture"
python3 bridge/haskell/test-check/generate-workspace-capture-fixture.py \
  "$repo/bridge/facade/src/exomonad/workspace.hs" \
  "$workspace/config.toml" \
  "$workspace_fixture/Exomonad/Workspace.hs" \
  | tee "$scratch/workspace-fixture.log"
export TIDEPOOL_CHECK_INCLUDE="$workspace_fixture"

# Re-run the outer recipe in the actual prepared runtime to capture its cells
# using this checkout's generated effect vocabulary. The captured assertions
# are diagnostic data, not behavioral proof.
capture="$scratch/source-capture"
mkdir -p "$capture" "$scratch/xdg-capture"
env -u TIDEPOOL_EXTRACT_DAEMON_SOCKET XDG_CACHE_HOME="$scratch/xdg-capture" \
  "$fixtures/capture-recipes" \
  "$repo" "$repo/exomonad/examples/workspace/.exomonad" "$capture" \
  Project.BackgroundCommandExampleChecks.completion "$support" \
  | tee "$capture/result.log"
grep -Fq \
  'captured Project.BackgroundCommandExampleChecks.completion: 7 cells; 4 host assertions recorded, not validated' \
  "$capture/result.log"

probe="$scratch/probe"
mkdir -p "$probe/objects"
bash scripts/dev-shell.sh ghc -O0 -Wall -Wno-simplifiable-class-constraints \
  -fforce-recomp -outputdir "$probe/objects" -o "$probe/contract" \
  -i"$support" -ibridge/haskell/lib -ibridge/haskell/actors \
  -i"$jev_core" -i"$workspace" -i"$workspace_fixture" \
  exomonad/examples/workspace/.exomonad/checks/automation-helper-contract.hs \
  > "$probe/compile.log" 2>&1
"$probe/contract" > "$probe/result.log" 2>&1
[[ $(awk '/^passed:/ { count++ } END { print count+0 }' "$probe/result.log") -eq 34 ]] || {
  echo "error: native probe contract did not report 34 checks" >&2
  exit 1
}

browser="$scratch/browser"
mkdir -p "$browser/objects"
bash scripts/dev-shell.sh ghc -O0 -Wall -Wno-simplifiable-class-constraints \
  -fforce-recomp -outputdir "$browser/objects" -o "$browser/contract" \
  -i"$support" -ibridge/haskell/lib -ibridge/haskell/actors \
  -i"$jev_core" -i"$workspace" -i"$workspace_fixture" \
  exomonad/examples/workspace/.exomonad/checks/browser-scenario-contract.hs \
  > "$browser/compile.log" 2>&1
"$browser/contract" > "$browser/result.log" 2>&1
grep -Fq \
  '24 workflow cases passed; protocol-failure isolation regression and 3 scoped-helper validation cases' \
  "$browser/result.log"

bash bridge/haskell/test-check/run-native-contract.sh \
  --in-dev-shell "$support" "$scratch/assertions" \
  > "$scratch/assertions-driver.log" 2>&1
grep -Fq 'executed: 17 native helper contract checks' \
  "$scratch/assertions/result.log"

bash bridge/haskell/test-check/run-pinned-contract.sh \
  --in-dev-shell "$support" "$scratch/pinned" \
  > "$scratch/pinned-driver.log" 2>&1
grep -Fq 'executed: 1 native pinned-source contract' \
  "$scratch/pinned/result.log"

prepared="$scratch/prepared-cases"
mkdir -p "$prepared" "$scratch/xdg-prepared"
env -u TIDEPOOL_EXTRACT_DAEMON_SOCKET XDG_CACHE_HOME="$scratch/xdg-prepared" \
  bash scripts/dev-shell.sh \
    "$fixtures/prepared-contract" "$repo" "$prepared" \
    > "$prepared/result.log" 2>&1
grep -Fq \
  'executed: 8 prepared-runtime helper contract cases from one compiled immutable fixture' \
  "$prepared/result.log"

printf '%s\n' \
  'passed: source-capture smoke (7 cells; 4 assertions recorded)' \
  'passed: native probe contract (34 assertions)' \
  'passed: native browser contract (24 workflows, 3 helpers, 1 protocol regression)' \
  'passed: native assertion contract (17 checks)' \
  'passed: pinned assertion contract (1 check)' \
  'passed: prepared assertion contract (8 cases)' \
  "evidence: $scratch"
