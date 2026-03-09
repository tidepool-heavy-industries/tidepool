# Current Investigation: Free-variable capture bug with T.intercalate

## What's Done (committed & pushed)

### Alpha-rename (commit 4c8ad2d)
- `haskell/src/Tidepool/Resolve.hs` — `alphaRenameExpr` uses `substExpr` + `InScopeSet` to rename local binders in inlined unfoldings. Applied to all 3 inlining paths (standard, spec fallback, fat interface Rec).
- `haskell/src/Tidepool/Translate.hs` — `localVarId` hashes OccName + unique as defense-in-depth.
- `tidepool-mcp/src/lib.rs` — improved `ask` prompt extraction error handling.
- `tidepool-mcp/tests/spliton_repro.rs` — cleaned up bisect scaffolding, kept 2 repro tests.
- `tidepool-runtime/tests/text_spliton.rs` — 24 tests (original + collision + adversarial), all pass.

### Failing tests (commit 9adb940)
- 4 failing tests documenting the free-variable capture bug
- 4 passing control tests confirming the boundary

## The New Bug

**Symptom**: `T.intercalate` silently dropped when called inside a closure that captures a free variable used with `(++)`.

**Minimal repro**:
```haskell
let f b = T.intercalate "/" (["fixed"] ++ b)
in [f ["x"], f ["y"]]
-- Expected: ["fixed/x", "fixed/y"]
-- Actual:   [["fixed","x"], ["fixed","y"]]  (intercalate dropped, raw ++ result)
-- In MCP eval context: "Haskell undefined forced" crash instead
```

**Boundary conditions** (all confirmed by tests):
| Pattern | Result |
|---------|--------|
| `f` called once | ✓ works |
| `f` called twice (let bindings, list literal, map, zipWith) | ✗ fails |
| Both lists as **arguments** (not free vars) | ✓ works |
| `T.concat` instead of `T.intercalate` | ✓ works |
| Prelude `intercalate` (monomorphic shadow) | ✓ works |
| `length` instead of `T.intercalate` | ✓ works |
| `filter` on the `++` result | ✓ works |
| Manual `myAppend` instead of `(++)` | ✓ works |
| `intersperse` instead of `intercalate` | ✓ works |
| `T.unwords` (which uses intercalate internally) | ✗ fails |

**Key insight**: It's NOT about the second call. Even `head [f ["x"], f ["y"]]` crashes. The mere existence of a second call site changes codegen enough to break the first.

## Hypotheses

1. **Thunk sharing in `(++)`'s free-var tail**: `(++)` shares the captured `["fixed"]` list. `T.intercalate`'s strict traversal forces a thunk, and the second call site causes the codegen to share that thunk incorrectly.

2. **Case binder aliasing in T.intercalate**: T.intercalate's inlined case expression has a binder that aliases the scrutinee. If compiled as a shared binding, the second call site sees stale data.

3. **GHC float-out of `["fixed"]` or `(++) ["fixed"]`**: GHC floats the free-var expression to a shared let binding that gets thunkified and evaluated once.

4. **T.intercalate's ByteArray# ops corrupt heap state**: The inlined Data.Text.intercalate uses Array operations that the JIT doesn't handle correctly on reuse.

5. **VarId collision within the same handleUnfolding scope**: Alpha-rename operates per-handleUnfolding call but `(++)` and `T.intercalate` get inlined together, so internal binders may still collide.

## Failing Tests
```bash
cargo test -p tidepool-runtime --test text_spliton -- freevar_  # 4 fail, expected
cargo test -p tidepool-runtime --test text_spliton -- args_intercalate  # passes (control)
cargo test -p tidepool-runtime --test text_spliton -- freevar_concat  # passes (control)
cargo test -p tidepool-runtime --test text_spliton -- freevar_prelude  # passes (control)
cargo test -p tidepool-runtime --test text_spliton -- freevar_length  # passes (control)
```

## Key Files
- `tidepool-runtime/tests/text_spliton.rs` — all tests (passing + failing)
- `tidepool-codegen/src/emit/expr.rs` — closure emission, LetRec phases, capture handling
- `tidepool-codegen/src/emit/case.rs` — case binder compilation
- `tidepool-codegen/src/host_fns.rs:298` — `heap_force` (thunk evaluation)
- `haskell/src/Tidepool/Resolve.hs` — alpha-rename + unfolding inlining
- `haskell/src/Tidepool/Translate.hs` — Core→IR translation, `varId`
