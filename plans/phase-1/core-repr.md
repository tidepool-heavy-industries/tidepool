# Phase 1: core-repr

**Owner:** Claude TL (depth 1, worktree off main)
**Branch:** `main.core-repr`
**Depends on:** nothing (first phase)
**Produces:** `core-repr` crate with CoreFrame, MapLayer, supporting types, CBOR serialization, pretty printer. End-to-end: `.hs` → blob → `CoreExpr` → pretty.

---

## Wave 1: Scaffold (1 worker, gate)

### scaffold-core-repr

**Task:** Write the CoreFrame enum, MapLayer impl, and all supporting types.

**Read First:**
- `tidepool-plans/decisions.md` (§CoreFrame Variants — exact enum definition)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-repr/src/frame.rs` — `CoreFrame<A>` enum with all 11 variants (Var, Lit, App, Lam, LetNonRec, LetRec, Case, Con, Join, Jump, PrimOp). Case has a `binder: VarId` field (the case binder, bound to the evaluated scrutinee).
2. Create `core-repr/src/types.rs` — `VarId`, `JoinId`, `DataConId`, `Literal`, `PrimOpKind`, `AltCon` (DataAlt | LitAlt | Default), `Alt<A>` (con + binders + body). See decisions.md for exact definitions.
3. Implement `MapLayer<A, B>` for CoreFrame — `fn map_layer(self, f: impl FnMut(A) -> B) -> CoreFrame<B>`, covering EVERY variant
4. Create `core-repr/src/lib.rs` — `RecursiveTree<CoreFrame>` as `CoreExpr` type alias, crate re-exports

**Verify:** `cargo test -p core-repr`

**Done:** All types compile. MapLayer identity law, composition law, and construct+map_layer+reconstruct for each variant all pass.

**Boundary:**
- Use EXACT variant names and field names from decisions.md. Do not rename anything.
- MapLayer must cover all 11 variants. No `_ => todo!()` arms.
- No type variants. Types are stripped at serialization (decision D2).

**Gate:** TL reviews scaffold before wave 2. Verify enum matches decisions.md exactly.

---

## Wave 2: Rust Implementation (3-4 workers, parallel)

### frame-and-utils

**Task:** Free variable collection, substitution, and alpha-equivalence for CoreFrame.

**Read First:**
- `core-repr/src/frame.rs` (scaffold output)
- `core-repr/src/types.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-repr/src/free_vars.rs` — collapse: `CoreFrame<HashSet<VarId>>` → `HashSet<VarId>`. Account for binding sites (Lam, Let, Case binder, Join bind variables that are NOT free in their scope).
2. Create `core-repr/src/subst.rs` — substitution: capture-avoiding. Rename if body contains free vars that shadow the substituted value.
3. Create `core-repr/src/alpha.rs` — alpha-equivalence check

**Verify:** `cargo test -p core-repr -- free_vars subst alpha`

**Done:** All three functions implemented and tested.

**Tests:**
- `free_vars(Lam x body) == free_vars(body) - {x}`
- `free_vars(LetNonRec x rhs body) == free_vars(rhs) ∪ (free_vars(body) - {x})`
- Substitution is capture-avoiding (verified with shadowing case)
- Alpha-eq: `λx.x ≡α λy.y`

**Boundary:**
- Substitution MUST be capture-avoiding. This is a correctness requirement, not an optimization.

---

### types-and-datacon

**Task:** TyCon/DataCon structs and DataConTable.

**Read First:**
- `core-repr/src/types.rs` (DataConId from scaffold)
- `tidepool-plans/decisions.md` (D2 — types stripped, DataCon metadata retained)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-repr/src/datacon.rs` — DataCon struct: name, numeric tag (`dataConTag`), representation arity (`dataConRepArgTys` — post-worker/wrapper, NOT source arity), strictness per field (`dataConSrcBangs`)
2. Create `core-repr/src/datacon_table.rs` — DataConTable: lookup by name, lookup by tag, builder from metadata
3. Handle DataCons from imported modules (base's Maybe, Either, [])

**Verify:** `cargo test -p core-repr -- datacon`

**Done:** Table lookups roundtrip. Repr arity matches GHC output.

**Tests:**
- Construct DataCon, store in table, look up by tag → same DataCon
- Look up by name → same DataCon
- Repr arity for known types (Maybe has 1-arity Just, 0-arity Nothing)

**Boundary:**
- Representation arity, NOT source arity. These differ after worker/wrapper.
- DataConSrcBangs is for debugging/pretty-printing only. The evaluator does NOT use it for forcing.

---

### serial

**Task:** Rust-side CBOR reader (anamorphism: bytes → CoreExpr) and writer (catamorphism: CoreExpr → bytes).

**Read First:**
- `core-repr/src/frame.rs` (CoreFrame variants)
- `core-repr/src/datacon.rs` (DataCon metadata)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Add `ciborium` dependency to `core-repr/Cargo.toml`
2. Create `core-repr/src/serial/read.rs` — CBOR bytes → `RecursiveTree<CoreFrame>`. Handle all PureExpr variants from the Haskell serializer, DataCon metadata table, PrimOp identifiers, JoinId arities.
3. Create `core-repr/src/serial/write.rs` — `RecursiveTree<CoreFrame>` → CBOR bytes (for test fixture generation)
4. Create `core-repr/src/serial/mod.rs` — re-exports

**Verify:** `cargo test -p core-repr -- serial`

**Done:** Roundtrip identity (write → read → compare). No Cast/Tick/Type nodes in deserialized output.

**Tests:**
- Roundtrip: `write(expr) |> read == expr` for all variant combinations
- No Cast, Tick, or Type nodes survive deserialization (they're erased by Haskell serializer)
- DataCon metadata table deserializes correctly
- PrimOp identifiers preserved

**Boundary:**
- Only `ciborium` for CBOR. No other serialization crate.

---

### pretty

**Task:** Pretty printer: collapse `CoreFrame<String>` → `String`.

**Read First:**
- `core-repr/src/frame.rs` (CoreFrame variants)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-repr/src/pretty.rs` — implement pretty-print for all CoreFrame variants
2. Parenthesization: nested App (left-associative), Lam (right-associative)
3. LetRec groups: display mutual bindings together
4. PrimOp, Join/Jump: readable format matching GHC `-ddump-simpl` style

**Verify:** `cargo test -p core-repr -- pretty`

**Done:** All variants rendered. Output is readable. No "TODO" placeholder strings.

**Tests:**
- All 11 CoreFrame variants produce output (coverage check)
- Parenthesization: `App(App(f, x), y)` → `f x y` (not `(f x) y`)
- LetRec: mutual bindings visible
- Corresponds to GHC `-ddump-simpl` on known modules

**Boundary:**
- All variants must be covered. No `_ => "???"` arms.

---

**After wave 2:** TL runs `cargo test -p core-repr`. Commit.

---

## Wave 3: Haskell Subtree

**`spawn_subtree`** → Claude TL at `main.core-repr.haskell-harness`. This subtree's Claude dispatches Gemini workers for the Haskell code, providing judgment around GHC API quirks.

### ghc-api-harness (worker in subtree)

**Task:** Haskell executable: GHC API session setup with explicit pipeline orchestration.

**Read First:**
- `tidepool-plans/decisions.md` (§GHC Pipeline)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create Haskell executable with GHC API imports
2. Pipeline: `parseModule` → `typecheckModule` → `hscDesugar` → `core2core`
3. MUST use explicit pipeline, NOT `load LoadAllTargets` (ModGuts is discarded after implicit load)
4. Capture ModGuts after core2core (post-simplifier, pre-tidy)
5. DynFlags: `backend=noBackend`, `ghcLink=NoLink`, `updOptLevel 2`
6. Package DB: inherit `GHC_PACKAGE_PATH` from nix environment
7. Expose: `mg_binds` (CoreProgram) and `mg_tcs` ([TyCon])

**Verify:** Build and run on 5 known `.hs` modules.

**Done:** Prints CoreBind count and TyCon names for each module. Verifies `-O2` simplification ran.

**Boundary:**
- MUST use explicit pipeline orchestration. `LoadAllTargets` discards ModGuts.
- Read GHC source for `hscDesugar` and `core2core` before coding — GHC API docs have gaps.

---

### core-serializer (worker in subtree)

**Task:** Haskell module: CoreExpr → PureExpr → CBOR encoding.

**Read First:**
- `tidepool-plans/decisions.md` (D2 — type erasure rules, D4 — PrimOp handling)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create translation: CoreExpr → PureExpr (simplified intermediate type)
2. Erasure at translation time: Cast → strip, Tick → strip, App (Type _) → strip, Type/Coercion → omit
3. Join point detection: check `isJoinId_maybe` on Let *binders* (not variable occurrences). If the binder is a join Id, serialize the binding as `Join` and its call sites as `Jump`. Otherwise serialize as `LetNonRec`/`LetRec`.
4. Constructor saturation: recognize saturated applications of DataCon worker Ids (nested `App` chains where the function is a `DataConWorkId`), collapse into `Con { tag, fields }`. Unsaturated constructor applications (fewer args than representation arity) serialize as `Var` — the Rust side treats them as closures.
5. PrimOp saturation: recognize saturated applications of `GHC.Prim` Ids, collapse into `PrimOp { op, args }`. Unsaturated primops after `-O2` should not occur; error if encountered.
6. Extract DataCon metadata: `dataConTag`, `dataConRepArgTys`, `dataConSrcBangs`
7. CBOR-encode via `serialise` package
8. Takes ModGuts as input from ghc-api-harness

**Verify:** Serialize known CoreBind structures, verify CBOR roundtrips, verify no Cast/Tick/Type nodes.

**Done:** CBOR output is parseable by Rust serial crate. No erased nodes survive.

**Boundary:**
- Erasure happens HERE (Haskell side), not in Rust. This is by design.
- ghc-dump is ruled out: version lag, payload bloat, no filtering.

---

### Subtree integration (Claude TL direct)

TL wires harness + serializer into single executable. Pipes ModGuts into serializer, produces `.blob` files. Verifies CBOR deserializes into CoreExpr via serial crate. ~30 lines of glue. Files PR against `main.core-repr`.

---

## Wave 4: End-to-End (TL direct)

TL merges Haskell subtree PR, wires serial output into pretty printer. `.hs` → blob → `CoreExpr` → pretty. `cargo test`. This is glue, not a worker task.

File PR against `main`.

---

## Domain Anti-Patterns

In addition to the base anti-patterns file:

- No type variants in CoreFrame. Types are stripped at serialization (D2).
- Representation arity, not source arity. These differ after worker/wrapper.
- `dataConRepArgTys` returns `[Scaled Type]` in GHC 9.0+, NOT `[Type]`. Use `map scaledThing (dataConRepArgTys dc)` to extract the types. (See research/03.)
- DataConSrcBangs is for debugging only. Evaluator does NOT use it for forcing.
- CBOR erasure happens in Haskell, not Rust.
