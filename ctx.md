# T.splitOn Bug Investigation Context

## Bug Summary
`T.splitOn`, `T.words`, `T.lines` from `Data.Text` crash with "unresolved variable VarId(0x580000000000000c) [tag='X', key=12]" in the MCP eval context but work fine in standalone tests.

## Root Cause (IDENTIFIED)
**GHC Unique collision after cross-module inlining.**

VarId `0x580000000000000c` is NOT a single variable — it's **63 different GHC local variables** that all share the same Unique `(tag='X', key=12)`. They come from inlined unfoldings from different external modules (Data.Text.Internal, Data.Map, etc.).

### Mechanism:
1. GHC's unique supply for system-generated variables (tag 'X') is only unique within a single compilation unit
2. `resolveExternals` inlines unfoldings from multiple external modules
3. The local binders within those inlined bodies can share the same Unique across modules
4. `varId` for non-external names uses `fromIntegral (getKey (varUnique v))` — so all these distinct `Id`s map to the identical `Word64 = 0x580000000000000c`
5. The JIT sees multiple binders and references all using the same VarId
6. When one binding's scope doesn't contain a reference from a different inlined body, the lookup fails → `unresolved_var_trap`

### Why only in MCP context:
The full MCP preamble has ~300 lines of helper function bodies (effect orchestration, searchFiles, todoScan, etc.) that pull in additional external unfoldings. More inlined code = more chance of unique collisions.

### Bisect results (from the agent that was interrupted):
- The bisect agent created extensive tests in `tidepool-mcp/tests/spliton_repro.rs` with GROUP_A through GROUP_D constants
- Individual groups pass, but combinations fail
- The agent was deep into rounds 8-13 narrowing down to specific function pairs (kvAll + kvClear as one minimal trigger)
- The test file has been modified with many bisect test functions — review before using

## Key Variables Identified (from debug trace)
The 63 colliding variables include: `exit_Xc`, `ww_Xc`, `ds_Xc`, `bx_Xc`, `$j5_Xc`, `r#1_Xc`, etc. All are:
- `isLocal=True`, `isGlobal=False`, `isExternalName=False`
- `module=<no-module>`
- GHC unique = `(X, 12)` → `0x580000000000000c`

## Fix Direction
The fix needs to happen in `varId` in `haskell/src/Tidepool/Translate.hs` (line 1060). Currently:

```haskell
varId :: Var -> Word64
varId v = case isDataConId_maybe v of
  Just dc -> stableVarId (varName (dataConWorkId dc))
  Nothing -> if isExternalName (varName v)
             then stableVarId (varName v)
             else fromIntegral (getKey (varUnique v))
```

The `else` branch returns raw GHC uniques for local variables. When cross-module inlining brings in locals from different modules that happen to have the same Unique, they collide.

### Possible fixes:
1. **Make all variable IDs stable/unique**: Use a disambiguating counter or include more info in the ID (e.g., hash the OccName + parent binding context)
2. **Rename colliding locals during resolution**: In `resolveExternals`, when inlining an unfolding, alpha-rename all local binders to fresh uniques
3. **Use a different ID scheme for locals**: Instead of raw GHC Unique, use a monotonic counter during translation that guarantees uniqueness

Option 2 (alpha-renaming during inlining) is the most principled fix — it's what GHC's own inliner does. The `resolveExternals` function should rename local binders in inlined unfoldings to avoid collisions with existing locals.

## Key Files
- `haskell/src/Tidepool/Translate.hs` — `varId` (line 1060), `translateModuleClosed` (line 364)
- `haskell/src/Tidepool/Resolve.hs` — `resolveExternals`, `isResolvable`
- `tidepool-mcp/src/lib.rs` — `build_preamble`, `template_haskell` (lines 409, 1127)
- `tidepool-mcp/tests/spliton_repro.rs` — reproduction tests (MODIFIED with bisect tests)
- `tidepool-runtime/tests/text_spliton.rs` — additional tests (all pass, less comprehensive)
- `/tmp/tidepool_spliton_repro.hs` — dumped full MCP source (606 lines)

## Reproduction
```bash
# Exact reproduction:
cargo test -p tidepool-mcp --test spliton_repro repro_spliton_full_mcp -- --test-threads=1

# Via MCP eval:
# pure (T.splitOn "," "a,b,c")  → fails with unresolved variable

# Passes without user_library helpers:
cargo test -p tidepool-mcp --test spliton_repro repro_spliton_no_user_library -- --test-threads=1
```

## State of Working Tree
- `tidepool-mcp/src/lib.rs` — has a temporary debug dump line (unconditional write to /tmp/tidepool_mcp_source.hs) that should be removed
- `tidepool-mcp/tests/spliton_repro.rs` — has extensive bisect tests from the agent (large file, ~1400+ lines)
- `tidepool-runtime/tests/text_spliton.rs` — has 3-preamble-level tests (all pass)
- `haskell/src/Tidepool/Translate.hs` — was modified for debug traces but REVERTED (confirmed clean)

## Translate.hs varId fix sketch
```haskell
-- Option 2: alpha-rename in resolveExternals
-- In handleUnfolding, before adding the new bind:
--   1. Collect all local binders in unfoldingExpr
--   2. Generate fresh uniques for each
--   3. Substitute throughout unfoldingExpr
-- This ensures no two inlined unfoldings share local variable IDs.

-- Option 3: monotonic counter in varId (simpler but changes the whole ID scheme)
-- Replace the `else fromIntegral (getKey (varUnique v))` with a lookup
-- in a Map from Var to fresh Word64, populated during translation.
-- The TransState already has tsSynthCounter for fresh IDs.
```
