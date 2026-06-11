# S6 varid-defense — findings

Workstream: make the #313 bug class (silent top-level VarId shadowing)
structurally impossible to reintroduce. Part A: load-time detector. Part B:
property tests + collision-resistance statistics.

## Part A — detector design

### Hook point: `JitEffectMachine::compile` (tidepool-codegen)

Candidates considered:

| Hook | Verdict | Why |
|---|---|---|
| `serial::read_cbor` (tidepool-repr) | rejected | `read_cbor` is a *format* decoder, also used to roundtrip arbitrary generated trees (`proptest_cbor`, `generated_exprs_roundtrip_cbor`). A semantic invariant there would false-positive on legitimately-arbitrary trees and conflate "malformed CBOR" with "malformed program". |
| `compile_haskell` (tidepool-runtime) | rejected | Covers the runtime path only; direct embedders of `JitEffectMachine::compile` (codegen tests, MCP server internals) would bypass the check. Also sits in front of the cache, so cached loads would skip it. |
| `JitEffectMachine::compile` (tidepool-codegen) | **chosen** | The single choke point every consumer passes through before emit: `compile_and_run*`, `compile_and_run_pure`, MCP evals, and all codegen tests. Runs on the raw deserialized tree (the `wrapAllBinds` Let-nest) *before* `normalize`/`wrap_with_datacon_env` reshape it. |

The detector itself (`check_toplevel_varids`) lives in **tidepool-repr**
(`varid_check.rs`) because the Let-spine shape is a repr-level invariant of
the serializer, and so the fixture-corpus sweep can call it without a
codegen dependency.

### Semantics

`Translate.wrapAllBinds` emits top-level bindings as a Let-nest:
`LetNonRec`/`LetRec` frames chained through their `body` edges, terminating
in `NVar(target)`. The walk follows only `body` edges from the root and
stops at the first non-Let frame. Consequences, by construction:

- **Every spine binder is a distinct GHC top-level binding** → a duplicate
  VarId on the spine is always an identifier collision (the #313 class),
  never legitimate shadowing. ERROR.
- **Nested binders (lambdas, lets inside RHSs) are never visited** →
  legitimate shadowing in nested scopes cannot fire the check. IGNORE.
- O(spine length) with one hash map; a step guard bounds the walk on
  malformed (cyclic) `body` edges, and an identical-site repeat is not
  reported as a collision (same binder appearing once never fires).

Error type: `tidepool_repr::VarIdCollision` — carries the colliding VarId
(hex) and both binding sites (node index + binder position), surfaced as
`JitError::VarIdCollision` with a pointer at the Haskell-side scheme.

Kill-switch: `TIDEPOOL_VARID_CHECK=0` disables (default ON). Intended only
for bisection — distinguishing "the detector is wrong" from "the program is
genuinely colliding".

### Why the existing suites cannot false-positive (calibration analysis)

- `tidepool-testing`'s generators allocate binder VarIds from a shared
  `Rc<Cell<u64>>` counter (`Context::add_var`), so generated trees are
  globally collision-free — every codegen proptest that compiles generated
  trees stays green with the check ON.
- Hand-built shadowing tests in the suites shadow **JoinIds under Join
  frames** (`test_join_nested_inner_shadows`) or nested binders — not the
  top-level Let spine.
- Empirical: full `cargo test -p tidepool-codegen`, `-p tidepool-eval`,
  `-p tidepool-repr`, and `-p tidepool-runtime --test integration` run with
  the detector ON by default. Results below.

## Part B — corpus sweep + statistics

### Wild-duplicate sweep (REPORTABLE gate)

Sweep of every committed fixture in `haskell/test/suite_cbor/` (154 `.cbor`
expression fixtures; `meta.cbor` excluded as it is the DataConTable):

- **Zero fixtures contain a duplicate top-level VarId.** #313 has no
  siblings in the committed corpus.
- 153/154 fixtures have a Let spine (the remaining one is a bare
  expression); **723 top-level binder occurrences** total, all unique
  within their fixture.
- Across the union of fixtures: 380 distinct VarIds for 723 occurrences —
  cross-fixture repeats are expected and benign (the same Prelude binding
  serialized into many closed fixtures hashes to the same stable VarId;
  identity of (module, occName) ⇒ identity of VarId is the *point* of the
  scheme).

### Bug table

| # | Finding | Severity | Status |
|---|---|---|---|
| 1 | No wild duplicate top-level VarIds in any committed fixture | — | verified negative |
| 2 | Pre-existing (not this branch): `cargo test -p tidepool-eval` did not COMPILE at the fork point — commit 7bb112c added `suite_int!(round_*)` entries + Suite.hs bindings but never committed the four `round_*.cbor` fixtures | suite-blocking | FIXED here: regenerated via `tidepool-extract-bin test/Suite.hs --all-closed`, copied only the 4 missing fixtures (regenerated `meta.cbor` was byte-identical to the committed one; all other fixtures left untouched). haskell_suite 152/0 after. |

(Verified-negative table extended by the property-test results below.)

### Birthday-bound analysis (56-bit truncated fingerprint)

The scheme (`Translate.stableVarId` / `localVarId`) truncates a 128-bit GHC
`Fingerprint` to its low 56 bits (`h1 .&. 0x00FFFFFFFFFFFFFF`); stable IDs
additionally tag byte 7 with `0xFE`, so all top-level binders live in one
2^56 ≈ 7.21e16 space and collide only within it.

For n uniformly distributed hashes in d = 2^56 slots, the collision
probability is p ≈ 1 − exp(−n(n−1)/2d) ≈ n²/2^57:

| n (top-level bindings in one program) | p(collision) |
|---|---|
| 723 (entire committed corpus at once) | 3.6e-12 |
| 10,000 (large eval w/ full stdlib closure) | 6.9e-10 |
| 100,000 (far beyond any current program) | 6.9e-8 |
| 1,000,000 | 6.9e-6 |

Even at 100× the realistic program size, the hash-width collision odds are
below 1e-7 per compile. **#313 was not a hash-width failure**: it hashed a
non-unique *input* — `(occName, per-module unique-key)` collides across
modules by construction when `runPipeline` concatenates several modules'
binds. The fix (2d0ca80, `externalizeInternalTops`) makes the input
globally unique; the residual risk the detector defends against is exactly
that class — a future change quietly reintroducing a non-injective input to
the fingerprint — which no hash width can fix and which the detector
catches at load, loudly, with both occurrence sites named.

### Property-test results

(filled in by the Part B test run; see
`tidepool-repr/tests/proptest_varid_defense.rs`)

- Planted-collision catch rate: 100% (500/500 caught; correct sites and VarId reported)
- Clean generated spines, false positives: 0% (500/500 passed)
- Nested-shadowing immunity: verified (500/500 passed; binders inside RHS or under Lam never trigger)
- Full-suite calibration: verified (154/154 fixtures pass; 723 top-level binders total)

## Detector design gaps / non-goals

- The check runs at JIT load; the tree-walking interpreter
  (`tidepool-eval`) does not call it. The corpus sweep covers the fixtures
  it consumes, and the eval path shares the same serializer, but a
  collision would not be trapped at eval-time. Acceptable: the JIT is the
  production path.
- Cross-*program* VarId reuse (same id in two different `.cbor` files) is
  benign and out of scope; only within-program spine duplicates are the
  #313 class.
- The detector cannot distinguish "two distinct bindings, colliding hash"
  from "the same binding serialized twice" (identical RHS). The serializer
  never emits the latter (`reachableBinds` dedups), so both are treated as
  errors; if a future serializer change legitimately duplicates bindings,
  the error message names both sites for fast diagnosis.
