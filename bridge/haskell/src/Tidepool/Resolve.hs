module Tidepool.Resolve
  ( ExactBodyLookup(..), recoverExactBody
  ) where

import GHC.Core (CoreBind, CoreExpr, Bind(..))
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.Rep (Type)
import GHC.Core.Utils (exprType)
import GHC.Types.Id (Id, idType, isPrimOpId_maybe, isDataConWorkId_maybe)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Types.Var (varName)
import GHC.Types.Name (Name, nameModule_maybe)
import GHC.Unit.Types (Module)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Data.List (find)
import Data.Maybe (isJust)
import GHC.Driver.Env (HscEnv)

-- The defining interface's original Core owns executable bodies. Optimizer
-- unfoldings can reconstruct different allocations and entry contracts.
import Tidepool.FatIface
  ( FatIfaceCache, FatIfaceLookup(..), FatIfaceMissing(..), lookupFatIfaceExact )

-- | Prepared recovery is exact: a failed lookup never selects an alias or
-- synthesizes a dictionary. Found groups belong to the returned module and
-- must be prepared there, not appended to the caller's Core bindings.
data ExactBodyLookup
  = ExactBody Module CoreBind
  | MissingExactBody Name FatIfaceMissing
  | BodyInterfaceFailure Module String
  | BodyTypeMismatch
      { mismatchOwner :: Module
      , mismatchName :: Name
      , mismatchRequestedType :: String
      , mismatchCandidateType :: String
      , mismatchReason :: String
      }
  | UnsupportedBodyCapability Name

recoverExactBody :: HscEnv -> FatIfaceCache -> Id -> IO ExactBodyLookup
recoverExactBody env cache binder
  | unsupportedBodyCapability binder = pure (UnsupportedBodyCapability name)
  | otherwise = case nameModule_maybe name of
      Nothing -> pure (MissingExactBody name NameWithoutModule)
      Just owner -> lookupOriginalGroup owner
  where
    name = varName binder

    lookupOriginalGroup owner = do
      result <- lookupFatIfaceExact env cache name
      pure $ case result of
        FatIfaceFound group -> case fatGroupMismatch binder group of
          Nothing -> ExactBody owner group
          Just mismatch -> BodyTypeMismatch owner name
            (renderType (idType binder))
            (candidateType mismatch)
            (mismatchDetail mismatch)
        FatIfaceMissing reason -> MissingExactBody name reason
        FatIfaceLoadFailure modul reason -> BodyInterfaceFailure modul reason

data CandidateMismatch = CandidateMismatch
  { candidateType :: String
  , mismatchDetail :: String
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
