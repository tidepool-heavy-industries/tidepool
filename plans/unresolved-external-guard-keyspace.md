# Latent bug: unresolved-external guards no-op (key-space mismatch)

Found 2026-06-20 by the core-trace agent while diagnosing the nospec bug
(plans/send-print-unresolved-bug.md). NOT yet fixed — a follow-up.

## What
Two guards are meant to turn a genuinely-unresolvable external reference into a
clean **compile-time** error (the project's "fail LOUD" guarantee), instead of a
runtime `unresolved_var_trap`:
- `haskell/app/Main.hs` (~:164) — the "Unresolved external(s)" compile guard,
  gated on `trulyUnresolved`.
- `haskell/src/Tidepool/Translate.hs` (~:1145, the `Var` catch-all in
  `translateHead`) — the clean-error-node fallback
  `if Set.member (varId v) tsUnresolvedIds then NVar <error-sentinel>`.

Both **silently no-op** because of a key-space mismatch: `UnresolvedVar.uvKey`
(Resolve.hs) is `getKey (varUnique v)` — a GHC `Unique` — but the guards compare
it against sets keyed by `varId`/`stableVarId` (the MD5-based 64-bit id). The two
key spaces never coincide, so:
- `tsUnresolvedIds` (built from `uvKey`s) never contains the `varId` the
  translator looks up → the clean-error fallback never fires.
- `trulyUnresolved` (filtered by `uvKey ∈ referencedIds`, but referencedIds are
  `varId`s) is always empty → the compile guard never trips.

## Impact
Any external the resolver can't inline (no unfolding) is emitted as a plain
`NVar (stableVarId …)` and **forced at JIT runtime → unresolved_var_trap**,
instead of a clean compile error naming the symbol. `GHC.Magic.nospec` was a
victim: had this guard fired, the nospec regression would have been a one-line
compile error rather than a multi-hour runtime-trap hunt. So fixing this has high
leverage — it restores the loud-at-compile-time net for the WHOLE
unresolved-external class, not just nospec.

## Fix (proposed by core-trace)
Make `uvKey` and the guard comparisons share the `varId`/`stableVarId` key space:
set `UnresolvedVar.uvKey = varId v` (in Resolve.hs and its consumers) so
`tsUnresolvedIds` and `trulyUnresolved` are keyed the same way the translator and
`referencedIds` are. Then both guards fire and an unresolvable external becomes a
clean compile error again.

## Validation plan
- Add a deliberately-unresolvable external probe (an FFI/magic symbol with no
  unfolding and no special-case handling) and assert it produces a clean COMPILE
  error (not a runtime trap). (Pick a symbol not already special-cased — nospec
  is now handled, so it can't be the probe.)
- Confirm no regression: the existing suite still compiles/links (the guard
  firing must not false-positive on resolvable externals).
- Needs an extract rebuild + suite run.

## Status
Latent (the nospec special-case sidesteps it). Distinct from, and lower-urgency
than, the (fixed) nospec bug — but a strong "fix the mechanism" candidate: it
would have made nospec self-diagnosing.
