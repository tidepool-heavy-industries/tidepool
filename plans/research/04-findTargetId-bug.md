# Deep Research: GHC Core Binding Name Collision in translateModule

## Problem Statement

We have a Haskell-to-Rust compilation pipeline (Tidepool) that:
1. Uses the GHC 9.12.2 API to desugar+optimize Haskell source → GHC Core
2. Translates the Core AST into a flat node array (CBOR-serialized)
3. Evaluates the flat representation in a Rust tree-walking interpreter

The `translateModule` function in `Translate.hs` takes ALL top-level Core bindings from a module and wraps them as nested `LetNonRec`/`LetRec` expressions around a `Var` reference to the "target" binding (the one the user wants to evaluate). The target is identified by name using `occNameString (nameOccName (idName b)) == targetName`.

**The bug**: For a Haskell module containing `game :: Eff '[Console, Rng] ()`, calling `translateModule allBinds "game"` produces a tree whose root body-chain terminates at `Var(VarId(8286623314361716746))`, which is bound to `Con(I#, [1])` — a boxed integer literal 1. The CORRECT target binding has `VarId(8214565720323785845)` and is bound to `Con(E, [Union(...), FTCQueue(...)])`.

**Hypothesis**: GHC's optimizer generates auxiliary/worker bindings (visible in Core dumps as `game_s14e`, `game_s14f`, `game_s14h`, etc.) whose `occNameString` may also return `"game"` because the `_s14e` suffix is part of the GHC Unique, not the OccName. The `findTargetId` function finds the first match, which is an auxiliary binding.

## Context: GHC Core Dump (Abridged)

The `-ddump-simpl` output for Guess.hs shows (among many bindings):
```haskell
-- System-generated auxiliary bindings:
game_s14e = I# 1#          -- literal 1, used for randInt lower bound
game_s14f = I# 100#        -- literal 100, used for randInt upper bound
game_s14h = ...             -- lambda body of the guessLoop continuation
game_s14l = ...             -- another continuation piece

-- The real top-level binding:
game = E @'[Console, Rng] @() @Int game_s14h game_s14l
```

## Research Questions

### Q1: OccName vs Unique in GHC System-Generated Names

In GHC 9.12.2 (and earlier), when the simplifier generates auxiliary bindings like `game_s14e`:
- What is the `OccName` of such a binding? Is it `"game"` or `"game_s14e"`?
- Is the `_s14e` suffix part of the `OccName` or the `Unique`?
- Does `occNameString (nameOccName (idName b))` return `"game"` or `"game_s14e"` for these bindings?
- What GHC functions create these system names? (`mkSysLocalOrCoVar`, `mkSystemName`, etc.)
- Does the behavior differ between `-O0`, `-O1`, and `-O2`?

**Key GHC modules to check**: `GHC.Types.Name.Occurrence`, `GHC.Types.Unique`, `GHC.Types.Name`, `GHC.Types.Id.Make`, `GHC.Core.Opt.Simplify`.

### Q2: Distinguishing Top-Level User Bindings from System Bindings

Given a list of `CoreBind` from `mg_binds :: ModGuts -> [CoreBind]`:
- How can we distinguish the original user-written `game` binding from optimizer-generated `game_s14e` etc.?
- What properties does the original top-level binding's `Id` have that system-generated ones don't?
  - `isExportedId :: Id -> Bool` — does this work?
  - `isLocalId :: Id -> Bool` vs `isGlobalId :: Id -> Bool`
  - `idIsFrom :: Module -> Id -> Bool`
  - Name sort: `isExternalName :: Name -> Bool`, `isInternalName :: Name -> Bool`
  - `isSystemName :: Name -> Bool` (checks `NameSort`)
  - `OccName` namespace: `isVarOcc`, `isTvOcc`, `isTcOcc`
- After `core2core` optimization, do top-level bindings retain `isExportedId` / `isGlobalId` status?
- Does `isSystemName (idName b)` return True for the system-generated auxiliaries?

### Q3: Does `occNameString` Actually Collide?

