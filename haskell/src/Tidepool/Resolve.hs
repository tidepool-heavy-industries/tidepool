module Tidepool.Resolve (resolveExternals, UnresolvedVar(..)) where

import GHC.Core (CoreBind, CoreExpr, Bind(..), maybeUnfoldingTemplate)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Types.Id (Id, idUnfolding, realIdUnfolding, isGlobalId, isPrimOpId_maybe, isDataConWorkId_maybe, isDataConWrapId_maybe)
import GHC.Utils.Outputable (showPprUnsafe)
import Debug.Trace (trace)
import GHC.Types.Var (Var, varName, varUnique)
import GHC.Types.Var.Set (VarSet, emptyVarSet, unitVarSet, elemVarSet, extendVarSet)
import GHC.Types.Unique (getKey)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Name (nameOccName, nameModule_maybe)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Unit.Module (moduleName, moduleNameString)
import Data.Word (Word64)

data UnresolvedVar = UnresolvedVar
  { uvKey    :: !Word64
  , uvName   :: !String
  , uvModule :: !String
  } deriving (Show)

-- | Resolve cross-module references by inlining their unfoldings.
--
-- Uses exprSomeFreeVars (const True) instead of exprFreeVars because
-- the latter filters to isLocalVar, excluding all GlobalIds. Since we
-- serialize Core into a self-contained CBOR tree (no linker), we need
-- to discover and inline global references too.
--
-- Returns: (augmented bindings, list of variables that could not be resolved).
resolveExternals :: [CoreBind] -> ([CoreBind], [UnresolvedVar])
resolveExternals binds =
  let localBinders = foldMap bindersOfSet binds
      allFreeVars  = foldMap freeVarsOfBind binds
      externals    = filter (isResolvable localBinders) (nonDetEltsUniqSet allFreeVars)
      (resolvedBinds, _, unresolved) = go externals emptyVarSet localBinders [] []
      resolvedPairs = concatMap toRecPairs resolvedBinds
      augmented = case resolvedPairs of
        []  -> binds
        _   -> Rec resolvedPairs : binds
  in (augmented, unresolved)
  where
    go :: [Var] -> VarSet -> VarSet -> [CoreBind] -> [UnresolvedVar]
       -> ([CoreBind], VarSet, [UnresolvedVar])
    go [] visited _ acc unres = (reverse acc, visited, reverse unres)
    go (v:rest) visited localSet acc unres
      | elemVarSet v visited = go rest visited localSet acc unres
      | otherwise =
          let visited' = extendVarSet visited v
              vName = occNameString (nameOccName (varName v))
              handleUnfolding unfoldingExpr =
                let newBind = NonRec v unfoldingExpr
                    -- Use exprSomeFreeVars (const True) here too, so we
                    -- discover globals in the inlined unfolding bodies.
                    newFVs = exprSomeFreeVars (const True) unfoldingExpr
                    localSet' = extendVarSet localSet v
                    newExternals = filter (isResolvable localSet')
                                         (nonDetEltsUniqSet newFVs)
                in go (newExternals ++ rest) visited' localSet' (newBind : acc) unres
          in case maybeUnfoldingTemplate (idUnfolding v) of
               Just unfoldingExpr -> handleUnfolding unfoldingExpr
               Nothing -> case maybeUnfoldingTemplate (realIdUnfolding v) of
                 Just unfoldingExpr -> handleUnfolding unfoldingExpr
                 Nothing ->
                   trace ("  [resolve] FAIL " ++ vName ++ ": " ++ showPprUnsafe (idUnfolding v)) $
                   let uv = UnresolvedVar
                         { uvKey    = fromIntegral (getKey (varUnique v))
                         , uvName   = occNameString (nameOccName (varName v))
                         , uvModule = case nameModule_maybe (varName v) of
                                        Just m  -> moduleNameString (moduleName m)
                                        Nothing -> "<no module>"
                         }
                   in go rest visited' localSet acc (uv : unres)

    toRecPairs :: CoreBind -> [(Var, CoreExpr)]
    toRecPairs (NonRec b rhs) = [(b, rhs)]
    toRecPairs (Rec pairs)    = pairs

    isResolvable :: VarSet -> Var -> Bool
    isResolvable localSet v =
      isGlobalId v
      && not (elemVarSet v localSet)
      && not (isPrimOp v)
      && not (isDataCon v)

    bindersOfSet :: CoreBind -> VarSet
    bindersOfSet (NonRec b _) = unitVarSet b
    bindersOfSet (Rec pairs) = foldl (\s (b, _) -> extendVarSet s b) emptyVarSet pairs

    -- | Collect ALL free variables including globals.
    -- exprFreeVars only returns local vars (isLocalVar filter).
    -- We need globals too since there's no linker in our JIT runtime.
    freeVarsOfBind :: CoreBind -> VarSet
    freeVarsOfBind (NonRec _ rhs) = exprSomeFreeVars (const True) rhs
    freeVarsOfBind (Rec pairs) = foldMap (exprSomeFreeVars (const True) . snd) pairs

    isPrimOp :: Id -> Bool
    isPrimOp v = case isPrimOpId_maybe v of
      Just _  -> True
      Nothing -> False

    isDataCon :: Id -> Bool
    isDataCon v = case isDataConWorkId_maybe v of
      Just _  -> True
      Nothing -> case isDataConWrapId_maybe v of
        Just _  -> True
        Nothing -> False
