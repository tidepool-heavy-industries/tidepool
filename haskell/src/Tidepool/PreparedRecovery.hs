-- | Exact dependency closure for closed programs. Session generation linking
-- remains a separate owner: this path never loads missing source-home bodies
-- from an interface left by an earlier edit.
module Tidepool.PreparedRecovery
  ( RecoveryFailure(..), RecoveredClosure(..), recoverPreparedClosure
  , insertGroup
  ) where

import Control.Monad (foldM)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC.Core (CoreBind, Bind(..))
import GHC.Driver.Env (HscEnv)
import GHC.Types.Id (idType)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Types.Var (Id, varName)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (Module, unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExecutionProjection
  (ProjectionContext, preparedTargetReferences)
import Tidepool.FatIface (FatIfaceMissing, newFatIfaceCache)
import Tidepool.PreparedStg
  (PreparedModule(..), RecoveredModuleFailure(..), prepareRecoveredBodies)
import Tidepool.Resolve (ExactBodyLookup(..), recoverExactBody)

data RecoveryFailure
  = MissingImplementation Name FatIfaceMissing
  | InterfaceLoadingFailure Module String
  | UnsupportedExternalCapability Name
  | MissingHomeImplementation Name
  | DefiningPreparationFailure RecoveredModuleFailure
  deriving (Eq)

instance Show RecoveryFailure where
  show failure = case failure of
    MissingImplementation name reason ->
      "missing implementation " ++ renderName name ++ ": " ++ show reason
    InterfaceLoadingFailure owner reason ->
      "interface loading failure " ++ renderModule owner ++ ": " ++ reason
    UnsupportedExternalCapability name ->
      "unsupported external capability " ++ renderName name
    MissingHomeImplementation name ->
      "missing home implementation " ++ renderName name
    DefiningPreparationFailure reason ->
      "defining preparation failure: " ++ show reason
    where
      renderModule owner = unitString (moduleUnit owner) ++ ":"
        ++ moduleNameString (moduleName owner)
      renderName name = case nameModule_maybe name of
        Just owner -> renderModule owner ++ ":" ++ occNameString (nameOccName name)
        Nothing -> showSDocUnsafe (ppr name)

-- | Residuals are evidence, not an empty-map fallback. Projection retains
-- their global declarations, allowing corpus admission to report what remains.
data RecoveredClosure = RecoveredClosure
  { closureModules :: [PreparedModule]
  , closureFailures :: [RecoveryFailure]
  }

-- | Reprepare only defining modules whose exact body set grows. Attempted
-- names include typed failures, so unavailable bodies terminate the worklist
-- without a retry limit. New CorePrep references re-enter this same loop.
recoverPreparedClosure :: HscEnv -> ProjectionContext -> [PreparedModule]
  -> IO RecoveredClosure
recoverPreparedClosure env context home = do
  cache <- newFatIfaceCache
  let homeOwners = Set.fromList (map pmModule home)
      go attempted groups prepared failures = do
        let modules = home ++ Map.elems prepared
            pending = filter (\binder -> not (Set.member (varName binder) attempted)
                && typePrimRep_maybe (idType binder) /= Just [])
              (preparedTargetReferences context modules)
        if null pending
          then pure (RecoveredClosure modules failures)
          else do
            (nextGroups, dirty, nextFailures) <- foldM
              (lookupOne cache homeOwners) (groups, Set.empty, failures) pending
            (nextPrepared, finalFailures) <- foldM
              (prepareOne nextGroups) (prepared, nextFailures) (Set.toAscList dirty)
            go (Set.union attempted (Set.fromList (map varName pending)))
              nextGroups nextPrepared finalFailures
      lookupOne _cacheRef homeOwnersRef (groups, dirty, failures) binder
        | maybe False (`Set.member` homeOwnersRef) (nameModule_maybe (varName binder)) =
            pure (groups, dirty, failures ++ [MissingHomeImplementation (varName binder)])
        | otherwise = do
            found <- recoverExactBody env cache binder
            pure $ case found of
              ExactBody owner group _ ->
                (Map.alter (Just . insertGroup group . maybe [] id) owner groups,
                 Set.insert owner dirty, failures)
              MissingExactBody name reason ->
                (groups, dirty, failures ++ [MissingImplementation name reason])
              BodyInterfaceFailure owner reason ->
                (groups, dirty, failures ++ [InterfaceLoadingFailure owner reason])
              UnsupportedBodyCapability name ->
                (groups, dirty, failures ++ [UnsupportedExternalCapability name])
      prepareOne groups (prepared, failures) owner = do
        result <- prepareRecoveredBodies env owner (Map.findWithDefault [] owner groups)
        pure $ case result of
          Right modul ->
            ( Map.insert owner modul prepared
            , filter (not . preparationFailureFor owner) failures
            )
          Left failure -> (prepared, failures ++ [DefiningPreparationFailure failure])
  go Set.empty Map.empty Map.empty []

preparationFailureFor :: Module -> RecoveryFailure -> Bool
preparationFailureFor owner (DefiningPreparationFailure failure) =
  recoveredFailureOwner failure == owner
preparationFailureFor _ _ = False

recoveredFailureOwner :: RecoveredModuleFailure -> Module
recoveredFailureOwner failure = case failure of
  RecoveredModuleFinderFailure owner _ -> owner
  RecoveredModuleInterfaceFailure owner _ -> owner
  RecoveredModulePreparationFailure owner _ -> owner

-- Full recursive groups supersede overlapping singleton unfoldings. When a
-- newly discovered group bridges two previously disjoint groups, retain every
-- member from all of them; dropping the non-overlapping siblings would leave
-- references in the merged group unbound.
insertGroup :: CoreBind -> [CoreBind] -> [CoreBind]
insertGroup incoming previous =
  let names = Set.fromList (map varName (binders incoming))
      overlaps group = any ((`Set.member` names) . varName) (binders group)
      existing = filter overlaps previous
      existingNames group = Set.fromList (map varName (binders group))
      merged = Rec (mergePairs (concatMap pairsOf existing) (pairsOf incoming))
  in case existing of
       [] -> previous ++ [incoming]
       -- A full fat-interface Rec group is authoritative for the complete
       -- sibling set.  A later singleton unfolding from the same pinned
       -- environment must not discard those siblings.  Only a partial
       -- overlap needs a merge; in that case incoming pairs deterministically
       -- replace duplicate binders while retaining every sibling.
       [group] | names `Set.isSubsetOf` existingNames group -> previous
       _ -> filter (not . overlaps) previous ++ [merged]
  where
    pairsOf (NonRec binder body) = [(binder, body)]
    pairsOf (Rec pairs) = pairs

    -- When a partial overlap bridges groups, prefer the incoming body for a
    -- duplicate binder while retaining all siblings from every group.
    mergePairs existingPairs incomingPairs =
      foldl replace existingPairs incomingPairs

    replace pairs incomingPair@(binder, _) =
      filter ((/= varName binder) . varName . fst) pairs ++ [incomingPair]

binders :: CoreBind -> [Id]
binders (NonRec binder _) = [binder]
binders (Rec pairs) = map fst pairs
