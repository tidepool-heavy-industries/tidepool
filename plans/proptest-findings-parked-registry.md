# W4/W5 parked-stream registry property findings (bug map)

Property suite: `tidepool-codegen/tests/proptest_parked_registry.rs`.
Date: 2026-06-10. Workstream W5 (ParkedStream registry + stream_chunk).

## Method

Each property case is interpreted by the `JitEffectMachine` driven by deterministic adversarial `ValueSource` implementations. Track A uses unit `heap_force` directly to probe thunk-level mechanics (memoization, blackholing). Track B uses full JIT dispatch over hand-built Core trees.

## Properties & Verification Status

| ID | Property | Status | Finding |
|:---|:---|:---|:---|
| **P1** | Model Equivalence (Seq/Idx) | **PASS** | Validated for fenceposts 0/1/255/256/257. |
| **P2** | Laziness Quantification | **PASS** | EXACT counts verified: Seq(3)=256, Idx(3)=3, Walk(280)=0, ForceTwice=1. |
| **P3** | Registry Isolation | **PASS** | Two interleaved streams do not cross-talk. |
| **P4** | Abandon/Re-enter | **PASS** | RegistryGuard cleans up; no state resurrection on machine reuse. |
| **P5** | Teardown Universality | **PASS** | All sources dropped even on early exit or panic. |
| **P6** | Panic/Contract Containment | **PASS** | Producer panics and lying lengths trapped as `UserErrorMsg`. |
| **P7** | Forked GC Safety | **PASS** | GC mid-chunk with tiny nursery (512KB) survived. |

## Bug Table (Verified Negative)

| Bug | Class | Property | Reproduction | Status |
|:---|:---|:---|:---|:---|
| — | B1 | P1 | — | **NEGATIVE** |
| — | B2 | P6 | — | **NEGATIVE** |
| — | B3 | P7 | — | **NEGATIVE** |
| — | B5 | P2d, P3 | — | **NEGATIVE** |

## Route Inventory (Unreachable pub(crate) surface)

The following `pub(crate)` items in `host_fns.rs` were exercised exclusively via `JitEffectMachine::run` and `heap_force` dispatch:

- `ParkedStream` (Struct)
- `park_stream` (Function)
- `clear_parked_streams` (Function)
- `stream_chunk` (Function)
- `stream_element` (Function)
- `materialize_cons_list` (Function)
- `PARKED_STREAMS` (Static)
- `STREAM_NEXT_ID` (Static)
- `ReadySource` (Struct)

## Execution

```bash
cargo test -p tidepool-codegen --test proptest_parked_registry -- --nocapture
```
--test-threads=1 is not strictly required as the registry is thread-local, but recommended for clean diagnostic output.
