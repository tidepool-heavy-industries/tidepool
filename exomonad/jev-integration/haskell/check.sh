#!/usr/bin/env bash
# Invoke via scripts/dev-shell.sh; keep GHC outputs in the worktree build tree.
set -euo pipefail
cd "$(dirname "$0")/../../.."
artifact_dir=target/jev-haskell
mkdir -p "$artifact_dir"
ghc -Wall -Werror -fno-code -ijev-integration/haskell \
  -outputdir "$artifact_dir" jev-integration/bridge/haskell/Accepted.hs
runghc -ijev-integration/haskell jev-integration/bridge/haskell/Accepted.hs
for fixture in RejectMixedScopes RejectWrongPayload RejectCoerceScope RejectMissingHandler; do
  if ghc -Wall -Werror -fno-code -ijev-integration/haskell \
      -outputdir "$artifact_dir" "jev-integration/bridge/haskell/$fixture.hs" \
      > "$artifact_dir/$fixture.log" 2>&1; then
    echo "UNEXPECTED COMPILE SUCCESS: $fixture" >&2
    exit 1
  fi
  # Dependency failures must not masquerade as negative-fixture success.
  if ! grep -q "$fixture.hs:.*error:" "$artifact_dir/$fixture.log"; then
    cat "$artifact_dir/$fixture.log" >&2
    exit 1
  fi
  echo "Rejected as expected: $fixture (diagnostics: $artifact_dir/$fixture.log)"
done
