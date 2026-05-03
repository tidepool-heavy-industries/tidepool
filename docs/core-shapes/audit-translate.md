# Audit: Translate.hs

## emitRuntimeUnpackCString

- **Location:** `haskell/src/Tidepool/Translate.hs:131` (`emitRuntimeUnpackCString`)
- **Trigger shape:** `unpackCString# addr` where `addr` is not a static literal (e.g. `plusAddr# a 1#`)
- **Normalized output:** `NLetRec` wrapping a recursive lambda using `IndexCharOffAddr` and `PlusAddr` primops to build a `(:)` chain
- **Mode:** `always-on`
- **Motivation:** GHC's `unpackCString#` unfolding relies on `Addr#` arithmetic which the JIT avoids; this provides a safe runtime implementation for dynamic strings.
- **Test coverage:** `uncovered` (most strings in tests are static literals)
- **Notes:** Used as a fallback for `isUnpackCStringVar` and `isShowDoubleVar` intercepts.

## emitRuntimeUnpackAppendCString

- **Location:** `haskell/src/Tidepool/Translate.hs:172` (`emitRuntimeUnpackAppendCString`)
- **Trigger shape:** `unpackAppendCString# addr suffix` where `addr` is not a static literal
- **Normalized output:** `NLetRec` recursive loop similar to `emitRuntimeUnpackCString` but terminates with `suffix` instead of `[]`
- **Mode:** `always-on`
- **Motivation:** Handles dynamic strings in `(++)` or `show` that GHC optimizes into `unpackAppendCString#`.
- **Test coverage:** `uncovered`
- **Notes:** Used by `isShowDoubleSpecVar` to preserve the `ShowS` continuation.

## emitShowDoubleSpecBody