This is the crux. We need to verify empirically:
- For a binding visible in Core dumps as `game_s14e`, is the full string `"game_s14e"` the OccName, or is `"game"` the OccName with `s14e` being the Unique suffix?
- In GHC's pretty-printer for Core, how are system names rendered? Does `pprBinder` append the Unique to the OccName for disambiguation?
- Could the collision be happening NOT in `occNameString` but elsewhere — e.g., multiple top-level bindings genuinely named `"game"` produced by the simplifier?

### Q4: Correct Approach to Find a Top-Level Binding by Name

What is the idiomatic way in the GHC API to find a specific top-level binding by its user-visible name in a `[CoreBind]`? Consider:

1. **Filter by `isExportedId`**: Only exported bindings should match user-written names.
   ```haskell
   findTargetId name binds =
     case [b | NonRec b _ <- binds, isExportedId b, occNameString (nameOccName (idName b)) == name]
          ++ [b | Rec pairs <- binds, (b, _) <- pairs, isExportedId b, occNameString (nameOccName (idName b)) == name]
     of (b:_) -> b
        []    -> error ...
   ```

2. **Filter by `isGlobalId`**: Global Ids are top-level and visible.

3. **Filter by `isExternalName . idName`**: External names are the ones visible from other modules.

4. **Filter by `not . isSystemName . idName`**: Exclude system-generated names.

5. **Use `lookupGlobalName`** or other GHC-provided lookup mechanisms instead of manual search.

Which approach is most robust for our use case? We're operating on the output of `core2core` (post-optimization), and we need to find the binding that corresponds to the user's `game :: Eff '[Console, Rng] ()` declaration.

### Q5: Order Dependence in `mg_binds`

- Are `mg_binds` ordered in a specific way? (dependency order, source order, etc.)
- After `core2core`, does the optimizer change the order of bindings?
- If two bindings have the same `occNameString`, which one appears first?
- Could the bug be order-dependent — i.e., the system-generated binding appears before the real one in the list?

### Q6: Alternative Identification Strategies

If name-based lookup is fundamentally fragile, what alternatives exist?
- **By type**: Can we match on the type `Eff '[Console, Rng] ()` of the binding? (Types are available on `Id` via `idType`)
- **By module + name**: Use qualified name matching
- **By exported status**: Only look at exported bindings
- **Unique-based**: Somehow obtain the Unique of the target binding before translation

## Code Under Investigation

```haskell
-- Translate.hs, lines 109-124
translateModule :: [CoreBind] -> String -> (Seq FlatNode, Map.Map Word64 DataCon)
translateModule allBinds targetName =
  let targetId = findTargetId targetName allBinds
      (_, finalState) = runState (wrapAllBinds allBinds targetId) (TransState Seq.empty Map.empty)
  in (tsNodes finalState, tsUsedDCs finalState)
  where
    findTargetId name binds =
      case concatMap (findInBind name) binds of
        (b:_) -> b
        []    -> error $ "translateModule: binding '" ++ name ++ "' not found"

    findInBind name (NonRec b _)
      | occNameString (nameOccName (idName b)) == name = [b]
      | otherwise = []
    findInBind name (Rec pairs) =
      [b | (b, _) <- pairs, occNameString (nameOccName (idName b)) == name]
```

## Environment

- **GHC version**: 9.12.2
- **Optimization level**: `-O2` with `Opt_FullLaziness` disabled (`gopt_unset ... Opt_FullLaziness`)
- **Pipeline**: `parseModule → typecheckModule → hscDesugar → core2core`
- **Haskell dependencies**: `freer-simple` (for `Eff`, `Member`, `send`)
- **Source module**: Single-file module `Guess` with `game :: Eff '[Console, Rng] ()`

## Expected Output Format

For each question, provide:
1. **Answer** with citations to GHC source code (module paths, function names, line numbers if possible)
2. **Verification approach** — how to confirm the answer empirically (GHCi commands, debug prints, etc.)
3. **Recommended fix** for our `findTargetId` function

The most valuable deliverable is a **concrete, correct implementation** of `findTargetId` that reliably selects the user-written top-level binding and ignores system-generated auxiliaries, with explanation of WHY it works based on GHC's internal invariants.
