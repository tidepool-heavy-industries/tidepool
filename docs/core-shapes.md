# Core Shapes — Normative Dossier

This dossier provides a consolidated, normative reference for the GHC Core constructs and heap shapes supported by the Tidepool JIT. It synthesizes findings from four parallel audits covering translation, codegen, and runtime bridge layers. This document serves as a technical foundation for addressing "cross-module divergence" patterns where bindings merged across modules break assumptions made during single-module optimization. For historical context, see PR #272 and Issue #273.

## How to read this dossier
Each section describes a Core construct or shape concern, defining what the JIT must accept and what it produces as a canonical form. Entries cite source audits using cross-links such as [audit-translate § emitRuntimeUnpackCString](core-shapes/audit-translate.md#emitruntimeunpackcstring). The [Coverage Gaps](#coverage-gaps) section at the end provides a prioritized punch list of uncovered branches, silent fallbacks, and cross-module collision risks that require hardening.

## Table of contents
- [1. Literals (`Lit`)](#1-literals-lit)
- [2. Constructors (`Con`)](#2-constructors-con)
- [3. Cases (`Case`) and pattern matching](#3-cases-case-and-pattern-matching)
- [4. Lambdas, Applications, and Tail Calls](#4-lambdas-applications-and-tail-calls)
- [5. Let / LetRec](#5-let--letrec)
- [6. Join points](#6-join-points)
- [7. Effects (freer-simple Val/E/Union/Leaf/Node)](#7-effects-freer-simple-valeunionleafnode)
- [8. Bridge: Rust ↔ Core value conversion](#8-bridge-rust--core-value-conversion)
- [9. Heap-layout invariants](#9-heap-layout-invariants)
- [10. Cross-module compilation: known divergence points](#10-cross-module-compilation-known-divergence-points)
- [11. Cancellation safepoints](#11-cancellation-safepoints)
- [12. Normalization pipeline](#12-normalization-pipeline)
- [13. Cross-mode test infrastructure](#13-cross-mode-test-infrastructure)
- [Coverage Gaps](#coverage-gaps)
- [Cross-references and source audits](#cross-references-and-source-audits)

## 1. Literals (`Lit`)

The JIT must accept literal values arriving as `LEInt`, `LEWord`, `LEChar`, `LEFloat`, `LEDouble`, and `LEString`. During translation, certain GHC-specific literals (like `Addr#` and `RealWorld#`) are erased or canonicalized. Static string literals are desugared from `Addr#` pointers into uniform cons-lists of characters to maintain compatibility with list-processing functions.

Translation produces `NLit` nodes containing the primitive value. The `TAG_LIT` heap layout, including the specific `LitTag` identifying the primitive type, is only materialized during codegen or allocation.

**Special cases**:
- `unpackCString# "lit"#` desugar — converts static `Addr#` to `(:)` chain. Mode: always-on. [audit-translate § isUnpackCStringVar static desugar](core-shapes/audit-translate.md#isunpackcstringvar-static-desugar).
- `unpackFoldrCString#` desugar — handles fusion-based string literals. Mode: always-on. [audit-translate § isUnpackFoldrCStringVar static desugar](core-shapes/audit-translate.md#isunpackfoldrcstringvar-static-desugar).
- `realWorld#` erasure — state tokens are replaced with dummy `LEInt 0`. Mode: always-on. [audit-translate § isRealWorldVar](core-shapes/audit-translate.md#isrealworldvar).
- `Typeable` metadata erasure — metadata variables are replaced with error nodes. Mode: always-on. [audit-translate § isTypeMetadataVar](core-shapes/audit-translate.md#istypemetadatavar).

**Known fragility**:
- `Addr#` fallback: a top-level `Addr#` result (legitimately produced by `PlusAddr` / `ShowDoubleAddr` primops in `emit/primop.rs`) returns an empty `LitString` because the bridge cannot decode a raw pointer back to a typed Haskell value. Intentional behavior, not a translator-bug guard — programs that compose `Addr#` with `unpackCString#` etc. evaluate to a real string before reaching the bridge. [audit-heap-bridge § LitTag::Addr fallback](core-shapes/audit-heap-bridge.md#littagaddr-fallback).
- Character literals: the bridge performs a silent fallback to `\0` if the heap contains invalid Unicode data. [audit-heap-bridge § LitTag::Char](core-shapes/audit-heap-bridge.md#littagchar).

## 2. Constructors (`Con`)

Constructors are the primary means of heap allocation. The JIT must handle both standard `DataCon` applications and unboxed tuples. Arity must be adjusted for GADTs where equality evidence (`EqSpec`) is present in the Core but erased in the JIT.

Translation produces `NCon` nodes. All constructors are materialized as `TAG_CON` objects on the heap during codegen or dynamic allocation. The runtime heap layout, where `con_tag` (DataConId) and field pointers are stored at fixed offsets, only applies once the constructor has been materialized.

**Special cases**:
- Unboxed tuples — translated to standard `NCase` dispatch or single-element fallthrough. Mode: always-on. [audit-translate § isUnboxedTupleDataCon (general)](core-shapes/audit-translate.md#isunboxedtupledatacon-general).
- `unsafeEqualityProof` — represented as a unit constructor `()`. Mode: always-on. [audit-translate § isUnsafeEqualityProofVar desugar](core-shapes/audit-translate.md#isunsafeequalityproofvar-desugar).
- Arity adjustment — `valueRepArity` corrects for erased `Coercion` arguments. Mode: always-on. [audit-translate § valueRepArity](core-shapes/audit-translate.md#valuereparity).
- Composed `Node`/`E` — the effect machine dynamically allocates these during continuation composition. Mode: always-on. [audit-effect-machine § alloc_con](core-shapes/audit-effect-machine.md#alloc_con).

**Known fragility**:
- Raw Con accessors: `read_con_tag` and friends do not perform tag-checks; passing a `TAG_LIT` or `TAG_THUNK` results in UB. [audit-effect-machine § Basic Con Accessors](core-shapes/audit-effect-machine.md#basic-con-accessors).
- Boxing constructor collisions: the bridge uses unqualified `get_by_name` for boxing types like `I#`, which risks collisions in multi-module builds. [audit-bridge § is_boxing_con helper](core-shapes/audit-bridge.md#is_boxing_con-helper).

## 3. Cases (`Case`) and pattern matching

Case expressions drive control flow and deconstruction. The JIT must accept scrutinees that are evaluated at runtime, as well as complex desugaring for `tagToEnum#` and multi-return primops.

Downstream, `Case` is lowered to Cranelift branch instructions or direct jumps based on the tag of the evaluated scrutinee.

**Special cases**:
- `tagToEnum#` — desugared into an explicit `Case` on the tag index. Mode: always-on. [audit-translate § tagToEnum# desugar](core-shapes/audit-translate.md#tagtoenum-desugar).
- `unsafeEqualityProof` elision — cases matching `UnsafeRefl` are elided to prevent tag mismatches with unit-represented evidence. Mode: always-on. [audit-translate § isUnsafeEqualityCase elision](core-shapes/audit-translate.md#isunsafeequalitycase-elision).
- Multi-return primops — `quotRemInt#` and others are desugared into nested cases of single-return primops. Mode: always-on. [audit-translate § splitMultiReturnPrimOp desugar](core-shapes/audit-translate.md#splitmultireturnprimop-desugar).
- Stateful primop erasure — `State#` tokens in case results are stripped. Mode: always-on. [audit-translate § Stateful primop state erasure](core-shapes/audit-translate.md#stateful-primop-state-erasure).

**Known fragility**:
- The `isUnsafeEqualityCase` elision was a critical fix for cross-module divergence where inlined evidence survived the optimizer. [audit-translate § isUnsafeEqualityCase elision](core-shapes/audit-translate.md#isunsafeequalitycase-elision).

## 4. Lambdas, Applications, and Tail Calls

The JIT must handle standard functional applications and eta-reduced variants of intercepted functions. Specialized `Show` instances for `Double` are intercepted to bypass GHC's complex `Integer` pipeline.

After translation, applications become `NApp` or tail-call jumps. Tail calls are resolved iteratively in a loop to avoid stack exhaustion.

**Special cases**:
- `Show Double` interception — specialized $fShowDouble handlers are replaced with JIT-native implementations. Mode: always-on. [audit-translate § emitShowDoubleSpecBody](core-shapes/audit-translate.md#emitshowdoublespecbody).
- `runRW#` erasure — state-token applications are simplified to unit applications. Mode: always-on. [audit-translate § isRunRWVar desugar](core-shapes/audit-translate.md#isrunrwvar-desugar).
- Tail-call resolution — the effect machine resolves `tail_callee` and `tail_arg` in a loop. Mode: always-on. [audit-effect-machine § resolve_tail_calls: Code pointer read](core-shapes/audit-effect-machine.md#resolve_tail_calls-code-pointer-read).

**Known fragility**:
- Closure code pointers: `call_closure` jumps directly to the pointer read from the heap; a tag mismatch here is fatal. [audit-effect-machine § call_closure: Code pointer read](core-shapes/audit-effect-machine.md#call_closure-code-pointer-read).

## 5. Let / LetRec

The JIT must accept `Let` and `LetRec` bindings, performing reachability analysis to prune unused bindings from large recursive groups. Standard library functions that lack unfoldings (like `(++)` and `take`) are desugared into JIT-native `LetRec` loops.

**Special cases**:
- `(++)` desugar — provided as a reliable runtime implementation for list concatenation. Mode: always-on. [audit-translate § isAppendVar desugar](core-shapes/audit-translate.md#isappendvar-desugar).
- `unsafeTake` desugar — implements `take` using unboxed counters and primops. Mode: always-on. [audit-translate § isUnsafeTakeVar desugar](core-shapes/audit-translate.md#isunsafetakevar-desugar).
- Reachable binds — prunes the transitive closure to keep JIT modules small. Mode: always-on. [audit-translate § reachableBinds](core-shapes/audit-translate.md#reachablebinds).

**Known fragility**:
- Large `Rec` groups: merging modules can create massive recursive groups that challenge the resolver if reachability analysis fails to fragment them. [audit-translate § reachableBinds](core-shapes/audit-translate.md#reachablebinds).

## 6. Join points

Join points are optimized local jumps. The JIT must promote join points to full closures if they are captured by a lambda, as Cranelift blocks cannot be targeted from separate function scopes.

**Special cases**:
- `jumpCrossesLam` — detects and promotes invalid join point jumps. Mode: always-on. [audit-translate § jumpCrossesLam (Join Point conversion)](core-shapes/audit-translate.md#jumpcrosseslam-join-point-conversion).

## 7. Effects (freer-simple Val/E/Union/Leaf/Node)

The JIT is specialized for `freer-simple` effect stacks. It must handle the extraction of `Val` and `E` (request) shapes from JIT-compiled results and the composition of continuations in the work-stack.

**Special cases**:
- Union tag boxing — handles both unboxed `Word#` and boxed `W#` effect tags to support cross-module divergence in the GHC optimizer. Mode: always-on. [audit-effect-machine § parse_result: Union position-tag boxing branch](core-shapes/audit-effect-machine.md#parse_result-union-position-tag-boxing-branch).
- Continuation tree-walking — iteratively applies `Leaf` and `Node` continuations. Mode: always-on. [audit-effect-machine § apply_cont_heap: Leaf/Node con_tag dispatch](core-shapes/audit-effect-machine.md#apply_cont_heap-leafnode-con_tag-dispatch).

**Known fragility**:
- `UnexpectedTag` in `parse_result`: if a JIT function returns an unexpected tag for an `Eff` type, the machine yields a hard error. [audit-effect-machine § parse_result: Result tag check](core-shapes/audit-effect-machine.md#parse_result-result-tag-check).

## 8. Bridge: Rust ↔ Core value conversion

The bridge layer converts between raw heap objects and Rust's `Value` enum. It must handle recursion depth limits and null guards. The `ToCore` and `FromCore` traits provide higher-level mapping for standard types like `Result`, `Option`, and `String`.

**Special cases**:
- Numeric/Char unwrapping — `i64`, `u64`, and `char` impls transparently unwrap `I#`, `W#`, and `C#` boxes. Mode: always-on. [audit-bridge § i64 (Int)](core-shapes/audit-bridge.md#i64-int).
- `Text` vs `[Char]` — the `String` bridge handles both unboxed `Text` workers and standard character lists. Mode: always-on. [audit-bridge § String (Text)](core-shapes/audit-bridge.md#string-text).
- JSON mapping — `serde_json::Value` uses `get_by_name_arity` to safely map types without collision. Mode: always-on. [audit-bridge § serde_json::Value](core-shapes/audit-bridge.md#serde_jsonvalue).

**Known fragility**:
- Unboxing constructors: the bridge relies on standard names like `I#`. If a user module defines a colliding `I#` with different arity, decoding will fail. [audit-bridge § is_boxing_con helper](core-shapes/audit-bridge.md#is_boxing_con-helper).

## 9. Heap-layout invariants

The JIT and runtime share a set of heap layout constants defined in `tidepool-heap`. Every `HeapObject` must begin with a 1-byte tag at offset 0.

**Core Invariants**:
- `TAG_CON`: `con_tag` at 8, `num_fields` at 16, pointers start at 24.
- `TAG_LIT`: `lit_tag` at 8, value starts at 16.
- `TAG_THUNK`: `state` at 8, indirection/code at 16.
- `TAG_CLOSURE`: code pointer at 8, `num_captured` at 16, fields at 24.

**Special cases**:
- Thunk indirection — evaluated thunks point to their result via an indirection pointer at offset 16. Mode: always-on. [audit-heap-bridge § TAG_THUNK evaluated (Indirection)](core-shapes/audit-heap-bridge.md#tag_thunk-evaluated-indirection).
- ByteArrays — data lives outside the GC nursery to prevent use-after-copy bugs. Mode: always-on. [audit-heap-bridge § LitTag::ByteArray](core-shapes/audit-heap-bridge.md#littagbytearray).

## 10. Cross-module compilation: known divergence points

Merging optimized modules into a single JIT module reveals "divergence" where GHC's module-local assumptions break.

**Key strategies**:
- Hash-based ID disambiguation — `localVarId` hashes `OccName` + `Unique` to prevent shadow-binder collisions across modules. Mode: always-on. [audit-translate § localVarId hash disambiguation](core-shapes/audit-translate.md#localvarid-hash-disambiguation).
- Stable IDs — `stableVarId` ensures external references have uniform IDs. Mode: always-on. [audit-translate § stableVarId](core-shapes/audit-translate.md#stablevarid).
- Tag boxing flexibility — codegen accepts both boxed and unboxed effect tags. [audit-effect-machine § parse_result: Union position-tag boxing branch](core-shapes/audit-effect-machine.md#parse_result-union-position-tag-boxing-branch).

**Known fragility**:
- `String` (Text) bridge: high risk of collision due to multiple unqualified lookups for `Text`, `ByteArray`, and `[]`. [audit-bridge § String (Text)](core-shapes/audit-bridge.md#string-text).

## 11. Cancellation safepoints

The JIT includes safepoints where long-running or infinite computations can be interrupted.

**Special cases**:
- Tail-call safepoint — the effect machine checks for external cancellation before resolving each tail call. Mode: always-on. [audit-effect-machine § resolve_tail_calls: Cancellation safepoint](core-shapes/audit-effect-machine.md#resolve_tail_calls-cancellation-safepoint).
- Effect-dispatch safepoint — `JitEffectMachine::run` checks the cancel flag at the top of every iteration of the effect-dispatch loop, after `handlers.dispatch` returns and before re-entering JIT via `resume`. Gives prompt unwind for handler-driven cancel (the watchdog-handler scenario). Mode: always-on. (PR #274)
- GC heap-check safepoint — `gc_trigger` records `RuntimeError::Cancelled` but does not unwind; observed via downstream paths. Best-effort. Mode: always-on.
- Pure non-tail-call allocator-only loops are not promptly interruptible — tracked in [#273](https://github.com/tidepool-heavy-industries/tidepool/issues/273).

## 12. Normalization pipeline

`tidepool-repr::normalize(expr, table)` runs `CoreExpr → CoreExpr` between translation and codegen, canonicalizing shape divergence that arises from cross-module compilation. Three rules, fixpoint-bounded at 100 iterations, idempotent and semantics-preserving.

**Pipeline placement**: `tidepool_repr::normalize → wrap_with_datacon_env → emit::compile_expr` (in `JitEffectMachine::compile`).

**Rules**:
- **Rule 1 (flatten_box)**: `Con(BOX, [Con(BOX, [inner])])` → `Con(BOX, [inner])` where `BOX ∈ {I#, W#, C#, F#, D#}`. [audit-normalize § Rule 1](core-shapes/audit-normalize.md#rule-1-flatten_box).
- **Rule 2 (canonicalize_effect_tag)**: `Con(Union, [W#(x_or_var), payload])` → `Con(Union, [Lit(LitWord, n), payload])` when `x_or_var` resolves to a `LitWord`. Pairs with the runtime fallback in [audit-effect-machine § Boxed Union tag](core-shapes/audit-effect-machine.md#boxed-union-tag-runtime-fallback). [audit-normalize § Rule 2](core-shapes/audit-normalize.md#rule-2-canonicalize_effect_tag).
- **Rule 3 (unbox_prim_args)**: `PrimOp { args: [Con(BOX, [Lit])..] }` → `PrimOp { args: [Lit..] }` (all-or-nothing). [audit-normalize § Rule 3](core-shapes/audit-normalize.md#rule-3-unbox_prim_args).

**Properties** (all proptest-verified):
- Idempotent (`prop_idempotence` in `normalize.rs`).
- Semantics-preserving (`prop_normalize_preserves_semantics` in `tidepool-testing/tests/normalize_semantics.rs`, PR #294).
- Bounded fixpoint (`prop_bounded_iteration`).

**Known limitation**: Rule 2 effectiveness on real cross-mode programs is unverified — see [Coverage Gaps](#coverage-gaps).

## 13. Cross-mode test infrastructure

`tidepool-runtime/tests/cross_mode_harness/` provides a structural-equivalence harness for asserting that single-module and split-module compilations of the same Haskell source produce semantically equivalent JIT outputs. The harness is the regression specification for the cross-module divergence patterns documented above.

**Harness API**:
- `CrossModeFixture { single, split, target }` — describes a Haskell program in two equivalent shapes.
- `assert_cross_mode_structurally_equivalent(fixture)` — alpha-equivalent CoreExpr comparison.
- `assert_cross_mode_pure_equivalent(fixture)` — runtime value comparison for pure programs.
- `assert_cross_mode_runtime_equivalent(fixture, ...)` — runtime value comparison with effect handlers.

**Coverage**: 21 fixtures across 5 divergence dimensions (GADT effect dispatch, name collisions, primitive boxing, mutual recursion, typeclass dispatch). See `tidepool-runtime/tests/cross_mode_existing.rs` (breadth) and `cross_mode_targeted.rs` (depth).

**Structural comparator**: alpha-equivalence via `var_map` / `join_map` HashMaps; tiered DataCon matching (qualified name → name+arity); opaque types (`Closure`, `ThunkRef`, `JoinCont`) skipped during comparison. See [audit-heap-bridge § Cross-mode harness](core-shapes/audit-heap-bridge.md#cross-mode-harness-structural-equivalence).

**Notes**: The harness is asymmetric (walks single → split). Tree-size mismatch is panic'd before per-node walk begins. Provides diagnostic naming the divergent node index, variant kinds, and DataCon names on failure.

## Coverage Gaps

### Uncovered branches (no regression test)
- `audit-translate § emitRuntimeUnpackCString` — fallback for dynamic Addr# strings.
- `audit-translate § emitRuntimeUnpackAppendCString` — dynamic string concatenation.
- `audit-translate § isUnpackCStringVar dynamic fallback` — handles non-static `Addr#` variables.
- `audit-translate § Coercion placeholder` — dummy literal for rare expression-position coercions.
- `audit-effect-machine § apply_cont_heap: Closure tag check` — raw closures as continuations.
- `audit-effect-machine § call_closure: Captured fields read` — debug/trace-only field inspection.
- `audit-heap-bridge § TAG_THUNK evaluated (Indirection)` — decoding already-forced thunks.
- `audit-heap-bridge § TAG_THUNK forcing` — triggering side-effects during bridge traversal.

### Silent fallbacks (return wrong-but-valid Value on shape mismatch)
- `audit-heap-bridge § LitTag::Char` — returns `\0` on invalid Unicode data. Intentional behavior per dossier §1.

### Hardened (recoverable error)
- `audit-effect-machine § force_ptr: Thunk tag check` — returns poison pointer and sets `YieldError::UserErrorMsg` on unexpected tag (Hardened).
- `audit-effect-machine § apply_cont_heap` — now surfaces `YieldError::UserErrorMsg` via `runtime_error_with_msg` on shape mismatch instead of returning `null_mut` silently (Hardened).
- `audit-heap-bridge § TAG_CLOSURE opaque representation` — now returns `BridgeError::UnexpectedHeapTag(TAG_CLOSURE)` instead of a dummy opaque `Closure` (Hardened).

### Silent fallbacks (still wrong-but-valid Value on shape mismatch)
- `audit-heap-bridge § SmallArray# / Array# coercion` — silently coerces to `Value::Con(DataConId(0), [..])`; type info erased. Hardening candidate; tracked under code-hardening-wave2.

### Cross-module collision risk: Medium (post-#293 hardening)
- `audit-bridge § String (Text)` — was `high`; reduced to `medium` after `get_resilient` migration. Remaining mitigation: migrate to `get_by_qualified_name` for `Text` constructor.
- `audit-bridge § is_boxing_con` — was `medium`; reduced to `low` after `get_resilient` adopted arity-aware lookup.
- `audit-bridge § Result<T, E> (Either)` — uses fallback names (Right/Ok) which may collide with user types.

### Defensive code with no observed triggering source
- `audit-translate § Coercion placeholder` — provides safety if coercions survive inlining into expression position.
- `audit-translate § isRealWorldVar` — part of the state-token erasure pipeline; rarely appears as a standalone variable.
- `audit-translate § Unresolved external Id → kind-4 error node` — runtime poison for missing unfoldings.
- `audit-translate § FFI primop support` — defensive against GHC wrapping primops with FFI bookkeeping.

### Verification gap
- Rule 2 (`canonicalize_effect_tag`) effectiveness on real cross-mode programs is unverified — the runtime fallback in `effect_machine.rs:~245` may be doing all the work. Adding a debug-only counter would surface this. See `audit-normalize.md` Coverage gaps.

## Cross-references and source audits

- **[audit-translate.md](core-shapes/audit-translate.md)**: 36 entries covering `Translate.hs` special-case branches and desugaring (30 original + 6 added from PR #295's source-coverage audit).
- **[audit-effect-machine.md](core-shapes/audit-effect-machine.md)**: 26 entries covering `effect_machine.rs` heap interpretation, composition, and post-#289 hardening (19 original + 7 added).
- **[audit-heap-bridge.md](core-shapes/audit-heap-bridge.md)**: 22 entries covering low-level heap-to-value decoding and the cross-mode test harness (17 original + 5 added).
- **[audit-bridge.md](core-shapes/audit-bridge.md)**: 17 entries covering high-level primitive implementations in `tidepool-bridge` (16 original + 1 `get_resilient` helper added; line citations refreshed).
- **[audit-normalize.md](core-shapes/audit-normalize.md)**: NEW — covers `tidepool-repr/src/normalize.rs` (3 rules, fuel limit, fixpoint bound, and proptest properties).

For historical context, see [PR #272](https://github.com/tidepool-heavy-industries/tidepool/pull/272) (cross-module fixes), [PR #285](https://github.com/tidepool-heavy-industries/tidepool/pull/285) (cross-mode test harness), [PR #289](https://github.com/tidepool-heavy-industries/tidepool/pull/289) (normalization pass), [PR #291](https://github.com/tidepool-heavy-industries/tidepool/pull/291) (initial dossier), [PR #293](https://github.com/tidepool-heavy-industries/tidepool/pull/293) (over-strict assert softening), [PR #294](https://github.com/tidepool-heavy-industries/tidepool/pull/294) (semantics proptest), [PR #295](https://github.com/tidepool-heavy-industries/tidepool/pull/295) (silent-fallback hardening), and [Issue #273](https://github.com/tidepool-heavy-industries/tidepool/issues/273) (deferred prompt-cancel for pure allocator loops), [Issue #296](https://github.com/tidepool-heavy-industries/tidepool/issues/296) (spliton_repro test rewrite).
