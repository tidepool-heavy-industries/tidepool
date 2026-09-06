# Floating-point correctness: repaired substrate and verification

## Outcome

Repaired and integrated on 2026-09-06. Exact verified source baseline:
`95e5eed6cd04f850d135b5d0ba640767310d52c4`. Subsequent report/scaffold cleanup is
documentation-only. The original report and wave contracts remain in Git.

The original Float/Double classification and Prelude Show reproductions now pass
through native-GHC comparisons, real extraction/JIT, and a freshly rebuilt hosted
workbench test. **The already-running interactive Shoal host was not replaced**;
start a rebuilt host to use the repaired runtime interactively.

## Causes and owning repairs

| Proven defect | Structural repair / owner |
|---|---|
| Missing classification FFI mappings; rendered substring recognition | Six Float/Double classifier primitives; exact static foreign-symbol recognition in `haskell/src/Tidepool/Translate.hs` |
| Lazy error closure read as a literal payload and accepted by case DEFAULT | Semantic WHNF demand at case entry, checked numeric unboxing and immediate error return in `tidepool-codegen/src/emit/{case,expr,primop}.rs` and `host_fns/force.rs` |
| Float literal case interpreted binary32 bits as binary64 | Width-correct Float comparison in case dispatch |
| Every primitive argument evaluated eagerly, including lifted array values | Typed `PrimOpKind::argument_demand` in `tidepool-repr/src/types.rs`; lazy argument subtrees retained until thunk materialization in JIT traversal |
| Supposedly trivial primitive computations evaluated in lazy positions | Conservative shared `is_trivial_field`: primitive applications remain computations, not eagerly materialized values; constructor, binding and array consumers share the rule |
| Nonzero subnormal decode returned an unnormalized significand | Shared `tidepool-bignum` decoding normalizes to GHC's 24/53-bit significand with compensating exponent |
| Serialized Float accepted ignored upper 32 bits | Shared literal decoder rejects noncanonical Float bits; valid Float/Double bits roundtrip exactly |

No special Prelude Show instance, replacement formatter, duplicate array backend,
or eager rejection of all unsupported dead code was introduced. Unused bottom
remains lazy; demanded unsupported/error values fail explicitly.

## Final evidence

Root executed these focused checks at the verified baseline, all passing:

| Selection | Passed |
|---|---:|
| Runtime native numeric/array oracle and extracted error-message controls | 17 |
| Classifier and IEEE arithmetic backend matrices | 2 |
| Primitive demand and constructor differential regressions | 7 |
| Resident error guards | 15 |
| Strict demand/heap force unit tests | 12 |
| Repr ingress, bit roundtrips and trivial-field safety | 10 |
| Shared Float/Double decode unit tests | 4 |
| `just fixtures-check` | 217 |

All selected targets compiled and executed. Workspace-edition formatting and
`git diff --check` passed. Full root command log:
`/tmp/numeric-final-root-checks.log` (local session evidence, not a shipped file).

Independent retained validation specialist executed
`just test-lib tidepool 'test(floating_point_resident_display_matches_prelude)'`
at the **same exact baseline**: one test containing all 17 probes passed
(nextest `1733f7bf-986c-4e72-b630-82de44281c75`). It covers bare/full inspection,
tuples, lists, derived numeric records, qualified Prelude Show, separate Render,
and classification. Hosted forest/task join and extractor daemon cleanup passed.
Evidence is specialist-attributed, not claimed as a second root execution.

Native oracle: repository Nix GHC 9.12.2, x86_64 Linux. Numeric tests include both
signed zeros, finite values/extrema, subnormals, infinities and NaNs. Arithmetic
NaN sign/payload preservation is not assumed; serialization compares exact bits.
See fixtures under `tidepool-runtime/tests/fixtures/floating/` and corresponding
`numeric_oracle.rs`, `finite_show_array.rs` tests for repeatable native comparisons.

## Explicit limits and follow-ups

- Reference evaluation still lacks boxed-array primitives; those controls use
  native GHC versus JIT, not fabricated evaluator parity.
- Conservative thunking may allocate more; no performance benchmark was run.
- Direct Rust `Literal::LitFloat(u64)` construction and the writer remain
  permissive; the ingress fix is not a universal in-memory invariant.
- No dedicated CAS regression or exhaustive/random decode sweep; special-value
  decode behavior outside finite numbers was not changed.
- Aeson Scientific and QQ/Fmt contain old floatToDigits/clz workaround rationale.
  `clz#` support exists and finite Show now works. Audit those paths separately,
  preserving intentional JSON formatting and Float/Double decimal distinctions;
  they were not rewritten or claimed equivalent in this repair.
- No workspace-wide release battery or live-host replacement was performed.

## Tree-of-workers experience

The campaign used committed shared contracts, independent implementation and
oracle leads, recursive worker/reviewer waves, local repair loops, exact-candidate
integration and typed surveys. Fresh review caught both defective tests and an
additional lazy-evaluation bug. The experience and unresolved orchestration
issues are recorded in
[the live dogfood notes](plans/actor-model/live-context-unfold-dogfood-followups.md#numeric-substrate-tree-live-observations-2026-09-06-ongoing).
Full typed deliveries and useful specialists remain retained in the root session;
no complete actor-retirement or cache/cost-efficiency claim is made.
