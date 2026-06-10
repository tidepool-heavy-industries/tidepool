# Proptest findings — JIT effect dispatch (W5 jit-dispatch)

Differential / scripted-session fuzzing of the JIT effect-dispatch loop
(`tidepool-codegen/src/jit_machine.rs:259-430`) and the
`EffectHandler`/`DispatchEffect` HList tag routing
(`tidepool-effect/src/dispatch.rs:212-280`), against the tree-walking
`EffectMachine` (`tidepool-effect/src/machine.rs`) as a differential oracle.

Test: `tidepool-codegen/tests/proptest_jit_dispatch.rs`
Seeds: `tidepool-codegen/tests/proptest_jit_dispatch.proptest-regressions`

## How it works

A *case* = `(program, script)`:

* **Program** — a hand-built freer-simple effect tree generalizing
  `proptest_effect_machine.rs`'s `E(Union(tag, req), Leaf(\x -> …))`
  constructors: chains of 1–6 sequential effects whose continuations thread
  each response into a ground-typed computation (integer sum, or a `case` that
  reduces a list response to its head). Tags drawn from valid `0..4` ∪ invalid
  `4..=255` (255/N/N+1 forced — cf. the `nested_mapm_tag255` off-by-one
  history).
* **Script** — one response per dispatch: `Complete(small int)`,
  `Complete(huge cons spine ≥ 2000)`, `Stream` at chunk boundaries
  (255/256/257/4096 via `respond_stream`), handler `Err` at position *k*
  (trampoline error path), and shape-mismatched resume values (string where an
  `Int#` is expected).

Both machines are driven by **two separate** transcript-recording handler-HList
instances over the **same** deterministic script (handlers are stateful — never
shared between machines).

### Oracles

1. **Transcript differential** — same `(program, script)` through
   `JitEffectMachine` AND `EffectMachine`; compare final values
   (`tidepool_testing::proptest::values_equal`) **and** the dispatch sequence
   (ordered handler indices). *Transcript divergence is a bug even when final
   values agree.*
2. **Run-twice determinism** — the JIT is run twice per case (B4).
3. **Fork-per-case crash isolation** — each case runs in `libc::fork`ed child
   (`libc` is already a unix dep of `tidepool-codegen`; not added). The child
   runs the JIT phase first, writes a survival **marker**, then the eval oracle,
   then a fixed verdict record. The parent attributes outcomes by **byte
   presence**, not `WIFSIGNALED`: the in-process signal handler converts a fault
   to a *thread* `SYS_exit` (`signal_safety.rs:316-330`), so a real signal would
   otherwise masquerade as a clean exit. No marker ⇒ JIT fault (B3); marker but
   no record ⇒ eval-side fault (known-divergence skip).

### Bug classes

| Class | Meaning |
|-------|---------|
| **B1** | both machines succeed, final values differ |
| **B2** | JIT errors/diverges where eval succeeds, outside the whitelist |
| **B3** | any fatal signal / uncaught fault (verdict absent) — always reportable |
| **B4** | JIT run-twice nondeterminism |
| **B-transcript** | dispatch-sequence divergence between the machines |

Known-divergence filters (NOT bugs): eval-side errors/faults on synthetic
programs; `HeapOverflow` from the tiny nursery.

## Coverage

`coverage_and_transcript_audit` (deterministic, RNG-free): 204 valid-tag
arithmetic chains, **204/204 (100%)** reach final-value comparison (≥ 80%
required), **204 transcript comparisons performed, all agreed** — the explicit
counter proving sequences (not just final values) are compared.

Property case counts: `full_differential` 200, `huge_complete_and_stream` 100,
`err_at_k` 100, `invalid_tag_never_signals` 100, `shape_mismatch_resume` 100.

## Findings

| # | Class | Component | Status | Observed | Expected | Seed / repro |
|---|-------|-----------|--------|----------|----------|--------------|
| 1 | B2 (silent garbage) | JIT dispatch resume → `value_to_heap`(string) + compiled `IntAdd` unchecked unbox | **OPEN**, captured | JIT `run` returns `Ok(Lit(LitInt(<heap-pointer + 7>)))` — a *different garbage int each run*; eval returns `Err(EffectError::Eval(..))` | A continuation forcing a response of the wrong runtime shape (string where `Int#` expected) yields a clean error, never a silently-wrong value | proptest `cc ee1877d8…84337f0`, shrinks to `Str("")`; `#[ignore]` repro `bug_shape_mismatch_jit_reads_string_as_int` |

### Finding 1 — JIT reads a string response as `Int#`

Minimal program: `E(Union(0, 0), Leaf(\x -> Val(x +# 7)))` with the tag-0
handler answering `Complete(Lit(""))`.

The eval oracle rejects `string +# Int#` with a clean `EffectError::Eval`. The
JIT resume path materializes the string heap object and the compiled `IntAdd`
primop **unboxes its pointer word as a raw `Int#`** and adds 7, returning a
pointer-derived garbage integer — no clean error, no recoverable trap, just a
wrong answer (and a nondeterministic one, since it is a live heap address).

* **Trigger requires ill-typed Core** — a handler whose response type disagrees
  with the continuation's expectation. Well-typed GHC output cannot reach this,
  so it is a **defensive-robustness gap**, not a miscompile of valid programs.
  Severity: low; but per the task contract ("shape-mismatched resume values …
  clean error required, not a trap") it is the specified hunt and is reportable.
* **Not a fault** — it neither traps (B3) nor crashes; the live
  `shape_mismatch_resume` property asserts only the no-fatal-fault guarantee the
  JIT *does* honor, keeping the suite green. The strict contract (JIT must error
  like eval) is asserted in the `#[ignore]`d repro, which fails on demand
  (`cargo test -p tidepool-codegen --test proptest_jit_dispatch -- --ignored`).
* **Fix direction (not applied — out of scope):** a runtime shape check on the
  unbox in the compiled integer primops (or at the resume `value_to_heap`
  boundary) so a non-`Int#`/`Lit` response surfaces a clean error instead of
  reading the pointer word.

## What did NOT diverge (negative results)

* **Tag routing** — invalid tags `4..=255` (incl. 255) produce a clean
  `EffectError::UnhandledEffect` with the **same decremented tag** on both
  machines, and **never a fatal signal**. No off-by-one sibling of the
  `nested_mapm_tag255` bug was found.
* **Huge `Complete` spines (≥ 2000)** — `probe_list_spine` /
  `dismantle_list_spine` / re-park path agrees with the eager eval drain on the
  reduced head; no fault, no spine-Drop overflow inside the fork.
* **Streams at chunk boundaries** (255/256/257/4096) — parked-iterator
  conversion agrees with the eager drain.
* **Handler `Err` mid-chain** — both machines stop at the same dispatch index;
  transcripts match; no fault on the trampoline error path.
* **Determinism** — no B4 nondeterminism on valid programs.

## One deviation from the boundary, and why

The task said *do not edit Cargo.toml*; the oracle design requires driving
**both** machines through the **real** `DispatchEffect` impl, which exists only
on frunk's `HCons`/`HNil` with no reachable re-export. A hand-rolled dispatcher
would test a mirror of the routing, not the real code. The minimal,
production-safe resolution is a **test-only `frunk` dev-dependency** in
`tidepool-codegen/Cargo.toml`; the production crate graph is unchanged. Flagged
in a Cargo.toml comment and the test module header.
