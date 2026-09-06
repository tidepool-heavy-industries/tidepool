# Finite Show lane contract

Seed: accepted parent 9c5a2098. Existing numeric_oracle.rs + NumericContract.hs
are the red end-to-end acceptance seam; do not weaken them or replace Show.
Finite Show fails Array.! undefined element while classifier/IEEE controls pass.

Investigative ownership before repair decisions:
* Lowering worker: trace GHC.Internal.Float floatToDigits/power-table Core through
  Translate, including State# and unboxed tuple handling. Own Translate and
  Haskell fidelity tests only if a fix is proven; no case/JIT forcing edits.
* Regression worker: minimize independent Data.Array/listArray/runST initialization
  and finite floatToDigits probes using adjacent Haskell fixtures and EvalHarness.
  Own new finite_show_array.rs + fixtures, registration in stdlib.rs. Establish
  native success and Tidepool failure/control; report mechanism evidence, not
  merely Show symptom. No production changes.
* Numeric lead: inspect JIT boxed array allocation/read/write/freeze mechanisms,
  integrate proven repairs and communicate boundary changes to strictness owner.
  Strictness sibling exclusively owns case, forcing and unboxing helpers.

Known code facts, not diagnoses: eval currently lists boxed array primitives as
unsupported; JIT implements them. Array creation/write arms carry pointer values;
read has unsigned bounds checks. floatToDigits uses arrays in GHC's library.
Clz is already mapped and JIT-supported; old comments asserting clz unavailable
are obsolete explanations, not permission to delete formatters in this wave.

## Agreed runtime repair boundary

Strictness lead explicitly approved numeric ownership of PrimOp argument traversal
and demand policy, not heap_demand/heap_force/demand_whnf_ssaval, case or unbox
helpers. Combined strictness baseline is 4bff0f58. The shared repr contract is
PrimOpKind::argument_demand(position) -> PrimArgDemand::{Strict,Lazy}, with lowered
zero-based positions; array initializers/new values and CAS identity operands
are lazy while array/size/index operands remain strict. This is a partial scaffold:
production traversal must consume it before it changes behavior. Both recursive
argument expansion and final forcing must honor demand; merely skipping the final
force still evaluates a nontrivial argument during bottom-up traversal. Lazy
arguments must be heap-boxed before array pointer storage. Reuse existing thunk
creation/GC-root machinery; no alternate array implementation.
