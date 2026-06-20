# Decision: Integer/bignum via the native GHC backend (not Rust-mpn)

Decided 2026-06-20 by the human, **reversing** the earlier in-session choice of
a Rust `mpn` reimplementation. This supersedes "i like the rust option".

## Why the reversal
The Rust-mpn path was scoped as "hot-13 mpn fns + a literal". Building it
revealed the scope was a large underestimate:
- mpn FFI was necessary-but-not-sufficient: enabling it surfaced ~15 more
  primops, a `LitByteArray` literal mechanism, and lazy-poison discipline.
- Worse, value-dependent integration bugs where ghc-bignum's gmp-backend Core
  drives the mpn surface in ways the hand-written contracts don't yet match:
  `show (2^100)` → "0" (wrong, no mpn ops even called), big div/mod/gcd → hit
  ghc-bignum error branches, `fromIntegral :: Integer -> Double` → unmapped
  `roundingMode#`. Each fix surfaced the next layer → multi-session, uncertain
  convergence.

The agent's hands-on assessment: getting Integer *correct* end-to-end via
mpn-Rust is a long, uncertain slog (matching GMP's contract op-by-op). The
**native GHC bignum backend** is ghc-bignum's own *pure-Core* Integer impl —
correct by construction, no FFI, no per-op contract matching, no value-dependent
bugs, no Read/show/Double integration gaps. Cost: a ~3-line flake change
(`enableNativeBignum`) + a GHC rebuild, which the project already does for fat
interfaces. Given the revealed depth, native is markedly cheaper to get CORRECT
— and "do it right / stable foundation" is the standing priority.

## What was validated under the mpn path (preserved on
##  branch proptest-ghc-idioms.double-literal, WIP commit)
- `tidepool-bignum` shared crate: 27 limb-contract unit tests pass.
- Full plumbing compiles across all layers.
- Big-integer LITERAL works end-to-end (materialize + show) via the new
  `LitByteArray` literal mechanism (root-caused a real bug: BigNat# stored as a
  String literal → 7-exabyte alloc).

## Pivot plan (agent: double-literal)
1. `enableNativeBignum` flake change; rebuild GHC + extract against it.
2. Keep carry-over pieces (LitByteArray, lazy-poison, still-needed primops);
   retire the now-dead mpn FFI machinery (don't ship dead code).
3. Validate end-to-end (must flip to WORKS): `show (2^100)`, big div/mod/gcd,
   `fromIntegral :: Integer -> Double`, the ORIGINAL trigger (large Double
   literals e.g. `1.79e308`), the JIT-vs-eval differential, and the
   gotcha_registry large-double-literal probe (LOUD-FAIL → WORKS).

## Promote on landing
Once the pivot is validated + merged, promote this to a Key Decision in
CLAUDE.md (Integer = native ghc-bignum backend) so it isn't re-litigated.
