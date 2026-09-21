-- | Exact dependency closure for closed programs. Session generation linking
-- remains a separate owner: this path never loads missing source-home bodies
-- from an interface left by an earlier edit.
module Tidepool.PreparedRecovery
  ( RecoveryFailure(..), RecoveredClosure(..), recoverPreparedClosure, newPreparedRecovery
  , insertGroup
  ) where

import Control.Exception (evaluate, throwIO)
import Control.Monad (foldM, unless, when)
import Data.IORef (modifyIORef', newIORef, readIORef)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Maybe (isJust)
import GHC.Types.Unique.Set (elementOfUniqSet, nonDetEltsUniqSet, sizeUniqSet)
import System.Environment (lookupEnv)
import GHC.Core (CoreBind, Bind(..))
import GHC.Driver.Env (HscEnv)
import GHC.Types.Id (idType)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Types.Var (Id, varName, varUnique)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (Module, unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExecutionProjection
  ( PreparedReachability(..), ProjectionContext(..), admitReachFacts
  , combinePreparedTargetReferences, emptyPreparedReachability
  , preparedModuleReachFacts, preparedModuleReferenceFacts, preparedSeedUniques
  , preparedTargetReferences, topBinders )
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.FatIface (FatIfaceCache, FatIfaceMissing, OwnerInterfaceCache)
import Tidepool.PreparedStg
  ( PreparedBodyCache, PreparedModule(..), RecoveredModuleFailure(..)
  , prepareRecoveredBodies )
import Tidepool.Resolve (ExactBodyLookup(..), recoverExactBody)
import Tidepool.PreparedBuiltins (deferredFunction, wiredInErrorKind)
import Tidepool.Timing (emitDetailPhase, emitCount, readTimingEnabled, timeSection)

data RecoveryFailure
  = MissingImplementation Name FatIfaceMissing
  | InterfaceLoadingFailure Module String
  | IncompatibleImplementation Name String
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
    IncompatibleImplementation name reason ->
      "incompatible implementation " ++ renderName name ++ ": " ++ reason
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

-- | Diagnostic split of 'prepared_recover' (flat sub-phases, summed over
-- rounds): per-module fact computation, the reachability walk, reference
-- collection, body lookup, and defining-module preparation, plus the round
-- and preparation counts.
data Spent = Spent
  { spentFacts :: !Integer
  , spentReach :: !Integer
  , spentRefs :: !Integer
  , spentLookup :: !Integer
  , spentPrepare :: !Integer
  , spentRounds :: !Integer
  , spentPreparations :: !Integer
  }

-- | Reprepare only defining modules whose exact body set grows. Attempted
-- names include typed failures, so unavailable bodies terminate the worklist
-- without a retry limit. New CorePrep references re-enter this same loop.
--
-- 'cache' (fat-interface Core, keyed by defining module), 'ownerCache'
-- (an owner's already-read-and-typechecked defining interface) and
-- 'bodyCache' (the prepared bodies themselves; see
-- 'Tidepool.PreparedStg.prepareRecoveredBodies') are all caller-owned so a
-- resident daemon can hoist them to daemon lifetime across requests, with
-- eviction at the request boundary for a request's own target module and any
-- @Tidepool.Session.*@ module (the scoped eviction callback in app/Main.hs); a one-shot invocation instead passes a
-- fresh cache created just for this call.
recoverPreparedClosure :: HscEnv -> FatIfaceCache -> OwnerInterfaceCache
  -> PreparedBodyCache -> ProjectionContext -> [PreparedModule]
  -> IO RecoveredClosure
recoverPreparedClosure env cache ownerCache bodyCache context home = do
  recover <- newPreparedRecovery env cache ownerCache bodyCache context home
  recover (projectionEntry context)

-- | One immutable home graph and authority context per multi-target request.
-- Only the entry changes between invocations; each target keeps its own exact
-- reachability and recovered body set. Reference facts for the unchanged home
-- bindings are shared, while replaced recovered modules invalidate their facts.
newPreparedRecovery :: HscEnv -> FatIfaceCache -> OwnerInterfaceCache
  -> PreparedBodyCache -> ProjectionContext -> [PreparedModule]
  -> IO (SymbolIdentity -> IO RecoveredClosure)
newPreparedRecovery env cache ownerCache bodyCache baseContext home = do
  timing <- readTimingEnabled
  checking <- isJust <$> lookupEnv "TIDEPOOL_RECOVERY_CHECK"
  let factsOf prepared =
        (preparedModuleReferenceFacts baseContext prepared, preparedModuleReachFacts baseContext prepared)
      homeFacts = [(prepared, factsOf prepared) | prepared <- home]
  pure $ \entry -> do
    let context = baseContext { projectionEntry = entry }
        seedList = nonDetEltsUniqSet (preparedSeedUniques context home)
    factsMemo <- newIORef Map.empty
    let factsFor prepared = do
          memo <- readIORef factsMemo
          case Map.lookup (pmModule prepared) memo of
            Just hit -> pure hit
            Nothing -> do
              let fresh = factsOf prepared
              modifyIORef' factsMemo (Map.insert (pmModule prepared) fresh)
              pure fresh
        roundReferences reach entries =
          let reached = reachedUniques reach
              kept binding =
                any ((`elementOfUniqSet` reached) . varUnique) (topBinders binding)
          in combinePreparedTargetReferences context (admittedTops reach) kept
               [(prepared, references) | (prepared, (references, _)) <- entries]
    spent <- newIORef (Spent 0 0 0 0 0 0 0)
    let homeOwners = Set.fromList (map pmModule home)
        charge f = modifyIORef' spent f
        go attempted groups prepared failures admitted previousReach = do
          let modules = home ++ Map.elems prepared
          -- Forced here rather than left to 'roundReferences': the per-module
          -- facts are the memoized half of this phase and the round-invariant
          -- one, so charging them separately is what says whether a round costs
          -- what it discovers or what it re-walks.
          (recovered, factsMs) <- timeSection $ do
            entries <- mapM (\m -> (,) m <$> factsFor m) (Map.elems prepared)
            mapM_ (\(_, (references, reach)) -> do
              _ <- evaluate (sum (map length (Map.elems references)))
              evaluate (sum (map (length . snd) reach))) entries
            pure entries
          let entries = homeFacts ++ recovered
          (reach, reachMs) <- timeSection $ do
            -- Only what this round admitted enters the walk: the closure and
            -- the dependency relation carry over from the previous round.
            let admittedFacts =
                  [ reach | (entry, (_, reach)) <- entries
                  , Set.member (pmModule entry) admitted ]
                extended = admitReachFacts seedList admittedFacts previousReach
            _ <- evaluate (sizeUniqSet (reachedUniques extended))
            _ <- evaluate (sizeUniqSet (admittedTops extended))
            pure extended
          (references, refsMs) <- timeSection $ do
            refs <- evaluate (roundReferences reach entries)
            _ <- evaluate (length refs)
            when checking $ do
              let expected = preparedTargetReferences context modules
              unless (map varUnique refs == map varUnique expected) $
                throwIO (userError ("recovery reachability diverged from identity selection: "
                  ++ show (length refs) ++ " vs " ++ show (length expected) ++ " references"))
            pure refs
          charge (\s -> s { spentFacts = spentFacts s + factsMs
                          , spentReach = spentReach s + reachMs
                          , spentRefs = spentRefs s + refsMs
                          , spentRounds = spentRounds s + 1 })
          let pending = filter (\binder -> not (Set.member (varName binder) attempted)
                  && typePrimRep_maybe (idType binder) /= Just [])
                references
          if null pending
            then do
              Spent factsTotal reachTotal refsTotal lookupTotal prepareTotal rounds
                preparedModules <- readIORef spent
              emitDetailPhase timing "prepared_recover" "prepared_recover_facts" factsTotal
              emitDetailPhase timing "prepared_recover" "prepared_recover_reach" reachTotal
              emitDetailPhase timing "prepared_recover" "prepared_recover_refs" refsTotal
              emitDetailPhase timing "prepared_recover" "prepared_recover_lookup" lookupTotal
              emitDetailPhase timing "prepared_recover" "prepared_recover_prepare" prepareTotal
              emitCount timing "prepared_recover_rounds" rounds
              emitCount timing "prepared_recover_module_preparations" preparedModules
              pure (RecoveredClosure modules failures)
            else do
              ((nextGroups, dirty, nextFailures), lookupMs) <- timeSection $ foldM
                (lookupOne cache homeOwners) (groups, Set.empty, failures) pending
              ((nextPrepared, finalFailures), prepareMs) <- timeSection $ foldM
                (prepareOne nextGroups) (prepared, nextFailures) (Set.toAscList dirty)
              charge (\s -> s { spentLookup = spentLookup s + lookupMs
                              , spentPrepare = spentPrepare s + prepareMs
                              , spentPreparations =
                                  spentPreparations s + fromIntegral (Set.size dirty) })
              go (Set.union attempted (Set.fromList (map varName pending)))
                nextGroups nextPrepared finalFailures dirty reach
        lookupOne _cacheRef homeOwnersRef (groups, dirty, failures) binder
          | Just _ <- wiredInErrorKind binder = pure (groups, dirty, failures)
          | Just _ <- deferredFunction binder = pure (groups, dirty, failures)
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
                BodyTypeMismatch _ name requested candidate fallback ->
                  let fallbackText = maybe "" ("; fallback: " ++) fallback
                      reason = "requested type " ++ requested
                        ++ "; candidate type " ++ candidate ++ fallbackText
                  in (groups, dirty
                    , failures ++ [IncompatibleImplementation name reason])
                UnsupportedBodyCapability name ->
                  (groups, dirty, failures ++ [UnsupportedExternalCapability name])
        prepareOne groups (prepared, failures) owner = do
          result <- prepareRecoveredBodies env ownerCache bodyCache owner
            (Map.findWithDefault [] owner groups)
          case result of
            Right modul -> do
              -- The module's pmBindings just changed; the memoized per-module
              -- reference facts for 'owner' (see 'factsFor' above) are stale.
              modifyIORef' factsMemo (Map.delete owner)
              pure
                ( Map.insert owner modul prepared
                , filter (not . preparationFailureFor owner) failures
                )
            Left failure -> pure (prepared, failures ++ [DefiningPreparationFailure failure])
    go Set.empty Map.empty Map.empty [] homeOwners emptyPreparedReachability

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
