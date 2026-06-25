# Bug class: spec-fallback bare-alias drops an erased dictionary arg → PAP → case trap

**Status:** root-caused 2026-06-22 (`fix/ne-group-case-trap`, b792ae4). Fix DEFERRED (not low-risk).
**Symptom:** `NE.group [1,1,2,3,3,3::Int]` → `signal-crash: case trap … (tag mismatch)`
(repro: `tidepool-runtime/tests/repro_ne_group.rs`, `#[ignore]`'d → `--ignored` gives
`unexpected heap tag: 0` on the pristine deployed binary).

## Root cause (evidence-backed)
1. GHC's `"SPEC groupBy @[]"` rule rewrites `NE.group @Int` → base's specialized binding
   `groupBy_$sgroupBy :: (a->a->Bool) -> [a] -> [NonEmpty a]` (**arity 2**).
2. base ships `groupBy_$sgroupBy` with **NO unfolding and NO fat interface**
   (`[fat-iface] Data.List.NonEmpty: no mi_extra_decls`).
3. So `Tidepool.Resolve.attemptSpecFallback` despecializes the OccName
   `groupBy_$sgroupBy` → `groupBy` and emits a **bare alias**
   `groupBy_$sgroupBy = groupBy`, pointing at the *generic*
   `groupBy :: Foldable f => (a->a->Bool) -> f a -> [NonEmpty a]` (**arity 3**).
4. The generic's first VALUE arg is the `Foldable` dictionary, which the specialization
   eliminated. The bare alias drops it: every 2-arg call **under-saturates** the arity-3
   generic → returns a **partial application**. The PAP flows where `[NonEmpty a]` is
   expected; the enclosing list `case` reads the closure's heap tag (0) → CASE TRAP.

NOT a VarId/DataConTable collision (audit: 0 collisions; NonEmpty's `groupBy` and Data.Text's
have distinct stableVarIds). Confirmed via `attemptSpecFallback` instrumentation:
specMod=genNameMod=Data.List.NonEmpty (correct target) but **specArity=2 vs genArity=3**.

**The class:** any boot-lib `SPECIALISE` binding that (a) erases a dictionary value arg,
(b) has no unfolding/fat-iface, and (c) is reached via its SPEC rule. Candidates:
`NE.group`/`groupBy`/`groupBy1`/`groupWith`, probably others.

## Proposed fix (deferred — needs greenlight)
When `idArity genId > idArity specVar`: find the SPEC rule on `genId` (`eps_rule_base`) and
rebuild `specVar = \@tv -> genId @T @tv <concreteDict>`, where each universally-quantified
dict binder in the rule's `ru_args` is replaced by a concrete instance dict resolved via
`lookupInstEnv` on the binder's class-pred type (the rule's dict is a *pattern binder* — no
concrete dict is readable from the rule/body/absent-fat-iface, so it must be looked up).

- **Net-new GHC-API surface** with no precedent here: `InstEnv`/`lookupInstEnv`/
  `InstEnvs{ie_visible}`/dfun-arg handling; version-specific shapes. High iteration surface
  for a resolver shared by **every eval**.
- **Provably non-regressing** if gated on `genArity > specArity` — that condition only holds
  for already-broken dict-erasing specializations; arity-matched aliases are untouched.
- A bare arity-guard alone does NOT fix base's `NE.group` (declining → fat-iface absent →
  poison → still traps), so no half-measure: the dict reconstruction is the real fix.

## Gate
Full workspace suite + the JIT-vs-eval differential proptests green; flip `repro_ne_group.rs`
from `#[ignore]` to a live assertion (`NE.group [1,1,2,3,3,3]` → `[[1,1],[2],[3,3,3]]`).
