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
-- THE DEFAULT-VOCABULARY CASE, deliberately. There is NO `import Tidepool.Event`
-- here: `Tidepool.Effects` has no export list, so it re-exports its own
-- generated `(<|>)`, and the eval preamble imports both it and
-- `Tidepool.Prelude` by DEFAULT. So an author hits this collision without
-- importing anything extra. An earlier version of this probe imported
-- Tidepool.Event explicitly, which understated the blast radius — the gate must
-- reproduce the worst case, not the one first stumbled into.
module AltGate where
import Tidepool.Effects
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
# NB: a `sed` s/// here is a trap — every usable delimiter (`|`, `/`, `,`) either
# appears in `(<|>)` or in the import path, and a delimiter collision fails with
# an opaque "unknown option to `s'" rather than a wrong result. Python does a
# literal string replace with no metacharacter surface at all.
python3 - "$WORK/gen/Tidepool/Effects.hs" "$WORK/gen_prefix/Tidepool/Effects.hs" <<'PYEOF'
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src).read()
fixed   = "import Tidepool.Prelude hiding (error, (<|>))"
prefix  = "import Tidepool.Prelude hiding (error)"
assert fixed in s, "emitted module does not carry the conditional hiding"
open(dst, "w").write(s.replace(fixed, prefix, 1))
PYEOF
if ! grep -q '^import Tidepool.Prelude hiding (error)$' "$WORK/gen_prefix/Tidepool/Effects.hs"; then
  fail "alternative_collision_is_real_without_the_fix" \
       "could not reconstruct the pre-fix import line; the emitted module may not carry the conditional hiding"
else
  out="$(compile "$WORK/od1" -i"$WORK/gen_prefix" -ihaskell/lib -i"$WORK/probe" "$WORK/probe/AltGate.hs")"
  # WRONG-REASON GUARD: it is not enough that SOME ambiguity about `<|>` appears.
  # If Tidepool.Effects itself fails to compile without the hiding (its `infixl 3
  # <|>` and definition sit alongside the imported operator), this gate would go
  # green while proving something entirely different — that the GENERATED module
  # cannot build, not that the AUTHOR's example is ambiguous. So require the
  # diagnostic to be located in the probe.
  if echo "$out" | grep -q "Ambiguous occurrence" \
     && echo "$out" | grep -q "<|>" \
     && echo "$out" | grep -q "AltGate.hs:"; then
    pass "alternative_collision_is_real_without_the_fix"
  elif echo "$out" | grep -q "Ambiguous occurrence"; then
    fail "alternative_collision_is_real_without_the_fix" \
         "ambiguity found but NOT in the probe — the generated module itself may not compile without the hiding, which is a different fact: $out"
  else
    fail "alternative_collision_is_real_without_the_fix" \
         "expected an ambiguous-occurrence diagnostic for <|>; got: ${out:-<clean compile>}"
  fi
fi

# ---- GATE 2: GREEN --------------------------------------------------------
# Assert on the ARTIFACT THIS GATE COMPILES before invoking GHC. A red result is
# no more self-describing than a green one: without this, "stale module" and
# "the fix is mis-scoped" are one indistinguishable mystery.
gen_import="$(grep -m1 '^import Tidepool.Prelude' "$WORK/gen/Tidepool/Effects.hs")"
if [ "$gen_import" != "import Tidepool.Prelude hiding (error, (<|>))" ]; then
  fail "prd_example_compiles_unqualified_with_the_fix" \
       "the module this gate compiled does NOT carry the fix — its import line is: ${gen_import:-<none found>}"
else
  out="$(compile "$WORK/od2" -i"$WORK/gen" -ihaskell/lib -i"$WORK/probe" "$WORK/probe/AltGate.hs")"
  if [ -z "$out" ]; then
    pass "prd_example_compiles_unqualified_with_the_fix"
  else
    fail "prd_example_compiles_unqualified_with_the_fix" \
         "the compiled module DID carry the fix (${gen_import}) and GHC still objected: $out"
  fi
fi

# ---- GATE 4: IS THE GENERATED MODULE'S OWN HIDING LOAD-BEARING? -----------
# Compile the STRIPPED Tidepool.Effects alone, with no probe. If it builds, that
# hiding does nothing for the module itself and is dead weight once the real
# author-side fix lands; if it fails, the hiding is load-bearing and must stay.
# Recorded as a fact either way rather than reasoned about — the module declares
# `infixl 3 <|>` and defines the operator alongside the imported one, and
# whether GHC calls that ambiguous is not something to predict.
cat > "$WORK/probe/EffOnly.hs" <<'EOF'
module EffOnly where
import Tidepool.Effects ()
EOF
out="$(compile "$WORK/od4" -i"$WORK/gen_prefix" -ihaskell/lib -i"$WORK/probe" "$WORK/probe/EffOnly.hs")"
if [ -z "$out" ]; then
  pass "generated_module_compiles_without_its_own_hiding"
else
  fail "generated_module_compiles_without_its_own_hiding" \
       "the eval_prep hiding is LOAD-BEARING — do NOT delete it. First diagnostic: $(echo "$out" | head -3)"
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
