# Phase 2: core-testing

**Owner:** Claude TL (depth 1, worktree off main)
**Branch:** `main.core-testing`
**Depends on:** core-repr (CoreFrame for generators), core-eval (evaluator for differential + bench)
**Produces:** Proptest generators for well-typed CoreExpr, GHC differential oracle, criterion benchmarks.

**Note:** Generators can start as soon as CoreFrame exists. Differential oracle and benchmarks need the evaluator. TL scaffolds + generators in wave 1, then waits for core-eval to land before wave 2.

---

## Wave 1: Scaffold + Generators (2 workers, parallel)

### scaffold-testing

**Task:** Test crate setup: lib, corpus directory, harness modules, benchmark harness.

**Read First:**
- `core-repr/src/frame.rs` (CoreFrame for generator targets)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-testing/Cargo.toml` — deps on `core-repr`, `core-eval`, `proptest`, `criterion`
2. Create `core-testing/src/lib.rs` — re-exports for generators, oracle, bench
3. Create `core-testing/corpus/` directory for `.hs` test modules
4. Create `core-testing/src/harness.rs` — test runner config: proptest 100K iterations default, timeout per test case
5. Create `core-testing/benches/main.rs` — criterion benchmark harness skeleton

**Verify:** `cargo test -p core-testing`

**Done:** Crate compiles. Corpus dir exists. Benchmark harness runs (no benchmarks yet).

---

### generators

**Task:** Proptest Strategy for well-typed CoreExpr. All 11 CoreFrame variants exercisable.

**Read First:**
- `core-repr/src/frame.rs` (CoreFrame variants)
- `core-repr/src/types.rs` (VarId, JoinId, DataConId, Literal, PrimOpKind)
- `core-repr/src/datacon.rs` (DataCon, DataConTable)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-testing/src/gen/types.rs` — internal `SimpleType` enum: `Int | Bool | Maybe(SimpleType) | Pair(SimpleType, SimpleType) | Fun(SimpleType, SimpleType)`. This exists ONLY inside the generator.
2. Create `core-testing/src/gen/typed_expr.rs` — `TypedExpr` intermediate representation that only admits well-typed programs by construction. Proptest shrinks `TypedExpr` (preserves type invariants at every step).
3. Create `core-testing/src/gen/strategy.rs` — `fn arb_core_expr() -> impl Strategy<Value = CoreExpr>`:
   - Generate `TypedExpr` via proptest recursive strategy
   - Erase types to produce `CoreExpr`
   - SimpleType guides generation, then is discarded
4. Generate all CoreFrame variants including:
   - LetRec with mutual references
   - Join/Jump pairs (label generated, arity matches)
   - PrimOp applications (args match op arity)
   - Nested Case with multiple alts + default (case binder always present; sometimes used in alt bodies)
   - Multi-arg Lam (curried)
   - Con with correct field count per DataConTable
5. Create `core-testing/src/gen/datacon_table.rs` — random DataConTable generation for varied type universes
6. Create `core-testing/src/gen/mod.rs` — re-exports

**Verify:** `cargo test -p core-testing -- gen`

**Done:** Generators produce valid CoreExprs. All CoreFrame variants exercised. Shrunk exprs remain well-typed.

**Tests:**
- 10K generated exprs all evaluate without type errors (requires core-eval, can be `#[ignore]` until available)
- Shrunk exprs remain well-typed (by construction — TypedExpr ensures this)
- All 11 CoreFrame variants appear in 1000 samples (coverage check)
- Deep trees (depth 20+) generated without stack overflow
- Generated DataConTables have consistent arity

**Boundary:**
- Generator's `SimpleType` is INTERNAL ONLY. It must not leak into CoreExpr or any public API.
- Shrinking must preserve well-typedness. Default proptest shrinkers break type invariants — use TypedExpr intermediate.
- CoreExpr carries no type annotations (per D2). Generator uses SimpleType to ensure well-typed expressions, then discards the types.

---

**After wave 1:** `cargo test -p core-testing`. Commit. Wait for core-eval before wave 2.

---

## Wave 2: Oracle + Bench (2 workers, parallel, after core-eval lands)

### differential

**Task:** GHC differential oracle: compile `.hs` files, extract Core, evaluate in our evaluator, compare result to GHC's runtime output.