- **Location:** `haskell/src/Tidepool/Translate.hs:210` (`emitShowDoubleSpecBody`)
- **Trigger shape:** `$fShowDouble_$sshowSignedFloat` binder (GHC's specialized `show` for `Double`)
- **Normalized output:** `NLam` chain wrapping `emitRuntimeUnpackAppendCString` using `ShowDoubleAddr` primop
- **Mode:** `always-on`
- **Motivation:** GHC's `floatToDigits` / `Integer` pipeline used in standard `show` for `Double` is too complex for the JIT and pulls in incompatible library code.
- **Test coverage:** `showDoublePrelude` in `haskell/test/Suite.hs`, `test_show_double` in `tidepool-runtime/tests/repro_split.rs`
- **Notes:** Cited in memory note #17. Essential for `Show` instances on `Double`.

## reachableBinds

- **Location:** `haskell/src/Tidepool/Translate.hs:282` (`reachableBinds`)
- **Trigger shape:** List of `CoreBind` during module translation
- **Normalized output:** Filtered list of `CoreBind` containing only transitively reachable bindings
- **Mode:** `always-on`
- **Motivation:** Prevents single large `Rec` groups from pulling in the entire transitive closure of a module's bindings, reducing node count and avoiding resolution failures.
- **Test coverage:** `prelude_length` in `haskell/test/Suite.hs` (exercises reachability into `Tidepool.Prelude`)
- **Notes:** Uses fine-grained reachability by flattening `Rec` groups into individual pairs for analysis.

## isShowDoubleVar intercept

- **Location:** `haskell/src/Tidepool/Translate.hs:484` (`translate`)
- **Trigger shape:** `Var showDouble` (or `showDouble'`) applied to 0 or 1 args
- **Normalized output:** `NPrimOp "ShowDoubleAddr"` followed by `emitRuntimeUnpackCString`
- **Mode:** `always-on`
- **Motivation:** Direct interception of `Double` to `String` conversion to bypass GHC's `Integer`-heavy pipeline.
- **Test coverage:** `show_double` in `tidepool-eval/tests/haskell_suite.rs`, `showDouble` in `haskell/test/Suite.hs`
- **Notes:** Memory #17. Handles both direct calls and eta-expanded variants.

## isShowDoubleSpecVar intercept

- **Location:** `haskell/src/Tidepool/Translate.hs:503` (`translate`)
- **Trigger shape:** `Var $fShowDouble_$sshowSignedFloat` (specialized `Double` show)
- **Normalized output:** `NPrimOp "ShowDoubleAddr"` followed by `emitRuntimeUnpackAppendCString`
- **Mode:** `always-on`
- **Motivation:** Intercepts specialized versions produced by GHC -O2 that include the `ShowS` continuation.
- **Test coverage:** `showDoublePrelude` in `haskell/test/Suite.hs`
- **Notes:** Memory #17. Crucial for derived `Show` instances on types containing `Double`.

## isUnpackCStringVar static desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:527` (`translate`)
- **Trigger shape:** `unpackCString# "lit"#` or `unpackCStringUtf8# "lit"#`
- **Normalized output:** Static `NCon` chain of `(:)` cells (cons-cells)
- **Mode:** `always-on`
- **Motivation:** Converts `Addr#` literals into uniform heap-allocated `[Char]` so that case matching and list functions work correctly.
- **Test coverage:** `lit_char_a` (via character literals), `text_pack` in `haskell/test/TextSuite.hs`
- **Notes:** Essential for all string literal handling in Core.

## isUnpackCStringVar dynamic fallback

- **Location:** `haskell/src/Tidepool/Translate.hs:545` (`translate`)
- **Trigger shape:** `unpackCString# addr` where `addr` is a variable
- **Normalized output:** `emitRuntimeUnpackCString` call
- **Mode:** `always-on`
- **Motivation:** Handles non-static `Addr#` values (e.g. from pointer arithmetic) gracefully at runtime.
- **Test coverage:** `uncovered`
- **Notes:** Less common than the static version.

## isUnpackAppendCStringVar dynamic fallback

- **Location:** `haskell/src/Tidepool/Translate.hs:554` (`translate`)
- **Trigger shape:** `unpackAppendCString# addr suffix` where `addr` is dynamic
- **Normalized output:** `emitRuntimeUnpackAppendCString` call
- **Mode:** `always-on`
- **Motivation:** Support for dynamic string concatenation/appending optimized by GHC.
- **Test coverage:** `uncovered`
- **Notes:** Memory note #13.

## isUnpackAppendCStringVar partial/eta-reduced

- **Location:** `haskell/src/Tidepool/Translate.hs:564` (`translate`)
- **Trigger shape:** `unpackAppendCString#` applied to 0 or 1 arguments
- **Normalized output:** `NLam` wrapper around runtime or static unpack loop
- **Mode:** `always-on`
- **Motivation:** Handles eta-reduced applications of the builtin, common in specialized `Show` instances.
- **Test coverage:** `showDoublePrelude` in `haskell/test/Suite.hs`
- **Notes:** Line 591 handles the zero-arg case specifically.

## isUnpackAppendCStringVar static prefix desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:604` (`translate`)
- **Trigger shape:** `unpackAppendCString# "prefix"# suffix`
- **Normalized output:** Static `NCon` chain of `(:)` cells ending in `suffix`
- **Mode:** `always-on`
- **Motivation:** Optimal conversion of static prefix strings with dynamic tails.
- **Test coverage:** `showInt` in `haskell/test/Suite.hs` (specialized show often uses this)
- **Notes:** Memory note #13.

## isErrorVar intercept

- **Location:** `haskell/src/Tidepool/Translate.hs:619` (`translate`)
- **Trigger shape:** `Var error` (and variants like `patError`, `recSelError`) applied to arguments
- **Normalized output:** `NApp` to error handler node (VarId 0x45...02) with extracted message literal
- **Mode:** `always-on`
- **Motivation:** Preserves the error message string as a `LEString` literal so the JIT can report it, rather than just crashing.
- **Test coverage:** PR #153, memory note #5.
- **Notes:** Extracts message from `unpackCString#` or `PushCallStack` wrappers recursively.

## isUnpackFoldrCStringVar static desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:638` (`translate`)
- **Trigger shape:** `unpackFoldrCString# "lit"# f z`
- **Normalized output:** Static application chain `f c1 (f c2 (... z))`
- **Mode:** `always-on`
- **Motivation:** Handles GHC's build/foldr fusion for string literals without requiring runtime `Addr#` arithmetic.
- **Test coverage:** Memory note #11.
- **Notes:** Avoids complex unfoldings of `unpackFoldrCString#`.

## isAppendVar desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:654` (`translate`)
- **Trigger shape:** `Var (++)` applied to 2 arguments
- **Normalized output:** `NLetRec` recursive loop implementing list concatenation
- **Mode:** `always-on`
- **Motivation:** `GHC.Internal.Base.++` often has no unfolding in `.hi` files; this provides a reliable, JIT-friendly implementation.
- **Test coverage:** `prelude_string_append` in `haskell/test/Suite.hs`, `tidepool-runtime/tests/text_spliton.rs`
- **Notes:** Memory note #12.

## isUnsafeTakeVar desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:686` (`translate`)
- **Trigger shape:** `Var $wunsafeTake` or `unsafeTake` applied to 2 arguments
- **Normalized output:** `NLetRec` recursive loop with unboxed `Int#` counter and `IntLe`/`IntSub` primops
- **Mode:** `always-on`
- **Motivation:** GHC worker-wrappers for `take` at -O2 result in `unsafeTake` calls whose unfoldings are often missing or rely on complex pointer logic.
- **Test coverage:** `prelude_take` in `haskell/test/Suite.hs`
- **Notes:** Memory note #16.

## isUnsafeEqualityProofVar desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:752` (`translate`)
- **Trigger shape:** `Var unsafeEqualityProof`
- **Normalized output:** `NCon` unit value (`()`)
- **Mode:** `always-on`
- **Motivation:** GHC uses this for GADT equality evidence. Emitting a unit value allows it to be erased or matched against `UnsafeRefl` (unit constructor) safely.
- **Test coverage:** PR #71, `tidepool-runtime/tests/multi_module_datacon.rs`
- **Notes:** Memory note #1.

## isRunRWVar desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:761` (`translate`)
- **Trigger shape:** `runRW# f`
- **Normalized output:** `NApp f ()` (state token erasure)
- **Mode:** `always-on`
- **Motivation:** `runRW#` is the underlying primop for `unsafePerformIO`. Erasing the state token allows pure JIT execution of IO-based initialization.
- **Test coverage:** Memory note #14.
- **Notes:** Handles both direct application and eta-reduced variants.

## tagToEnum# desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:778` (`translate`)
- **Trigger shape:** `tagToEnum# @T arg`
- **Normalized output:** `NCase` on `arg` with one `FLitAlt` per constructor of type `T`
- **Mode:** `always-on`
- **Motivation:** `tagToEnum#` is a magical primop that requires type information erased at runtime; desugaring to a `Case` preserves the mapping.
- **Test coverage:** Memory note #2.
- **Notes:** Requires resolvable type argument `T` to identify the target `TyCon`.

## isRuntimeErrorVar / isErrorVar handling

- **Location:** `haskell/src/Tidepool/Translate.hs:832` (`translateHead`)
- **Trigger shape:** `divZeroError`, `overflowError`, or general `error` / `undefined`
- **Normalized output:** `NVar` with special error tag `0x45...`
- **Mode:** `always-on`
- **Motivation:** Converts known GHC error sentinels into JIT-native error nodes that trigger safe traps.
- **Test coverage:** Memory note #5, #15.
- **Notes:** Includes `undefined` (kind 3) and `divZeroError` (kind 0).

## isRealWorldVar

- **Location:** `haskell/src/Tidepool/Translate.hs:837` (`translateHead`)
- **Trigger shape:** `Var realWorld#`
- **Normalized output:** `NLit (LEInt 0)` (dummy literal)
- **Mode:** `always-on`
- **Motivation:** Erases the `RealWorld#` state token which has no runtime representation in the JIT.
- **Test coverage:** `uncovered`
- **Notes:** Part of state-token erasure pipeline.

## isTypeMetadataVar

- **Location:** `haskell/src/Tidepool/Translate.hs:839` (`translateHead`)
- **Trigger shape:** Variables starting with `$trModule`, `$krep`, `$tc`, etc.
- **Normalized output:** `NVar` with error tag (kind 4 - type metadata)
- **Mode:** `always-on`
- **Motivation:** GHC generates massive amounts of Typeable metadata that have no runtime use in Tidepool; emitting error nodes avoids resolving useless unfoldings.
- **Test coverage:** Memory note #9.
- **Notes:** These vars are skipped by the resolver but can appear in inlined code.

## jumpCrossesLam (Join Point conversion)

- **Location:** `haskell/src/Tidepool/Translate.hs:854` (`translateHead`)
- **Trigger shape:** `Let (NonRec b rhs) body` where `b` is a join point used inside a lambda in `body`
- **Normalized output:** `NLetNonRec` wrapping a lambda (regular function) instead of `NJoin`
- **Mode:** `always-on`
- **Motivation:** Cranelift blocks (used for `NJoin`) cannot be jumped to from separate lambda functions; this promotes them to full closures.
- **Test coverage:** Memory note #10.
- **Notes:** Also applies to `Let (Rec ...)` joinrec binders at line 863.

## splitMultiReturnPrimOp desugar

- **Location:** `haskell/src/Tidepool/Translate.hs:902` (`translateHead`)
- **Trigger shape:** `Case (op a b) of (# q, r #) -> body` where `op` is `quotRemInt#`, etc.
- **Normalized output:** Nested `NCase` nodes each calling a single-return version of the primop (e.g. `IntQuot` and `IntRem`)
- **Mode:** `always-on`
- **Motivation:** The JIT backend prefers single-result primops for simplicity; this ensures shared arguments and correct forcing.
- **Test coverage:** PR #71, `prim_quot_rem_int` in `haskell/test/Suite.hs`
- **Notes:** Also handles unary (`decodeDouble_Int64#`) and triple-return (`timesInt2#`) variants.

## Stateful primop state erasure

- **Location:** `haskell/src/Tidepool/Translate.hs:960` (`translateHead`)
- **Trigger shape:** `Case (op args... s) of (# s', results... #) -> body`
- **Normalized output:** `NPrimOp` with `s` dropped, results bound via `NCase`, `s'` bound to dummy
- **Mode:** `always-on`
- **Motivation:** Supports stateful primops (like `readSmallArray#`) by stripping the `State#` tokens that Tidepool does not represent at runtime.
- **Test coverage:** Memory note #14.
- **Notes:** Crucial for all mutable state / IO operations in the JIT.

## isUnboxedTupleDataCon (general)

- **Location:** `haskell/src/Tidepool/Translate.hs:1022` (`translateHead`)
- **Trigger shape:** `Case scrut of (# binders #) -> body`
- **Normalized output:** `NCase` with `FDataAlt` for multi-element or `FDefault` for single-element/void
- **Mode:** `always-on`
- **Motivation:** Uniform handling of unboxed tuples as ephemeral heap boxes or literals.
- **Test coverage:** `case_pair` in `haskell/test/Suite.hs`
- **Notes:** Handles zero, single, and multiple binders.

## isUnsafeEqualityCase elision

- **Location:** `haskell/src/Tidepool/Translate.hs:1047` (`translateHead`)
- **Trigger shape:** `case unsafeEqualityProof of UnsafeRefl -> body`
- **Normalized output:** Direct translation of `body` (case elided)
- **Mode:** `always-on`
- **Motivation:** PR #272. Inlined `unsafeEqualityProof` cases survive GHC's optimizer; eliding them prevents tag mismatch (CASE TRAP) with unit-represented evidence.
- **Test coverage:** PR #272, `tidepool-runtime/tests/multi_module_datacon.rs`
- **Notes:** Memory note #1. Cited as a major cross-module bug fix.

## Coercion placeholder

- **Location:** `haskell/src/Tidepool/Translate.hs:1059` (`translateHead`)
- **Trigger shape:** `Coercion _` in expression position
- **Normalized output:** `NLit (LEInt 0)` (dummy literal)
- **Mode:** `defensive (no known triggering source)`
- **Motivation:** Coercions are zero-cost; if they survive to expression position (rare), we provide a dummy value to avoid crashing the translator.
- **Test coverage:** `uncovered`
- **Notes:** Linked to GHC inlining through vendored code.

## localVarId hash disambiguation

- **Location:** `haskell/src/Tidepool/Translate.hs:1090` (`localVarId`)
- **Trigger shape:** Non-external `Var`
- **Normalized output:** 64-bit ID hashed from `OccName` + GHC `Unique`
- **Mode:** `always-on`
- **Motivation:** Raw GHC uniques collide across modules when inlining; hashing with `OccName` disambiguates them and prevents "binder shadow" bugs.
- **Test coverage:** `tidepool-runtime/tests/text_spliton.rs` (complex multi-module test)
- **Notes:** Uses the lower 56 bits for the hash.

## stableVarId

- **Location:** `haskell/src/Tidepool/Translate.hs:1113` (`stableVarId`)
- **Trigger shape:** External `Name`
- **Normalized output:** 64-bit ID with tag `0xFE`, hashed from `ModuleName:OccName`
- **Mode:** `always-on`
- **Motivation:** Ensures that external references (like `base:GHC.Base.map`) have identical IDs regardless of which module they are referenced from.
- **Test coverage:** All cross-module tests.
- **Notes:** Crucial for linking JIT-compiled modules with the runtime's wired-in DataCon table.

## valueRepArity

- **Location:** `haskell/src/Tidepool/Translate.hs:1446` (`valueRepArity`)
- **Trigger shape:** `DataCon` arity calculation
- **Normalized output:** `dataConRepArity` minus length of `EqSpec`
- **Mode:** `always-on`
- **Motivation:** GADT constructors include equality evidence in `dataConRepArity`, but Core passes these as `Coercion` args which are erased; this adjustment aligns the JIT arity with the Core application.
- **Test coverage:** Memory note #8.
- **Notes:** Prevents arity mismatch on GADT constructors in `freer-simple`.
