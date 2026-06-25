# Code Health Report

Generated 2026-06-22 via tidepool dogfooding (machine-native Haskell analysis).
Findings from ~6 evals scanning the full workspace.

## 1. Duplication (178 intra-file pairs >65% overlap, 7 extractable)

LLM-triaged the top 8 pairs in a single batch call. 7 confirmed extractable.

| What | File | Lines | Overlap | Suggested helper |
|------|------|-------|---------|------------------|
| trap fn declaration | primop.rs | 2099, 2418 | 100% | `declare_trap_fn` |
| from_value codegen | codegen.rs (bridge-derive) | 194, 357 | 92% | `generate_from_to_core_base` |
| unbox heap pointer | primop.rs | 2134, 2205 | 90% | `emit_unbox_heap_ptr` |
| pipeline fn decl setup | pipeline.rs | 271, 295 | 90% | `declare_fn_common` |
| HS path resolution | expand.rs (macro) | 46, 171 | 90% | `resolve_hs_path` |
| effect machine apply | machine.rs (effect) | 433, 510 | 87% | `apply_cont_common` |
| emit force_fn | expr.rs | 585, 2141 | 84% | `emit_force_fn` |

One pair (oracle.rs:448,475) was judged intentional — legitimately different eval paths.

## 2. Long functions (9 functions >200 lines)

| Function | File | Crate | Lines |
|----------|------|-------|-------|
| `emit_primop` | primop.rs | codegen | 1968 |
| `dispatch_primop` | eval.rs | eval | 1672 |
| `build_preamble` | lib.rs | mcp | 354 |
| `emit_letrec_phases` | expr.rs | codegen | 316 |
| `run` (JitEffectMachine) | jit_machine.rs | codegen | 295 |
| `handle_session_result_with_timeout` | lib.rs | mcp | 286 |
| `apply_cont_heap` | effect_machine.rs | codegen | 278 |
| `FromStr for PrimOpKind` | types.rs | repr | 277 |
| `emit_node_impl` | expr.rs | codegen | 260 |

The two primop dispatch functions (1968 + 1672 lines) are match-arm monsters by nature.
The rest may be decomposable.

## 3. Error handling (650 panic-family call sites)

| Category | Count |
|----------|-------|
| `panic!` / `unreachable!` | 212 |
| `.expect()` | 91 |
| `.unwrap()` | 347 |

### Panics by crate (top 5)

| Crate | Panics | Priority conversion |
|-------|--------|---------------------|
| tidepool-eval | 56 | `value.rs:320` JoinCont → Result |
| tidepool-bridge | 49 | `json.rs:766` unexpected type → Result |
| tidepool-optimize | 19 | `partial.rs:566` literal mismatch → Result |
| tidepool-mcp | 18 | — |
| tidepool-repr | 17 | — |

Note: many panics in bridge/eval are in `from_value` impls and may be intentional
invariant guards. The LLM flagged the three above as reachable in normal operation.

## 4. Name collisions (3 cross-crate)

| Type name | Crate A | Crate B |
|-----------|---------|---------|
| `BridgeError` | tidepool-bridge/error.rs | tidepool-codegen/heap_bridge.rs |
| `HeapError` | tidepool-heap/arena.rs | tidepool-codegen/debug.rs |
| `RuntimeError` | tidepool-runtime/lib.rs | tidepool-codegen/host_fns.rs |

Not bugs — different types with the same name. Confusing for cross-crate imports.

## 5. Quick wins (FIX NOW)

### Unused imports (3)

| Import | File | Crate |
|--------|------|-------|
| `Module` | primop.rs:10 | tidepool-codegen |
| `MapLayer` | normalize.rs:21 | tidepool-repr |
| `MapLayer` | subst.rs:3 | tidepool-repr |

### Repeated string literals (40 strings appear >2x)

The `runtime_*` host function names (`runtime_copy_byte_array` etc.) are
repeated 3–6x as string literals — should be `const` declarations. Error
format strings like `"range {}..{} exceeds length {}"` (6x) and
`"offset {} exceeds length {}"` (4x) should also be consts.

## 6. Track (needs design)

### Narrow traits

7 pub traits have ≤1 impl: `CollectEffectDecls`, `DescribeEffect`,
`McpEffectHandler`, `DispatchEffect`, `EffectHandler`, `ValueSource`,
`MapLayer`. All are extension points (intentional). `MapLayer` powers
the stack-safe recursion machinery (collapse/expand/hylo) — it's the
foundational trait, not a smell. The 2 unused imports of it
(normalize.rs, subst.rs) are the actual issue — stale imports, not a
trait-design problem.

### TODO/FIXME (4, all tidepool-mcp/lib.rs)

- L517: eval-prep relocation (blocked on text-vendor)
- L525: GHC `enableCodeGenForTH` latency cost
- L1958, L2632: cross-refs to same root decision re: eval dialect

## 7. Wontfix (verified intentional)

- **Leaky modules**: codegen/layout.rs (38 pub, 0 priv) — these are type
  definitions and constants consumed by other codegen modules. 100% pub is
  correct for a type-definition module.
- **Clippy suppressions**: 16 total, all justified (5× `not_unsafe_ptr_arg_deref`
  in JIT code, 5× `approx_constant` in tests using 3.14 as round-trip data).

---

## Method

All findings produced by tidepool evals using machine-native Haskell idioms:
- Line-similarity detection: O(n²) pairwise comparison within each file
- LLM triage: batch call with 8 pairs formatted as a numbered report,
  model returned one-line verdicts (extract/intentional/trivial)
- Panic census: text-level grep across full workspace
- Name collision: collect all struct/enum names, group by name, filter cross-crate

Total cost: ~6 tidepool evals + 2 Haiku LLM calls.
