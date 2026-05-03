# Core normalization audit

## 1. Shapes consumers peel inline

- **tidepool-codegen/src/emit/primop.rs:1863 (`unbox_addr`)**: Recursively peels `TAG_CON` to find a `TAG_LIT` (specifically `LIT_TAG_STRING` or `LIT_TAG_BYTEARRAY`). It loads the first field (`CON_FIELDS_OFFSET`) and loops until it hits a non-Con object.
  - *Current logic*: Recursive block-loop in Cranelift IR.
  - *Canonical form*: IR-level `stripBoxCon` normalization to ensure primops receive direct `Lit` nodes when possible.

- **tidepool-codegen/src/emit/primop.rs:1935 (`unbox_bytearray`)**: Identical recursive peeling logic for ByteArray unboxing.
  - *Current logic*: Recursive block-loop in Cranelift IR.
  - *Canonical form*: IR-level `stripBoxCon` normalization.

- **tidepool-codegen/src/emit/primop.rs:2005 (`unbox_numeric`)**: Recursive peeling for `Int#`, `Double#`, `Float#`. Also handles thunk forcing (`TAG_THUNK`) which must remain runtime logic.
  - *Current logic*: Recursive block-loop in Cranelift IR with a call to `heap_force`.
  - *Canonical form*: IR-level normalization can eliminate the `TAG_CON` peeling loop, but `TAG_THUNK` check must stay.

- **tidepool-codegen/src/effect_machine.rs:241-255**: Ad-hoc peeling of effect tags. Handles both direct `Lit(Word, n)` and boxed `Con(W#, [Lit(Word, n)])`.
  - *Current logic*: `if tag_ptr_tag == layout::TAG_LIT { ... } else if tag_ptr_tag == layout::TAG_CON { ... }`.
  - *Canonical form*: Normalize all effect tags to unboxed `LitWord` at the IR level.

## 2. Shapes Translate.hs eliminates pre-IR

- **haskell/src/Tidepool/Translate.hs:1038-1045 (`isUnsafeEqualityCase`)**: Elides `case unsafeEqualityProof of UnsafeRefl -> body`.
  - *Shape*: `Case (Var unsafeEqualityProof) _ _ [Alt (DataAlt UnsafeRefl) _ body]`.
  - *Why it stays*: Requires GHC-side Name/Id knowledge to identify `unsafeEqualityProof` and `UnsafeRefl`.

- **haskell/src/Tidepool/Translate.hs:524-541**: Desugars `unpackCString# "addr"#` into a static chain of `Con` (cons-cells).
  - *Shape*: `App (Var unpackCString#) (Lit (LitString bs))`.
  - *Why it stays*: Core representation of strings as `Addr#` is a GHC-specific optimization that we want to unify early.

- **haskell/src/Tidepool/Translate.hs:760-775**: Desugars `runRW# f` to `f ()`.
  - *Shape*: `App (Var runRW#) f`.
  - *Why it stays*: Erases the state token which has no runtime representation in Tidepool.

- **haskell/src/Tidepool/Translate.hs:786-801**: Desugars `tagToEnum#` into a `Case` expression matching on an `Int#` literal.
  - *Shape*: `App (Var tagToEnum#) arg`.
  - *Why it stays*: Requires access to the `TyCon` and its `DataCons` to generate the case alternatives, which is erased in the IR.

- **haskell/src/Tidepool/Translate.hs:492-520**: Intercepts `showDouble` and specializes it using `ShowDoubleAddr` primop.
  - *Shape*: `App (Var showDouble) d`.
  - *Why it stays*: Avoids pulling in complex floating-point formatting libraries from GHC's `base`.

## 3. Shapes that survive translation; JIT handles ad-hoc

- **Recursive boxing (`I# (I# n#)`)**: Survivors of cross-module inlining where the optimizer didn't see the redundant boxings.
  - *JIT Handling*: Recursive `unbox_numeric` in `primop.rs`.
  - *Normalization*: `stripBoxCon` pass should flatten `Con(I#, [Con(I#, [x])])` -> `Con(I#, [x])`.

