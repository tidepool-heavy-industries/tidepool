#!/usr/bin/env bash
# PRD 19 — the three GHC gates for the conditional `hiding (error, (<|>))` in
# `effects_module_source_at` (tidepool-mcp/src/eval_prep.rs).
#
# These live in a script rather than as `#[test]`s because each one is a full
# GHC compile of the Tidepool.Prelude tree (~19 modules). Putting them in
# tidepool-mcp's test binary would drop minutes-long GHC work into the fast
# pure-Rust tier, which is exactly what `.config/nextest.toml`'s default-filter
# exists to prevent. They are run deliberately, wrapped, and their named PASS
# lines are pasted into plans/post-restart/worktree-lanes/L4-receipt.md with the
# base commit.
#
# WHY GATE 1 EXISTS. A green compile in gate 2 is equally consistent with "the
# fix works" and "the collision was never reachable". Gate 1 removes that
# ambiguity by reproducing the pre-fix source EXACTLY (same generated module,
# with only the hiding term reverted) and asserting the failure is specifically
# the ambiguous-occurrence diagnostic — not merely that something failed.
#
# Run it wrapped, never `exclusive`:
#   /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- scripts/prd19-alternative-gates.sh
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The with-packages GHC, resolved from the installed extract wrapper rather than
# hardcoded — that wrapper is the thing that knows which store path supplies
# lens/freer-simple. Fail loudly if absent: a skipped gate is a gate that cannot
# be distinguished from one that silently stopped existing.
EXTRACT_WRAPPER="$(command -v tidepool-extract || true)"
if [ -z "$EXTRACT_WRAPPER" ]; then
  echo "FAIL: tidepool-extract not on PATH — cannot resolve the with-packages GHC" >&2
  exit 1
fi
GHC_BIN_DIR="$(sed -n 's|^export PATH="\([^:]*\):\$PATH"$|\1|p' "$EXTRACT_WRAPPER" | head -1)"
GHC="$GHC_BIN_DIR/ghc"
if [ ! -x "$GHC" ]; then
  echo "FAIL: no executable ghc at $GHC" >&2
  exit 1
fi

# Emit the generated Tidepool.Effects for a row that CONTAINS RepoEvent.
mkdir -p "$WORK/gen/Tidepool"
PRD19_EMIT="$WORK/gen/Tidepool/Effects.hs" \
  cargo test -q -p tidepool-mcp --test prd19_emit -- --ignored emit_for_gates >/dev/null 2>&1 || true
if [ ! -s "$WORK/gen/Tidepool/Effects.hs" ]; then
  echo "FAIL: could not emit the generated Tidepool.Effects" >&2
  exit 1
fi

# The PRD's own worked example, written UNQUALIFIED — no hand-added hiding.
# This is the spelling an author actually types.
mkdir -p "$WORK/probe"
cat > "$WORK/probe/AltGate.hs" <<'EOF'
{-# LANGUAGE OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, GADTs, OverloadedRecordDot #-}
module AltGate where
import Tidepool.Worktree
import Tidepool.Event
import Tidepool.Prelude

merged :: WorktreeHandle -> WorktreeHandle
       -> Event (Either (Observed CommitReceipt) (Observed HeadChangeReceipt))
merged a b = fmap Left (commit a) <|> fmap Right (headChanged b)
EOF

# A NON-RepoEvent row must still resolve Alternative's (<|>) for Maybe
# unqualified. This is the gate that would catch the change being accidentally
# unconditional.
cat > "$WORK/probe/NoRegress.hs" <<'EOF'
{-# LANGUAGE OverloadedStrings #-}
module NoRegress where
import Tidepool.Prelude

firstJust :: Maybe Int -> Maybe Int -> Maybe Int
firstJust a b = a <|> b
EOF

compile() { # <outdir> <includes...> <file>
  local od="$1"; shift
  rm -rf "$od"; mkdir -p "$od"
  "$GHC" -fno-code -v0 -XNoImplicitPrelude -outputdir "$od" "$@" 2>&1
}

status=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1 -- $2"; status=1; }

# ---- GATE 1: RED BASELINE -------------------------------------------------
# Revert ONLY the hiding term, reproducing the pre-fix generated module.
mkdir -p "$WORK/gen_prefix/Tidepool"
sed 's|^import Tidepool.Prelude hiding (error, (<|>))$|import Tidepool.Prelude hiding (error)|' \
  "$WORK/gen/Tidepool/Effects.hs" > "$WORK/gen_prefix/Tidepool/Effects.hs"
if ! grep -q '^import Tidepool.Prelude hiding (error)$' "$WORK/gen_prefix/Tidepool/Effects.hs"; then
  fail "alternative_collision_is_real_without_the_fix" \
       "could not reconstruct the pre-fix import line; the emitted module may not carry the conditional hiding"
else
  out="$(compile "$WORK/od1" -i"$WORK/gen_prefix" -ihaskell/lib -i"$WORK/probe" "$WORK/probe/AltGate.hs")"
  if echo "$out" | grep -q "Ambiguous occurrence" && echo "$out" | grep -q "<|>"; then
    pass "alternative_collision_is_real_without_the_fix"
  else
    fail "alternative_collision_is_real_without_the_fix" \
         "expected an ambiguous-occurrence diagnostic for <|>; got: ${out:-<clean compile>}"
  fi
fi

# ---- GATE 2: GREEN --------------------------------------------------------
out="$(compile "$WORK/od2" -i"$WORK/gen" -ihaskell/lib -i"$WORK/probe" "$WORK/probe/AltGate.hs")"
if [ -z "$out" ]; then
  pass "prd_example_compiles_unqualified_with_the_fix"
else
  fail "prd_example_compiles_unqualified_with_the_fix" "$out"
fi

# ---- GATE 3: NO REGRESSION ------------------------------------------------
# haskell/lib alone — no generated Effects on the include path, i.e. a row that
# does not carry RepoEvent.
out="$(compile "$WORK/od3" -ihaskell/lib -i"$WORK/probe" "$WORK/probe/NoRegress.hs")"
if [ -z "$out" ]; then
  pass "alternative_still_resolves_in_a_non_repoevent_row"
else
  fail "alternative_still_resolves_in_a_non_repoevent_row" "$out"
fi

exit $status
