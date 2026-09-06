# Floating-point correctness: repair-wave starting contract

## Status and objective

Reproduced on 2026-09-06 in the live Shoal workbench. Source baseline:
`e818b22d` (numeric implementation unchanged by the prompt/cache work).
No floating-point repair has been integrated. Prior investigation actors are
retired; any retained candidate branches require inspection before reuse.

Repair the numeric substrate, not just its display. Use this as the first
substantial dogfooding wave for typed, recursive Haskell orchestration: shared
contracts, independently owned implementation and testing, fresh-context review,
then checked integration. Start with the failures below and expand according to
evidence into the surrounding floating-point and unsupported-operation paths.
Do not assume every neighboring primitive is broken.

## Minimal reproductions

Run in `tidepool_actor.haskell`, without substituting the default `show` for the
qualified Prelude operation:

```haskell
import qualified Prelude as P
let d = 1.0 :: Double
let f = 1.0 :: Float
(P.isNaN d, P.isInfinite d, P.isNegativeZero d, P.show d)
(P.isNaN f, P.isInfinite f, P.isNegativeZero f, P.show f)
(d == d, d > 0, d + d)
show d
```

Observed in the resident runtime:

```text
(True,True,True,"-NaN")
(True,True,True,"-NaN")
(True,True,-NaN)
"1.0"
```

Native GHC, checked independently with `ghc -e` for both Float and Double,
returns `(False,False,False,"1.0")` for the classification/Show tuple.
The arithmetic tuple should display `(True,True,2.0)`.

Earlier probes also rendered `(fromIntegral (14592 :: Int) :: Double)` as
`-NaN`. This is a rendering observation, not proof that conversion itself
produces NaN. Likewise the displayed `d + d` does not establish its underlying
bit pattern. Test arithmetic and conversion through independent observations.

## Verified distinctions and unresolved mechanism

- This is not solely a presentation defect: classification predicates themselves
  return incorrect Bool values for ordinary finite inputs.
- The resident default `show` uses `Tidepool.Render`. Its Double instance routes
  through `Tidepool.Double.renderDoublePrec`, an extractor intrinsic, and masks
  this particular failure. It is not an independent oracle for the substrate.
- Generic `WorkbenchDisplay` and `FullDisplay` in `Tidepool.Inspection` use
  ordinary Prelude `Show`. Derived/nested Show consumers must be covered too;
  a special instance for bare Double would not repair those consumers.
- `mapFfiCall` in the extractor recognizes selected foreign calls, including
  `rintDouble`, but has no floating-classification mappings. Unsupported calls
  become lazy poison nodes via `emitFfiPoison`.
- Prior investigation of GHC's Float interface identified foreign classification
  calls such as `isDoubleNaN`, `isDoubleInfinite`, `isDoubleNegativeZero`, and
  their Float counterparts. Reconfirm the exact Core names and result types
  against the active GHC/toolchain when implementing.

**Leading hypothesis:** missing classification support contributes to the bad
Prelude predicates/Show, and a poison/forcing or representation boundary lets an
unsupported computation become an ordinary truthy value instead of an honest
failure. The exact path from emitted poison to `True` is NOT yet established.
Trace it; adding mappings alone must not conceal a general error-propagation bug.

Unsupported FFI can occur in dead branches of over-collected closures. Preserve
laziness: unused unsupported code need not prevent compilation, but forcing it
must fail honestly. Do not replace lazy poison with fabricated values or reject
all dead unsupported bindings eagerly as a shortcut.

## Owning code and existing test seams

Read root and nearest nested `AGENTS.md`; stale `CLAUDE.md` is not authority.

| Area | Source / existing tests to inspect |
|---|---|
| Core/FFI lowering and poison creation | `haskell/src/Tidepool/Translate.hs`: `isFCallId`, `mapFfiCall`, `emitFfiPoison` |
| Primitive vocabulary / serialization | `tidepool-repr/src/types.rs`: `PrimOpKind`, including `FfiRintDouble` |
| Reference evaluation | `tidepool-eval/src/eval.rs` |
| JIT lowering, boxing and comparisons | `tidepool-codegen/src/emit/primop.rs`; follow actual unboxing/forcing consumers |
| Heap/value decoding | `tidepool-codegen/src/heap_bridge.rs` |
| Display paths | `haskell/lib/Tidepool/{Prelude,Render,Double,Inspection}.hs` |
| Primitive differential tests | `tidepool-codegen/tests/proptest_primops_differential.rs` |
| Extractor/runtime regressions | `tidepool-runtime/tests/stdlib_regressions_02.rs`, `gc_and_errors.rs`, `suites/stdlib.rs` |
| Existing floating display regressions | `tidepool-runtime/tests/show_double_lens_sigill.rs`, `effect_stack/show_double_10effect.rs`, `repro/repro_lit_double_case.rs` |
| Public resident display | `tidepool-actor/src/workbench_display_tests.rs`; host documentation/workbench test harnesses |

