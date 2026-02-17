# Research: GHC 9.12 API Surface

**Priority:** MEDIUM — de-risks the Haskell harness (phase 1, wave 3)
**Status:** COMPLETE — from GHC source analysis + nix verification
**GHC version confirmed:** 9.12.2 (via `nix develop` in both exomonad and tidepool flakes)

## 1. freer-simple Compatibility

**Vendored copy:** `haskell/vendor/freer-simple/freer-simple.cabal` is version **1.2.1.2**.

**BLOCKER:** `template-haskell < 2.19` constraint. GHC 9.12 ships template-haskell 2.23.

**Recommended fix:** Remove `Control.Monad.Freer.TH` from the vendored copy and drop the `template-haskell` dependency entirely. Tidepool only needs the core `Eff`/`Free`/`Union` types — it never uses `makeEffect` TH splices. The core library code is pure Haskell 2010 with standard GHC extensions (GADTs, DataKinds, TypeOperators, RankNTypes) — these compile fine on any GHC 9.x.

**Alternative:** `allow-newer: freer-simple:template-haskell` in `cabal.project`.

## 2. Pipeline Functions (GHC 9.12)

### parseModule
- **Module:** `GHC` (re-exports from `GHC.Driver.Main`)
- **Signature:** `parseModule :: GhcMonad m => ModSummary -> m ParsedModule`
- **Lower-level:** `GHC.Driver.Main.hscParse :: HscEnv -> ModSummary -> IO HsParsedModule`

### typecheckModule
- **Module:** `GHC`
- **Signature:** `typecheckModule :: GhcMonad m => ParsedModule -> m TypecheckedModule`
- **Lower-level:** `GHC.Driver.Main.hscTypecheckRename :: HscEnv -> ModSummary -> HsParsedModule -> IO (TcGblEnv, RenamedStuff)`

### hscDesugar
- **Module:** `GHC.Driver.Main`
- **Signature:** `hscDesugar :: HscEnv -> ModSummary -> TcGblEnv -> IO ModGuts`
- **Note:** Not `deSugar` (which lives in `GHC.HsToCore` with a more complex signature). `hscDesugar` is the driver-level wrapper.

### core2core / hscSimplify
- **Recommended:** `GHC.Core.Opt.Pipeline.core2core :: HscEnv -> ModGuts -> IO ModGuts`
- This reads the pass list from `DynFlags` internally.
- **CAUTION:** `hscSimplify` signature may differ between 9.10 and 9.12. `core2core` is the safer entry point.

### Complete pipeline snippet

```haskell
import GHC
import GHC.Driver.Main (hscDesugar)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Driver.Session (updOptLevel)
import GHC.Driver.Backend (noBackend)

runGhc (Just libdir) $ do
  dflags <- getSessionDynFlags
  let dflags' = updOptLevel 2 $ dflags
        { backend = noBackend
        , ghcLink = NoLink
        }
  setSessionDynFlags dflags'
  target <- guessTarget "Input.hs" Nothing Nothing
  setTargets [target]
  modGraph <- depanal [] False
  let modSum = head (mgModSummaries modGraph)
  parsed <- parseModule modSum
  typechecked <- typecheckModule parsed
  hscEnv <- getSession
  let tcGblEnv = fst (tm_internals_ typechecked)
  desugared <- liftIO $ hscDesugar hscEnv modSum tcGblEnv
  simplified <- liftIO $ core2core hscEnv desugared
  -- simplified.mg_binds :: [CoreBind]
  -- simplified.mg_tcs :: [TyCon]
  return simplified
```

## 3. DynFlags Setup (GHC 9.12)

| Setting | How | Notes |
|---------|-----|-------|
| Backend | `dflags { backend = noBackend }` | `noBackend` is a lowercase smart constructor from `GHC.Driver.Backend`. **NOT** `NoBackend` (capitalized). |
| Link mode | `dflags { ghcLink = NoLink }` | Stable across all 9.x |
| Optimization | `updOptLevel 2 dflags` | Function, not field update. From `GHC.Driver.Session`. |
| Package DB | Inherited from `GHC_PACKAGE_PATH` env var | Set by nix develop |

## 4. Key Types for Serializer

| Function | Module | Signature | Notes |
|----------|--------|-----------|-------|
| `isJoinId_maybe` | `GHC.Types.Id` | `Id -> Maybe JoinArity` | `JoinArity = Int` from `GHC.Types.Basic`. Stable since 8.8. |
| `dataConTag` | `GHC.Core.DataCon` | `DataCon -> ConTag` | `ConTag = Int`, 1-indexed |
| `dataConRepArgTys` | `GHC.Core.DataCon` | `DataCon -> [Scaled Type]` | **CHANGED in 9.0+** — returns `Scaled Type`, not `Type`. Use `map scaledThing`. |
| `dataConSrcBangs` | `GHC.Core.DataCon` | `DataCon -> [HsSrcBang]` | Still exists |
| `isDataConWorkId` | `GHC.Types.Id` | `Id -> Bool` | Or `idDetails` → match `DataConWorkId dc` |
| `isPrimOpId` | `GHC.Types.Id` | `Id -> Bool` | Or `isPrimOpId_maybe :: Id -> Maybe PrimOp` |

## 5. Anti-Pattern Rules for Gemini Workers

These MUST be front-loaded in any Haskell harness spec:

1. **DO NOT** write `dataConRepArgTys :: DataCon -> [Type]`. It returns `[Scaled Type]` in GHC 9.0+. Use `map scaledThing (dataConRepArgTys dc)`.
2. **DO NOT** write `NoBackend` (capitalized). Use `noBackend` (lowercase) from `GHC.Driver.Backend`.
3. **DO NOT** use `hscSimplify` without verifying signature. Use `core2core` from `GHC.Core.Opt.Pipeline`.
4. **DO NOT** assume `template-haskell` bounds are satisfied. Use `allow-newer` or patch vendored freer-simple.
5. **DO NOT** import `Backend` from `DynFlags`. Import from `GHC.Driver.Backend`.

## 6. Remaining Verification

The pipeline snippet above should be tested as a minimal working example in the tidepool nix shell. This can be done as part of the haskell-harness subtree scaffolding (phase 1, wave 3) rather than as a separate research task — the scaffold worker should compile and run this snippet as its first step.
