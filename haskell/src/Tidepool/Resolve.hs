module Tidepool.Resolve
  ( ExactBodyLookup(..), BodyOrigin(..), recoverExactBody
  ) where

import GHC.Core (CoreBind, CoreExpr, Bind(..), maybeUnfoldingTemplate)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.Rep (Type)
import GHC.Core.Utils (exprType)
import GHC.Types.Id (Id, idType, realIdUnfolding, isPrimOpId_maybe, isDataConWorkId_maybe)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Types.Var (varName)
import GHC.Types.Var.Set (elemVarSet)
import GHC.Types.Name (Name, nameModule_maybe)
import GHC.Unit.Types (Module)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Data.List (find)
import Data.Maybe (isJust)
import GHC.Driver.Env (HscEnv)

-- Fat interface fallback (mi_extra_decls) — for loop-breakers whose
-- unfoldings are not exposed via realIdUnfolding even with threshold bumps.
import Tidepool.FatIface
  ( FatIfaceCache, FatIfaceLookup(..), FatIfaceMissing(..), lookupFatIfaceExact )

data BodyOrigin = InterfaceUnfolding | FatInterfaceGroup deriving (Eq, Show)

-- | Prepared recovery is exact: a failed lookup never selects an alias or
-- synthesizes a dictionary. Found groups belong to the returned module and
-- must be prepared there, not appended to the caller's Core bindings.
data ExactBodyLookup
  = ExactBody Module CoreBind BodyOrigin
  | MissingExactBody Name FatIfaceMissing
  | BodyInterfaceFailure Module String
  | BodyTypeMismatch
      { mismatchOwner :: Module
      , mismatchName :: Name
      , mismatchRequestedType :: String
      , mismatchCandidateType :: String
      , mismatchFallback :: Maybe String
      }
  | UnsupportedBodyCapability Name

recoverExactBody :: HscEnv -> FatIfaceCache -> Id -> IO ExactBodyLookup
recoverExactBody env cache binder
  | unsupportedBodyCapability binder = pure (UnsupportedBodyCapability name)
  | otherwise = case nameModule_maybe name of
      Nothing -> pure (MissingExactBody name NameWithoutModule)
      Just owner -> case maybeUnfoldingTemplate (realIdUnfolding binder) of
        Just body ->
          let group = if binder `elemVarSet` exprSomeFreeVars (const True) body
                then Rec [(binder, body)] else NonRec binder body
          in case interfaceBodyMismatch binder body of
               Nothing -> pure (ExactBody owner group InterfaceUnfolding)
               Just mismatch -> lookupFatCandidate owner (Just mismatch)
        Nothing -> lookupFatCandidate owner Nothing
  where
    name = varName binder

    lookupFatCandidate owner previousMismatch = do
      result <- lookupFatIfaceExact env cache name
      pure $ case result of
        FatIfaceFound group -> case fatGroupMismatch binder group of
          Nothing -> ExactBody owner group FatInterfaceGroup
          Just mismatch -> BodyTypeMismatch owner name
            (renderType (idType binder))
            (candidateType mismatch)
            (Just (fallbackDetail previousMismatch mismatch))
        FatIfaceMissing reason -> case previousMismatch of
          Nothing -> MissingExactBody name reason
          Just mismatch -> BodyTypeMismatch owner name
            (renderType (idType binder))
            (candidateType mismatch)
            (Just ("fat-interface lookup: " ++ show reason))
        FatIfaceLoadFailure modul reason -> case previousMismatch of
          Nothing -> BodyInterfaceFailure modul reason
          Just mismatch -> BodyTypeMismatch owner name
            (renderType (idType binder))
            (candidateType mismatch)
            (Just ("fat-interface load failure for "
              ++ renderModule modul ++ ": " ++ reason))

    fallbackDetail Nothing fatMismatch = fatDetail fatMismatch
    fallbackDetail (Just realMismatch) fatMismatch =
      "real unfolding rejected: " ++ mismatchDetail realMismatch
        ++ "; fat-interface candidate rejected: " ++ fatDetail fatMismatch

    fatDetail = mismatchDetail

data CandidateMismatch = CandidateMismatch
  { candidateType :: String
  , mismatchDetail :: String
  }

interfaceBodyMismatch :: Id -> CoreExpr -> Maybe CandidateMismatch
interfaceBodyMismatch binder body
  | eqType (idType binder) (exprType body) = Nothing
  | otherwise = Just CandidateMismatch
      { candidateType = renderType (exprType body)
      , mismatchDetail = "real unfolding RHS type " ++ renderType (exprType body)
          ++ " does not match requested binder type " ++ renderType (idType binder)
      }

fatGroupMismatch :: Id -> CoreBind -> Maybe CandidateMismatch
fatGroupMismatch requested group =
  case find ((== varName requested) . varName . fst) (bindPairs group) of
    Nothing -> Just CandidateMismatch
      { candidateType = "<missing selected binder>"
      , mismatchDetail = "fat-interface group does not contain the requested binder"
      }
    Just (selected, _) | not (eqType (idType requested) (idType selected)) ->
      Just CandidateMismatch
        { candidateType = renderType (idType selected)
        , mismatchDetail = "fat-interface selected binder type "
            ++ renderType (idType selected)
            ++ " does not match requested binder type "
            ++ renderType (idType requested)
        }
    _ -> firstMismatch (bindPairs group)
  where
    firstMismatch [] = Nothing
    firstMismatch ((binder, body) : rest)
      | eqType (idType binder) (exprType body) = firstMismatch rest
      | otherwise = Just CandidateMismatch
          { candidateType = renderType (exprType body)
          , mismatchDetail = "fat-interface RHS type "
              ++ renderType (exprType body) ++ " does not match binder "
              ++ renderType (idType binder)
          }

bindPairs :: CoreBind -> [(Id, CoreExpr)]
bindPairs (NonRec binder body) = [(binder, body)]
bindPairs (Rec pairs) = pairs

renderType :: Type -> String
renderType = showSDocUnsafe . ppr

renderModule :: Module -> String
renderModule = showSDocUnsafe . ppr

-- | Wired-in operations and zero-width values have no recoverable interface
-- body.  Keep them out of the exact-body worklist rather than asking the fat
-- interface loader for a definition that cannot exist.
unsupportedBodyCapability :: Id -> Bool
unsupportedBodyCapability binder =
  isJust (isPrimOpId_maybe binder)
  || isJust (isDataConWorkId_maybe binder)
  || case typePrimRep_maybe (idType binder) of
       Just [] -> True
       _ -> False