Existing tests are leads, not evidence that the affected behavior currently
passes. GHC source may be available through the Nix closure; use `ghc-pkg field
ghc-internal import-dirs --simple-output` and `ghc --show-iface` to inspect the
active interface rather than assuming symbols from a different GHC release.

## Acceptance criteria

1. The minimal Float and Double repros agree with native GHC through the real
   extractor/runtime and resident display paths.
2. Test classification for finite positives/negatives, both signed zeros,
   subnormals, finite extrema, infinities, and NaNs. Generate bit-pattern cases
   where useful; compare NaNs and signed zero by appropriate predicates/bits,
   not ordinary equality. State platform and NaN-payload assumptions.
3. Cover standard Prelude Show inside tuples, lists, and user-derived records,
   plus bare workbench display and full inspection. Test the separate Render
   path without letting it substitute for standard Show coverage.
4. Check relevant arithmetic, comparisons, Float/Double and integral conversions,
   encoding/decoding, and rounding against native semantics. Expand into adjacent
   operations when tracing or differential testing finds evidence; avoid imposing
   invented expectations on operations with unspecified or exceptional behavior.
5. Run cases through both reference evaluation and JIT. Agreement between them
   is not sufficient: both may share bad lowering. Retain a native-GHC oracle
   and end-to-end tests that include extraction.
6. Demonstrate forced unsupported operations fail explicitly and dead unsupported
   branches remain unevaluated. Exercise boxed/unboxed and case-analysis seams
   implicated by the diagnosis; no poison-as-value or silent-success fallback.
7. Keep one owner per primitive/formatter/error mechanism, clean up obsolete
   paths when superseded, and compile all changed consumers. No display-only
   workaround or duplicate formatter as the repair.
8. Use focused Nix-backed tests (`just test-lib` / `just test-target`). Run
   `just fixtures-check` after extractor translation or serialization changes;
   update fixtures only for intentional changes. Put substantial Haskell test
   programs in adjacent fixtures. Run formatting and `git diff --check`.

## Proposed wave: build instruments, not just patches

The coordinator first verifies the lowering/poison path and commits enough shared
primitive semantics and test interfaces to unblock independent consumers. Then
fork coherent leads for lowering/representation, evaluator/JIT behavior, and
adversarial differential/end-to-end testing. Each may recurse when its own
contracts support independent work. Assign shared enums, manifests and wiring to
one owner; do not start conflicting edits under a large arbitrary headcount.

Use the resident language as working machinery:

- Define typed numeric cases, observations, discrepancies, and acceptance results.
- Retain generators, oracle adapters, classifiers and minimizers as callable
  values where supported; compose them with `map`, `traverse`, folds and `Await`.
- Give small workers narrowly scoped typed tasks and, where supported, a
  task-specific Haskell interface rather than a repeated long manifesto.
- Fork exact-context specialists for semantic decisions and repair ownership.
  Small-context workers are an explicit different dispatch choice, not a lossy
  replacement for shared-context inheritance.
- Do not assume new custom-tool or effect capabilities already exist. Build on
  live APIs; genuinely new effects require a real Rust interpreter and authority
  boundary, not a stub reporting success.
- Fork reviewers against exact candidates. Keep reviewer/implementer repair
  dialogue local; return commits, decisive evidence, uncertainty and reusable
  testing machinery to the coordinator. Verify the merged revision.

Use Low by default for bounded obligations and choose higher effort deliberately
for the uncertain semantic seams. Source baselines, delivery, incorporation and
checked revisions remain separate facts. Every assignment must end with its typed
result or a registered concrete dependency—not an unowned “implementation next.”

## Launch conditions

Goals-off policy and shared prompt changes are integrated; restart with the
rebuilt Shoal host before launching this wave. Native Codex goals stay disabled
on all nodes. The separate cache control preserved all eight parent input items
and reused 15,488 / 16,010 child-first-response tokens (96.7%); this is evidence
from one controlled pair, not a universal cache guarantee or a numeric test.

This document is the handoff, not authorization to launch workers before the
user's restart. Once repaired, move standing invariants into owning source/tests
and contributor guidance; Git retains this investigation history.