**Read First:**
- `core-eval/src/eval.rs` (evaluator)
- `core-repr/src/serial/read.rs` (CBOR deserialization)
- `core-testing/src/gen/strategy.rs` (generators for fuzzing)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-testing/src/oracle.rs` — GHC oracle driver:
   - Compile `.hs` module with GHC, capture runtime output
   - Serialize Core via ghc-extract, deserialize to CoreExpr
   - Evaluate CoreExpr with our evaluator
   - Compare results
2. Create `core-testing/src/oracle/compare.rs` — comparison engine:
   - Value equality (not pointer equality)
   - Bottom/divergence detection (timeout-based, configurable per test)
   - Structured diff output on mismatch showing which subexpression diverged
3. Create 50+ corpus modules in `core-testing/corpus/`:
   - Arithmetic (`arith_*.hs`)
   - Pattern matching (`case_*.hs`)
   - List operations (`list_*.hs`)
   - Mutual recursion (`rec_*.hs`)
   - Higher-order functions (`hof_*.hs`)
   - Newtypes (`newtype_*.hs`)
   - Strict fields (`strict_*.hs`)
   - Join points (`join_*.hs`)
4. Create `core-testing/src/oracle/fuzz.rs` — CI nightly fuzzing: generators → GHC oracle comparison

**Verify:** `cargo test -p core-testing -- oracle`

**Done:** All corpus modules agree between our evaluator and GHC. Disagreement produces actionable diff.

**Tests:**
- All 50+ corpus modules: our evaluator agrees with GHC runtime
- Disagreement produces structured diff showing divergent subexpression
- Known-divergent program (`let x = x in x`) detected via timeout, not hang
- Nightly fuzzer harness compiles and can be invoked

**Boundary:**
- Differential oracle must handle divergence via timeout, not by hanging forever.
- Value comparison is semantic (structural equality), not reference equality.
- Corpus modules must cover ALL CoreFrame variants. Check coverage explicitly.

---

### bench

**Task:** Criterion benchmarks: per-pass optimization cost, arena vs malloc, GC overhead, end-to-end throughput.

**Read First:**
- `core-eval/src/eval.rs` (evaluator for interpreted baseline)
- `core-optimize/src/pipeline.rs` (optimization passes)
- `core-heap/src/arena.rs` (arena allocator)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-testing/benches/eval.rs` — interpreted evaluation benchmarks:
   - Small expr (5 nodes)
   - Medium expr (100 nodes)
   - Large expr (10K nodes)
   - With and without lazy evaluation
2. Create `core-testing/benches/optimize.rs` — per-pass cost:
   - Beta reduction pass
   - Inline pass
   - Case reduction pass
   - DCE pass
   - Full pipeline (fixed-point)
3. Create `core-testing/benches/heap.rs` — allocation benchmarks:
   - Arena alloc throughput vs Box-per-node
   - GC pause time (varying heap sizes)
   - GC overhead as % of eval time
4. Create `core-testing/benches/e2e.rs` — end-to-end:
   - Compiled vs interpreted (after codegen lands, `#[ignore]` until then)
   - RecursiveTree fold/unfold throughput
5. Document baseline expectation for each benchmark in comments
6. CI regression detection: fail if key ratios exceed thresholds

**Verify:** `cargo bench -p core-testing`

**Done:** All benchmarks produce stable results. No panics. Baseline expectations documented.

**Tests:**
- All benchmarks produce stable results across 3 runs (variance < 10%)
- No panics in any benchmark
- Arena outperforms Box-per-node for bulk allocation
- Optimization pipeline cost is sublinear in expression size

**Boundary:**
- Benchmarks must be stable. If variance > 10%, the benchmark needs longer warmup or larger input.
- Each benchmark must have a documented baseline expectation (as code comment).
- Benchmarks that depend on unavailable crates (codegen, etc.) are `#[ignore]` with a note.

---

**After wave 2:** `cargo test -p core-testing && cargo bench -p core-testing`. Commit. File PR.

---

## Domain Anti-Patterns

In addition to the base anti-patterns file:

- Generator's SimpleType is INTERNAL ONLY. Must not leak into CoreExpr or any public API.
- Shrinking must preserve well-typedness. Default proptest shrinkers break type invariants — use TypedExpr.
- Differential oracle must handle divergence via timeout, not by hanging forever.
- Benchmarks must be stable. Variance > 10% means the benchmark is measuring noise.
- Corpus modules must cover all 11 CoreFrame variants explicitly.