- **Boxed vs Unboxed literals in Unions**: Single-module compilation often unboxes these, but cross-module merges might leave a `Con(W#, [Lit])` where a direct `Lit` was expected.
  - *JIT Handling*: Case-by-case branching in `effect_machine.rs`.
  - *Normalization*: Canonicalize `Con(Box, [Lit])` -> `Lit` for all known primitive boxes (I#, W#, F#, D#).

- **DataCon Wrapper vs Worker**: Wrappers take boxed args, workers take unboxed. After inlining, we might see `Con worker_id [Con box_id [Lit]]`.
  - *JIT Handling*: The `unbox_*` helpers handle this by checking tags.
  - *Normalization*: A pass could rewrite `Con worker_id [Con box_id [Lit]]` -> `Con worker_id [Lit]` if the worker expects an unboxed field.

## 4. Shape-related error paths (precondition candidates)

- **tidepool-codegen/src/effect_machine.rs:186**: `YieldError::UnexpectedTag(tag)` - Raised when a non-Con is matched as a Freer continuation.
- **tidepool-codegen/src/effect_machine.rs:279**: `YieldError::UnexpectedConTag(con_tag)` - Raised when an unknown effect tag is encountered.
- **tidepool-codegen/src/effect_machine.rs:205**: `YieldError::BadEFields(num_fields)` - Raised when an `E` node has the wrong number of fields.
- **tidepool-codegen/src/heap_bridge.rs:165**: `BridgeError::TooManyFields` - Raised during unmarshaling if a Con exceeds `MAX_FIELDS`.

## 5. CoreFrame variants and current invariants

- **CoreFrame::Var(VarId)**: Invariant: Must be bound in the environment or a known external.
- **CoreFrame::Lit(Literal)**: Invariant: Post-optimization, these should ideally be the raw arguments to workers or primops.
- **CoreFrame::App { fun, arg }**: Invariant: `fun` should evaluate to a function or thunk. Normalization could ensure that `arg` is unboxed if `fun` is a primop wrapper.
- **CoreFrame::Case { scrutinee, alts, ... }**: Invariant: `scrutinee` must not be `unsafeEqualityProof` (handled in Translate.hs). Normalization could ensure `scrutinee` is always forced or a direct constructor.
- **CoreFrame::Con { tag, fields }**: Invariant: `fields` may be boxed or unboxed. Normalization goal: workers always get unboxed fields, wrappers get boxed.

## 6. Recommendations

1. **Rule: `flattenBoxRecursion`**:
   - *Transforms*: `Con(BoxId, [Con(BoxId, [x])])` -> `Con(BoxId, [x])`.
   - *Reduction*: Simplifies `unbox_numeric` in JIT; allows removing the recursive loop in most cases.
   - *Risk*: Low, as long as `BoxId` is a known primitive box (I#, W#, etc.).

2. **Rule: `canonicalizeEffectTags`**:
   - *Transforms*: `Con(W#, [LitWord n])` -> `LitWord n` in the second field of an `E` continuation.
   - *Reduction*: Deletes ad-hoc branching in `effect_machine.rs:241-255`.
   - *Risk*: Requires identifying "effect tag" positions in the IR.

3. **Rule: `unboxPrimArgs`**:
   - *Transforms*: `PrimOp { op, args: [Con(BoxId, [Lit x]), ...] }` -> `PrimOp { op, args: [Lit x, ...] }`.
   - *Reduction*: Moves unboxing logic from Cranelift emission to IR optimization.
   - *Risk*: Moderate; must ensure the `PrimOp` actually expects an unboxed value.

4. **Rule: `elideRedundantCasts`**:
   - *Transforms*: IR-level casts that are identity mappings (already partially handled by `stripTicksAndCasts` in Haskell).
   - *Reduction*: Reduces noise in the IR tree.
   - *Risk*: Very low.
