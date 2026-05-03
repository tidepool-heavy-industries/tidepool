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
- [Coverage Gaps](#coverage-gaps)
- [Cross-references and source audits](#cross-references-and-source-audits)

## 1. Literals (`Lit`)

The JIT must accept literal values arriving as `LEInt`, `LEWord`, `LEChar`, `LEFloat`, `LEDouble`, and `LEString`. During translation, certain GHC-specific literals (like `Addr#` and `RealWorld#`) are erased or canonicalized. Static string literals are desugared from `Addr#` pointers into uniform cons-lists of characters to maintain compatibility with list-processing functions.

After translation, literals are represented as `TAG_LIT` objects in the heap, with a specific `LitTag` identifying the primitive type.

**Special cases**:
- `unpackCString# "lit"#` desugar — converts static `Addr#` to `(:)` chain. Mode: always-on. [audit-translate § isUnpackCStringVar static desugar](core-shapes/audit-translate.md#isunpackcstringvar-static-desugar).
- `unpackFoldrCString#` desugar — handles fusion-based string literals. Mode: always-on. [audit-translate § isUnpackFoldrCStringVar static desugar](core-shapes/audit-translate.md#isunpackfoldrcstringvar-static-desugar).
- `realWorld#` erasure — state tokens are replaced with dummy `LEInt 0`. Mode: always-on. [audit-translate § isRealWorldVar](core-shapes/audit-translate.md#isrealworldvar).
- `Typeable` metadata erasure — metadata variables are replaced with error nodes. Mode: always-on. [audit-translate § isTypeMetadataVar](core-shapes/audit-translate.md#istypemetadatavar).

**Known fragility**:
- `Addr#` fallback: non-static `Addr#` values return an empty string in the bridge rather than risking raw pointer access. [audit-heap-bridge § LitTag::Addr fallback](core-shapes/audit-heap-bridge.md#littagaddr-fallback).
- Character literals: the bridge performs a silent fallback to `\0` if the heap contains invalid Unicode data. [audit-heap-bridge § LitTag::Char](core-shapes/audit-heap-bridge.md#littagchar).

## 2. Constructors (`Con`)

Constructors are the primary means of heap allocation. The JIT must handle both standard `DataCon` applications and unboxed tuples. Arity must be adjusted for GADTs where equality evidence (`EqSpec`) is present in the Core but erased in the JIT.

After translation, all constructors are represented as `TAG_CON` objects. The `con_tag` (DataConId) and field pointers are stored at fixed offsets.

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
- `audit-effect-machine § force_ptr: Thunk tag check` — returns current pointer if not a thunk, potentially hiding logic errors.
- `audit-effect-machine § apply_cont_heap` — multiple branches return `null_mut` on mismatch, causing downstream machine failure instead of immediate trap.
- `audit-heap-bridge § LitTag::Char` — returns `\0` on invalid Unicode data.
- `audit-heap-bridge § LitTag::Addr fallback` — returns empty `LitString` for raw machine addresses.
- `audit-heap-bridge § TAG_CLOSURE opaque representation` — produces a dummy opaque `Closure` instead of failing.

### Cross-module collision risk: high
- `audit-bridge § String (Text)` — uses unqualified `get_by_name` for `Text`, `ByteArray`, and `[]`; highly susceptible to arity or name collisions.
- `audit-bridge § is_boxing_con` — unqualified lookup for `I#`, `W#`, etc.
- `audit-bridge § Result<T, E> (Either)` — uses fallback names (Right/Ok) which may collide with user types.

### Defensive code with no observed triggering source
- `audit-translate § Coercion placeholder` — provides safety if coercions survive inlining into expression position.
- `audit-translate § isRealWorldVar` — part of the state-token erasure pipeline; rarely appears as a standalone variable.

## Cross-references and source audits

- **[audit-translate.md](core-shapes/audit-translate.md)**: 30 entries covering `Translate.hs` special-case branches and desugaring.
- **[audit-effect-machine.md](core-shapes/audit-effect-machine.md)**: 19 entries covering `effect_machine.rs` heap interpretation and composition.
- **[audit-heap-bridge.md](core-shapes/audit-heap-bridge.md)**: 17 entries covering low-level heap-to-value decoding in `heap_bridge.rs`.
- **[audit-bridge.md](core-shapes/audit-bridge.md)**: 16 entries covering high-level primitive implementations in `tidepool-bridge`.

For more information, see [PR #272](https://github.com/exo-monad/tidepool/pull/272) (cross-module fixes) and [Issue #273](https://github.com/exo-monad/tidepool/issues/273) (deferred hardening).
