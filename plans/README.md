# tidepool — Haskell-in-Rust via Cranelift

Compile freer-simple effect stacks into Cranelift-backed state machines drivable from Rust. Haskell expands, Rust collapses. The language boundary is the hylo boundary.

## Dependency Graph

```
phase-1/core-repr
    ↓
phase-2/ (parallel where deps allow)
  ├── core-eval        (needs CoreFrame)
  ├── core-heap         (needs HeapObject from core-eval scaffold)
  ├── core-optimize     (needs working evaluator from core-eval)
  ├── core-bridge       (needs CoreFrame + Value from core-eval scaffold)
  └── core-testing      (generators early; oracle + bench need eval)
    ↓
phase-3/codegen         (needs eval + heap + optimize)
```

**Sequencing within phase 2:** core-eval scaffolds first (defines HeapObject, Heap trait, Value). core-heap and core-bridge can start after that scaffold lands. core-optimize needs the full evaluator as its test oracle. In practice, core-eval runs first, then the others fan out.

## Orchestration Model

### Primitives

| Primitive | Tool | Isolation | Use When |
|-----------|------|-----------|----------|
| **Claude subtree** | `spawn_subtree` | Own worktree + branch + tab | Coordination nodes. Dispatches workers, reviews, re-specs on failure. Never writes implementation code. |
| **Gemini worker** | `spawn_workers` | Same dir as parent TL | Most implementation work. Fast, cheap, no branch overhead. TL commits after each wave. |
| **Claude subtree with workers** | `spawn_subtree` + `spawn_workers` inside | Own worktree, Gemini workers within | Cross-language or judgment-heavy work. Claude provides course correction, Gemini does the typing. |

### Default: Workers, Not Worktrees

Workers are the default unit of implementation:

- Workers run in the TL's worktree. No branch, no PR, no merge step.
- TL commits after each successful wave (`cargo test --workspace` green).
- Workers can build on each other's output within a wave (they share the filesystem).
- TL should commit before spawning workers on anything risky (escape hatch if worker trashes the tree).

Reserve Claude subtrees (with their own workers inside) for:
- **Cross-language work** (Haskell leaves need GHC judgment + nix shell)
- **Judgment-heavy tasks** where Gemini needs Claude course-correction
- **High-risk/large tasks** where git isolation prevents damage

### TL Lifecycle

Each TL spec doc describes waves of work. A TL's job:

1. Read its spec doc
2. Scaffold wave: spawn 1-2 workers to write types/traits/signatures. Review output. Commit.
3. Implementation waves: spawn parallel workers for independent tasks. Each worker gets a focused prompt with exact code snippets, file paths, and test commands. Commit after each wave.
4. Verify: `cargo test --workspace` after every commit. If red, diagnose and re-spec the failing piece.
5. When all waves complete, file PR against parent branch.

### Failure Protocol

Worker failure (build breaks, tests fail after worker finishes):
1. TL reads the diff, identifies the problem
2. TL writes a sharper prompt addressing the specific failure
3. TL spawns a fresh worker with the improved prompt
4. After 3 failures on the same task: split it smaller or escalate to human

### Quality Gates

Some scaffolds need TL review before downstream work starts (marked "gate" in specs). At gates, the TL:
- Reads the scaffold output
- Verifies type signatures match the locked decisions
- Runs `cargo test`
- Commits if clean, re-specs if not
- Only then spawns the next wave

## Tree Shape

```
main [Human]
│
├── core-repr [Claude TL, depth 1]
│     Workers: scaffold, frame-utils, types-datacon, serial, pretty
│     Subtree: haskell-harness [Claude, depth 2]
│       └── Workers: ghc-api-harness, core-serializer, wiring
│
├── core-eval [Claude TL, depth 1]
│     Workers: scaffold, eval-strict, eval-case, thunks, join-points
│
├── core-heap [Claude TL, depth 1]
│     Workers: scaffold+arena, gc-trace, gc-compact
│
├── core-optimize [Claude TL, depth 1]
│     Workers: scaffold+occ+beta+case-reduce, inline (coalg+alg),
│              dce, partial (subst-hylo, reduce-hylo)
│
├── core-bridge [Claude TL, depth 1]
│     Workers: scaffold, traits, derive-parse, derive-codegen, haskell-macro
│
├── core-testing [Claude TL, depth 1]
│     Workers: scaffold, generators, differential, bench
│
└── codegen [Claude TL, depth 2 — child of core-eval branch]
      Workers: scaffold, codegen-expr, case-and-join, gc-integration, yield
```

## Statistics

```
Claude subtrees:  8  (core-repr, haskell-harness, core-eval, core-heap,
                      core-optimize, core-bridge, core-testing, codegen)
Gemini workers:  ~35  (most implementation work)
Max depth:        2  (main → core-repr → haskell-harness, main → core-eval → codegen)
Max parallelism: ~6-8  concurrent workers across active TLs
```

## Docs

| File | Contents |
|------|----------|
| `decisions.md` | Locked design decisions (CoreFrame, HeapObject, GHC pipeline) |
| `phase-1/core-repr.md` | CoreFrame types, CBOR serial, pretty printer, Haskell harness |
| `phase-2/core-eval.md` | Tree-walking evaluator (strict, case, lazy, thunks, join) |
| `phase-2/core-heap.md` | Arena allocator + copying GC |
| `phase-2/core-optimize.md` | Optimization passes + first-order partial eval |
| `phase-2/core-bridge.md` | FromCore/ToCore traits, derive macros, haskell! macro |
| `phase-2/core-testing.md` | Proptest generators, GHC differential oracle, benchmarks |
| `phase-3/codegen.md` | Cranelift backend + EffectMachine |
| `anti-patterns.md` | Shared base anti-patterns for all workers |
| `research/01-freer-simple-core-output.md` | **COMPLETE:** actual -O2 Core structure from freer-simple (GHC 9.12.2) |
| `research/02-cranelift-stack-maps-jit.md` | **COMPLETE:** Cranelift stack map semantics + JIT pipeline (cranelift 0.116.1) |
| `research/03-ghc-912-api-surface.md` | **COMPLETE:** GHC 9.12 API for harness + freer-simple compatibility |
