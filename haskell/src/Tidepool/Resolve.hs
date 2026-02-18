module Tidepool.Resolve (resolveExternals, UnresolvedVar(..)) where

import GHC.Core (CoreBind, CoreExpr, Bind(..), maybeUnfoldingTemplate)
import GHC.Core.FVs (exprFreeVars)
import GHC.Types.Id (Id, idUnfolding, isGlobalId, isPrimOpId_maybe, isDataConWorkId_maybe)
import GHC.Types.Var (Var, varName, varUnique)
import GHC.Types.Var.Set (VarSet, emptyVarSet, unitVarSet, unionVarSet, elemVarSet, extendVarSet)
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
-- Returns: (augmented bindings, list of variables that could not be resolved).
-- Unresolved variables are globals that have no unfolding available —
-- typically class dictionaries, NOINLINE functions, or magic GHC ids.
-- The caller should report these; they will cause UnboundVar errors
-- at evaluation time if actually demanded.
resolveExternals :: [CoreBind] -> ([CoreBind], [UnresolvedVar])
resolveExternals binds =
  let localBinders = foldMap bindersOfSet binds
      allFreeVars  = foldMap freeVarsOfBind binds
      externals    = filter (isResolvable localBinders) (nonDetEltsUniqSet allFreeVars)
      (resolvedBinds, _, unresolved) = go externals emptyVarSet localBinders [] []
      -- Wrap all resolved externals in a single Rec group so they can
      -- mutually reference each other. Individual NonRec bindings would
      -- create nested lets where outer thunks can't see inner bindings.
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
          in case maybeUnfoldingTemplate (idUnfolding v) of
               Nothing ->
                 let uv = UnresolvedVar
                       { uvKey    = fromIntegral (getKey (varUnique v))
                       , uvName   = occNameString (nameOccName (varName v))
                       , uvModule = case nameModule_maybe (varName v) of
                                      Just m  -> moduleNameString (moduleName m)
                                      Nothing -> "<no module>"
                       }
                 in go rest visited' localSet acc (uv : unres)
               Just unfoldingExpr ->
                 let newBind = NonRec v unfoldingExpr
                     newFVs = exprFreeVars unfoldingExpr
                     localSet' = extendVarSet localSet v
                     newExternals = filter (isResolvable localSet')
                                          (nonDetEltsUniqSet newFVs)
                 in go (newExternals ++ rest) visited' localSet' (newBind : acc) unres

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

    freeVarsOfBind :: CoreBind -> VarSet
    freeVarsOfBind (NonRec _ rhs) = exprFreeVars rhs
    freeVarsOfBind (Rec pairs) = foldMap (exprFreeVars . snd) pairs

    isPrimOp :: Id -> Bool
    isPrimOp v = case isPrimOpId_maybe v of
      Just _  -> True
      Nothing -> False

    isDataCon :: Id -> Bool
    isDataCon v = case isDataConWorkId_maybe v of
      Just _  -> True
      Nothing -> False

