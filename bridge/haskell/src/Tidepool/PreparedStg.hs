-- | The request-local GHC handoff used to inspect the prepared program before
-- Tidepool commits to a cross-language execution schema.
--
-- This module deliberately retains GHC's stage-specific types.  It is an
-- internal compiler adapter, not the future serialized program model.
module Tidepool.PreparedStg
  ( PreparedModule, PreparedCoverage(..)
  , pmModule, pmCoverage, pmBindings, pmOriginalTopNames, pmStableTopSpellings, pmTagSigs, pmSitedSiblings, pmYieldSites, pmPreparedSites, pmTypeGraph, pmSiteRejections, pmRequestSiteTyCon
  , preparedBindingGroups, filterPreparedBindings, preparedRejectsIntrinsic, preparedUsesSiteAuthority, preparedExpectedEntry
  , prepareModule, PreparedModuleTask, acquirePreparedModule, runPreparedModuleTask
  , PreparedSiteEnvironment, resolvePreparedSiteEnvironment, preparedSiteDependenciesMatch, preparedSiteDependenciesEquivalent
  , acquirePreparedModuleWithSiteEnvironment, prepareModuleWithSiteEnvironment
  , RecoveredModuleInput(..)
  , RecoveredModuleFailure(..)
  , prepareRecoveredModule
  , prepareRecoveredBodies
  , PreparedBodyCache, newPreparedBodyCache, copyPreparedBodyCache, mergePreparedBodyCaches, selectPreparedBodyCaches, evictPreparedBodyMatching
  , PreparedBodyReuse(..), newPreparedOriginalModuleTaskPreparer
  , newPreparedBodyPreparer, newPreparedBodyTaskPreparer, PreparedBodyTask, runPreparedBodyTask
  , newPreparedComponentTaskPreparer
  ) where

import Control.Exception
  ( SomeAsyncException, SomeException, displayException, fromException
  , throwIO, try, evaluate )
import Control.Monad (unless, forM)
import Control.Concurrent.MVar (MVar, modifyMVar, modifyMVar_, newMVar, readMVar)
import Data.List (foldl', partition, sortOn)
import Data.Maybe (fromMaybe, listToMaybe, isJust)
import Data.IntMap.Strict qualified as IntMap
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text (Text)
import Data.Text qualified as Text
import Data.Word (Word32, Word64)
import Data.Unique (Unique)
import System.Environment (lookupEnv)
import GHC.Core.Lint (displayLintResults)
import GHC.Core (CoreBind, Bind(..), bindersOfBinds)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Core.Lint.Interactive (interactiveInScope)
import GHC.Core.Opt.Pipeline.Types (CoreToDo(CorePrep))
import GHC.Core.Opt.Arity (etaExpand)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Types.Id
  ( idArity, idType, idDmdSig, idCprSig, isDataConWorkId_maybe
  , setIdArity, setIdDmdSig, setIdCprSig )
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Core.TyCon (TyCon, isDataTyCon)
import GHC.CoreToStg (coreToStg)
import GHC.CoreToStg.Prep (corePrepPgm)
import GHC.Driver.Config.Core.Lint (lintCoreBindings)
import GHC.Driver.Config.CoreToStg (initCoreToStgOpts)
import GHC.Driver.Config.CoreToStg.Prep
  (initCorePrepConfig, initCorePrepPgmConfig)
import GHC.Driver.Config.Stg.Pipeline (initStgPipelineOpts)
import GHC.Driver.Env (HscEnv(..))
import GHC.Driver.Session
  (GeneralFlag(..), gopt_set, gopt_unset)
import GHC.IfaceToCore (typecheckIface)
import GHC.Stg.Pipeline (StgCgInfos, stg2stgWithExternalScope)
import GHC.Stg.Syntax (CgStgTopBinding, GenStgTopBinding(..), GenStgBinding(..))
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.Var.Set (IdSet, elemVarSet, mkVarSet, unionVarSets)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Unique (getKey)
import GHC.Types.Var (Id, isId, varName, varUnique)
import GHC.Types.Name (Name, isExternalName, nameModule_maybe, wiredInNameTyThing_maybe)
import GHC.Types.TyThing (TyThing(..))
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Env (plusNameEnv, emptyNameEnv, disjointNameEnv)
import GHC.Unit.Types (Module)
import GHC.Unit.Module.Location (ModLocation)
import GHC.Unit.Module.ModIface (ModIface, mi_iface_hash, mi_final_exts)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Types.TypeEnv (typeEnvTyCons, typeEnvIds)
import GHC.Utils.Outputable (ppr, showSDocUnsafe, text)
import Tidepool.EffectSchema (YieldSite)
import Tidepool.PreparedSites
  ( PreparedSite, SiteRejection, IntrinsicCensus
  , censusPreparedIntrinsics, intrinsicFree, intrinsicNames
  , resolvePreparedSiblings, PreparedSiteEnvironment, PreparedSiteDependencies
  , resolvePreparedSiteEnvironment, elaboratePreparedSitesWithDependencies )
import Tidepool.PreparedSites qualified as Sites
import Tidepool.PreparedStg.Internal
import Tidepool.FinalizedModule (FinalizedModule, finalizedTidyGuts)
import Tidepool.ExactScope (ExactScope)
import Tidepool.HomeProducts
  ( OriginalVersion, originalVersionOwner, originalVersionInRecoveryScope, AdmittedFinalizedOriginal
  , admittedOriginalModule, admittedOriginalLocation, admitOriginalRecoveryScope
  , recoverAdmittedFinalizedOriginalWithPrevious )
import Tidepool.Timing (readTimingEnabled, timePhase, timeModuleDetailPhase, emitCount)
import Tidepool.TypePolicy (TypeGraph, emptyTypeGraph)
import Tidepool.FatIface
  ( ExactInterfaceFailure(..), readExactInterface
  , FatOriginalVersion, fatOriginalOwner, FatIfaceComponent, fatComponentVersion
  , fatComponentOrdinals, fatComponentOrdinal, fatComponentBindings, fatComponentAllBinders
  , FatIfaceSelection, fatSelectionVersion, fatSelectionComponents
  , fatSelectionDemandedGroupCount, fatSelectionPreparedGroupCount
  , OwnerInterfaceContext, ownerInterfaceLocation, ownerInterfaceTyCons, ownerInterfaceEntries
  , sameOwnerInterfaceContext, ownerInterfaceMatchesOriginal, OwnerInterfaceCache, lookupOwnerInterface, cacheOwnerInterface )
import Tidepool.FatIface.Internal qualified as Shared

-- | Observe prepared output without replacing its compiler-owned evidence.
pmModule :: PreparedModule -> Module
pmModule = preparedModule

pmCoverage :: PreparedModule -> PreparedCoverage
pmCoverage = preparedCoverage

pmBindings :: PreparedModule -> [(CgStgTopBinding, IdSet)]
pmBindings = preparedBindings

pmStableTopSpellings :: PreparedModule -> Map Name Text
pmStableTopSpellings = preparedStableTopSpellings

pmTagSigs :: PreparedModule -> StgCgInfos
pmTagSigs = preparedTagSigs

pmSitedSiblings :: PreparedModule -> Map String Id
pmSitedSiblings = preparedSitedSiblings

pmYieldSites :: PreparedModule -> [YieldSite]
pmYieldSites = preparedYieldSites

pmPreparedSites :: PreparedModule -> [PreparedSite]
pmPreparedSites = preparedPreparedSites

pmTypeGraph :: PreparedModule -> TypeGraph
pmTypeGraph = preparedTypeGraph

pmSiteRejections :: PreparedModule -> [SiteRejection]
pmSiteRejections = preparedSiteRejections

pmRequestSiteTyCon :: PreparedModule -> Maybe TyCon
pmRequestSiteTyCon = preparedRequestSiteTyCon

-- | Projection can select existing groups; it cannot introduce bodies or sites.
filterPreparedBindings :: ((CgStgTopBinding, IdSet) -> Bool) -> PreparedModule -> PreparedModule
filterPreparedBindings keep prepared = prepared
  { preparedBindings = filter keep (preparedBindings prepared) }

pmOriginalTopNames :: PreparedModule -> Set.Set Name
pmOriginalTopNames = preparedOriginalTopNames

-- | Original-order views of this owner's existing groups. Each view retains
-- every module, tag, sibling and typed-site fact; no caller supplies a body.
-- Identity assignment must still use the complete module before selection.
preparedBindingGroups :: PreparedModule -> [(Word32, PreparedModule)]
preparedBindingGroups prepared =
  [ (fromIntegral ordinal, prepared { preparedBindings = [item] })
  | (ordinal, item) <- zip [0 :: Int ..] (preparedBindings prepared) ]

-- | Intrinsics use original GHC Names, independently of their diagnostic text.
preparedRejectsIntrinsic :: PreparedModule -> Id -> Bool
preparedRejectsIntrinsic prepared identifier =
  varName identifier `Set.member` preparedIntrinsicNames prepared

-- | A package definition's expected entry comes from its exact declaring
-- interface. Provisional subsets may acquire stricter STG shapes as recovery
-- admits missing siblings; only final selected emission checks this contract.
preparedExpectedEntry :: PreparedModule -> Id -> Maybe Id
preparedExpectedEntry prepared identifier =
  Map.lookup (varName identifier) $ case preparedExpectedEntries prepared of
    ProvisionalEntries entries -> entries
    DeclaringEntries context -> ownerInterfaceEntries context

-- | The typed census, not an empty site list, determines authority dependence.
preparedUsesSiteAuthority :: PreparedModule -> Bool
preparedUsesSiteAuthority = preparedAuthorityDependent

-- | All prepared-body caches use the same evidence check. No proof, or a
-- failed authority lookup, cannot authorize a dependent prepared-product hit.
preparedSiteDependenciesMatch :: PreparedSiteEnvironment -> Map String Id -> PreparedModule -> Bool
preparedSiteDependenciesMatch environment siblings prepared =
  not (preparedAuthorityDependent prepared)
    || maybe False (Sites.preparedSiteDependenciesMatch environment siblings)
         (preparedSiteDependencies prepared)

-- | Alternatives retain the same completed Core independently of the authority
-- observations used during lowering. Only verifiable consumed facts deduplicate.
preparedSiteDependenciesEquivalent :: PreparedModule -> PreparedModule -> Bool
preparedSiteDependenciesEquivalent first second =
  case (preparedAuthorityDependent first, preparedAuthorityDependent second) of
    (False, False) -> True
    (True, True) -> case (preparedSiteDependencies first, preparedSiteDependencies second) of
      (Just facts, Just facts') -> Sites.preparedSiteDependenciesEquivalent facts facts'
      _ -> False
    _ -> False

-- | Complete fresh and admitted retained originals share this preparation owner.
prepareModule :: HscEnv -> ModLocation -> Map String Id -> FinalizedModule -> IO PreparedModule
prepareModule env location siblings finalized =
  acquirePreparedModule env location siblings finalized >>= runPreparedModuleTask

prepareModuleWithSiteEnvironment :: PreparedSiteEnvironment -> HscEnv -> ModLocation
  -> Map String Id -> FinalizedModule -> IO PreparedModule
prepareModuleWithSiteEnvironment environment env location siblings finalized =
  acquirePreparedModuleWithSiteEnvironment environment env location siblings finalized >>= runPreparedModuleTask

-- | Acquired under the selected compiler context. The task retains typed
-- lowering inputs; running it does not consult or replace the live Session.
newtype PreparedModuleTask = PreparedModuleTask (IO PreparedModule)

runPreparedModuleTask :: PreparedModuleTask -> IO PreparedModule
runPreparedModuleTask (PreparedModuleTask action) = action

acquirePreparedModule :: HscEnv -> ModLocation -> Map String Id -> FinalizedModule
  -> IO PreparedModuleTask
acquirePreparedModule env location siblings finalized =
  let guts = finalizedTidyGuts finalized
  in acquireTypedBindings CompleteSourceModule env (cg_module guts) location
       (cg_tycons guts) siblings (cg_binds guts)

acquirePreparedModuleWithSiteEnvironment :: PreparedSiteEnvironment -> HscEnv -> ModLocation
  -> Map String Id -> FinalizedModule -> IO PreparedModuleTask
acquirePreparedModuleWithSiteEnvironment environment env location siblings finalized =
  let guts = finalizedTidyGuts finalized
  in acquireTypedBindingsWithSiteEnvironment (Just environment) CompleteSourceModule env
       (cg_module guts) location (cg_tycons guts) siblings (cg_binds guts)

-- | Exact optimized bindings retain their defining module and interface
-- context. No fabricated ModSummary or cross-module Core grouping is needed:
-- source and recovered inputs use the same preparation owner below.
data RecoveredModuleInput = RecoveredModuleInput
  { recoveredModule :: Module
  , recoveredLocation :: ModLocation
  , recoveredTyCons :: [TyCon]
  , recoveredBindings :: [CoreBind]
  , recoveredEntries :: Map Name Id
  }

-- | Failures while acquiring the defining-module context for an exact group.
-- A failed interface load is distinct from a missing binding: callers may
-- report or retry the former, but must never silently drop the group.
data RecoveredModuleFailure
  = RecoveredModuleFinderFailure Module String
  | RecoveredModuleInterfaceFailure Module String
  | RecoveredModulePreparationFailure Module String
  deriving (Eq)

instance Show RecoveredModuleFailure where
  show failure = case failure of
    RecoveredModuleFinderFailure owner reason ->
      "finder failure for " ++ renderModule owner ++ ": " ++ reason
    RecoveredModuleInterfaceFailure owner reason ->
      "interface failure for " ++ renderModule owner ++ ": " ++ reason
    RecoveredModulePreparationFailure owner reason ->
      "preparation failure for " ++ renderModule owner ++ ": " ++ reason
    where
      renderModule = showSDocUnsafe . ppr

prepareRecoveredModule :: HscEnv -> RecoveredModuleInput -> IO PreparedModule
prepareRecoveredModule hscEnv input =
  acquireRecoveredModule hscEnv input >>= runPreparedModuleTask

acquireRecoveredModule :: HscEnv -> RecoveredModuleInput -> IO PreparedModuleTask
acquireRecoveredModule = acquireRecoveredModuleUsingSiteEnvironment Nothing

acquireRecoveredModuleUsingSiteEnvironment :: Maybe PreparedSiteEnvironment
  -> HscEnv -> RecoveredModuleInput -> IO PreparedModuleTask
acquireRecoveredModuleUsingSiteEnvironment = acquireRecoveredModuleWithWorkers IncludeConstructorWorkers

acquireRecoveredModuleWithWorkers :: ConstructorWorkerPolicy -> Maybe PreparedSiteEnvironment
  -> HscEnv -> RecoveredModuleInput -> IO PreparedModuleTask
acquireRecoveredModuleWithWorkers workers environment hscEnv input = do
  let entries = recoveredEntries input
  bindings <- mapM (restoreRecoveredEntries entries) (recoveredBindings input)
  task <- acquireTypedBindingsWithEntryScope entries workers environment ExactBodySubset
    hscEnv (recoveredModule input) (recoveredLocation input)
    (recoveredTyCons input) Map.empty bindings
  pure $ PreparedModuleTask $ do
    prepared <- runPreparedModuleTask task
    pure prepared { preparedExpectedEntries = ProvisionalEntries entries }

-- Fat Core's local IdInfo is not the executable interface contract. Restore
-- only entry-relevant fields; occurrence analyses and unfoldings still belong
-- to the recovered body. Source arity counts Core arguments, not LF registers.
restoreRecoveredEntries :: Map Name Id -> CoreBind -> IO CoreBind
restoreRecoveredEntries entries binding = case binding of
  NonRec binder body -> uncurry NonRec <$> restore (binder, body)
  Rec pairs -> Rec <$> mapM restore pairs
  where
    restore pair@(binder, body) = case Map.lookup (varName binder) entries of
      Nothing -> pure pair
      Just original -> do
        unless (eqType (idType binder) (idType original)) $
          ioError (userError ("recovered defining entry type mismatch: " ++ showSDocUnsafe (ppr binder)))
        let arity = case importedIdLFInfo original of
              LFThunk{} -> 0
              _ -> idArity original
            metadata = setIdCprSig (setIdDmdSig (setIdArity binder arity)
              (idDmdSig original)) (idCprSig original)
        pure (metadata, etaExpand arity body)

-- Both complete modules and recovered subsets elaborate before CorePrep erases types.
acquireTypedBindings :: PreparedCoverage -> HscEnv -> Module -> ModLocation
  -> [TyCon] -> Map String Id -> [CoreBind] -> IO PreparedModuleTask
acquireTypedBindings coverage env owner location tycons imported bindings = do
  acquireTypedBindingsWithSiteEnvironment Nothing coverage env owner location tycons imported bindings

acquireTypedBindingsWithSiteEnvironment :: Maybe PreparedSiteEnvironment -> PreparedCoverage
  -> HscEnv -> Module -> ModLocation -> [TyCon] -> Map String Id -> [CoreBind] -> IO PreparedModuleTask
acquireTypedBindingsWithSiteEnvironment = acquireTypedBindingsWithWorkers IncludeConstructorWorkers

-- Canonical components own original groups; the defining constructor workers
-- are prepared once as a separate unit, rather than injected in every subset.
data ConstructorWorkerPolicy = IncludeConstructorWorkers | OmitConstructorWorkers
  deriving (Eq, Ord)

data FatPreparationUnit = OriginalComponent Int | ConstructorWorkers
  deriving (Eq, Ord)

acquireTypedBindingsWithWorkers :: ConstructorWorkerPolicy -> Maybe PreparedSiteEnvironment -> PreparedCoverage
  -> HscEnv -> Module -> ModLocation -> [TyCon] -> Map String Id -> [CoreBind] -> IO PreparedModuleTask
acquireTypedBindingsWithWorkers = acquireTypedBindingsWithEntryScope Map.empty

acquireTypedBindingsWithEntryScope :: Map Name Id -> ConstructorWorkerPolicy
  -> Maybe PreparedSiteEnvironment -> PreparedCoverage -> HscEnv -> Module -> ModLocation
  -> [TyCon] -> Map String Id -> [CoreBind] -> IO PreparedModuleTask
acquireTypedBindingsWithEntryScope entries workers selected coverage env owner location tycons imported bindings = do
  timing <- readTimingEnabled
  let census = censusPreparedIntrinsics tycons bindings
      ownedSiblings = resolvePreparedSiblings bindings
  (rewritten, sites, preparedSites, graph, rejections, carrier, dependencies) <-
    if intrinsicFree census then pure (bindings, [], [], emptyTypeGraph, [], Nothing, Nothing)
    else do
      environment <- maybe (timePhase timing "prepared_site_authority" (resolvePreparedSiteEnvironment env))
        pure selected
      (bodies, yields, issued, types, failures, evidence) <- timePhase timing "prepared_sites"
        (elaboratePreparedSitesWithDependencies environment ownedSiblings imported bindings)
      pure (bodies, yields, issued, types, failures, Sites.preparedSiteRequestAuthority environment, Just evidence)
  let subset = case coverage of
        CompleteSourceModule -> []
        ExactBodySubset -> recoveredSubsetScope owner rewritten
  acquireBindingsWithScope workers timing subset entries env owner location tycons
    rewritten coverage ownedSiblings sites preparedSites graph rejections carrier dependencies census

-- | An exact subset can reference other external tops in its defining module.
-- Admit only those free Ids to GHC's preparation scope; they remain dependency
-- edges for recovery, not supplied definitions. Complete source modules never
-- use this scope extension, so missing source definitions still fail lint.
recoveredSubsetScope :: Module -> [CoreBind] -> [Id]
recoveredSubsetScope owner bindings =
  filter (not . (`elemVarSet` supplied))
    (nonDetEltsUniqSet (unionVarSets
      [exprSomeFreeVars belongsToOwner rhs | binding <- bindings, rhs <- bodies binding]))
  where
    supplied = mkVarSet (bindersOfBinds bindings)
    belongsToOwner identifier = isId identifier
      && isExternalName (varName identifier)
      && nameModule_maybe (varName identifier) == Just owner
    bodies (NonRec _ rhs) = [rhs]
    bodies (Rec pairs) = map snd pairs

-- | Exact versions and consumed site facts authorize completed-body reuse.
-- Canonical pure units are coalesced separately from typed site batches.
data PreparedBodyCache = PreparedBodyCache
  { cachedExactBodies :: MVar (Map Module (Map PreparedBodyKey [PreparedModule]))
  , cachedOriginalModules :: MVar (Map Module (Map OriginalVersion [PreparedOriginalAlternative]))
  , cachedFatComponents :: MVar (Map Module (Shared.LoadCache (FatOriginalVersion, Unique, FatPreparationUnit) PreparedModule))
  }

-- Defining-context wrappers retain their exact binder census. Canonical fat
-- selections additionally seal the original body version, so identical Names
-- and consumed site facts cannot admit another version's prepared batch.
data PreparedBodyKey
  = ContextBodyKey ConstructorWorkerPolicy [[Word64]]
  | FatBodyKey ConstructorWorkerPolicy FatOriginalVersion [[Word64]]
  deriving (Eq, Ord)

newPreparedBodyCache :: IO PreparedBodyCache
newPreparedBodyCache = PreparedBodyCache <$> newMVar Map.empty <*> newMVar Map.empty <*> newMVar Map.empty

-- | Attempt additions are private until their compiler context is promoted.
copyPreparedBodyCache :: PreparedBodyCache -> IO PreparedBodyCache
copyPreparedBodyCache cache = PreparedBodyCache
  <$> (readMVar (cachedExactBodies cache) >>= newMVar)
  <*> (readMVar (cachedOriginalModules cache) >>= newMVar)
  <*> (readMVar (cachedFatComponents cache) >>= mapM Shared.copyLoadCache >>= newMVar)

-- | Select completed acceleration for the validated owner roster. Both exact
-- subsets and full canonical versions retain earlier-input key priority.
mergePreparedBodyCaches :: [(PreparedBodyCache, Module -> Bool)] -> IO PreparedBodyCache
mergePreparedBodyCaches sources = do
  selected <- mapM (\(cache, keep) -> do
    exact <- Map.filterWithKey (\owner _ -> keep owner) <$> readMVar (cachedExactBodies cache)
    originals <- Map.filterWithKey (\owner _ -> keep owner)
      <$> readMVar (cachedOriginalModules cache)
    pure (exact, originals)) sources
  components <- mapM (\(cache, keep) -> Map.filterWithKey (\owner _ -> keep owner)
    <$> readMVar (cachedFatComponents cache)) sources >>= mergeComponentBuckets
  PreparedBodyCache <$> newMVar (Map.unionsWith (Map.unionWith mergePreparedVariants) (map fst selected))
    <*> newMVar (Map.unionsWith (Map.unionWith mergeOriginalAlternatives) (map snd selected))
    <*> newMVar components

-- | Home activation reads only validated owner buckets. Within an owner,
-- exact subsets and canonical versions keep earlier-source key priority.
selectPreparedBodyCaches :: [(PreparedBodyCache, Set.Set Module)] -> IO PreparedBodyCache
selectPreparedBodyCaches sources = do
  selected <- mapM (\(cache, owners) -> do
    exact <- selectOwnerBuckets owners <$> readMVar (cachedExactBodies cache)
    originals <- selectOwnerBuckets owners <$> readMVar (cachedOriginalModules cache)
    pure (exact, originals)) sources
  components <- mapM (\(cache, owners) -> selectOwnerBuckets owners
    <$> readMVar (cachedFatComponents cache)) sources >>= mergeComponentBuckets
  PreparedBodyCache <$> newMVar (Map.unionsWith (Map.unionWith mergePreparedVariants) (map fst selected))
    <*> newMVar (Map.unionsWith (Map.unionWith mergeOriginalAlternatives) (map snd selected))
    <*> newMVar components

mergeComponentBuckets :: [Map Module (Shared.LoadCache (FatOriginalVersion, Unique, FatPreparationUnit) PreparedModule)]
  -> IO (Map Module (Shared.LoadCache (FatOriginalVersion, Unique, FatPreparationUnit) PreparedModule))
mergeComponentBuckets sources = mapM
  (Shared.mergeLoadCaches . map (\cache -> (cache,const True)))
  (Map.unionsWith (++) (map (Map.map pure) sources))

selectOwnerBuckets :: Set.Set Module -> Map Module value -> Map Module value
selectOwnerBuckets owners entries = Map.fromAscList
  [(owner, value) | owner <- Set.toAscList owners, Just value <- [Map.lookup owner entries]]

lookupOwnerEntry :: Ord key => Module -> key -> Map Module (Map key value) -> Maybe value
lookupOwnerEntry owner key entries = Map.lookup owner entries >>= Map.lookup key

mergePreparedVariants :: [PreparedModule] -> [PreparedModule] -> [PreparedModule]
mergePreparedVariants earlier later = foldl insert earlier later
  where
    insert known fresh
      | any (preparedSiteDependenciesEquivalent fresh) known = known
      | otherwise = known ++ [fresh]

evictPreparedBodyMatching :: PreparedBodyCache -> (Module -> Bool) -> IO ()
evictPreparedBodyMatching cache stale = do
  modifyMVar_ (cachedExactBodies cache) (pure . Map.filterWithKey (\owner _ -> not (stale owner)))
  modifyMVar_ (cachedOriginalModules cache)
    (pure . Map.filterWithKey (\owner _ -> not (stale owner)))
  modifyMVar_ (cachedFatComponents cache)
    (pure . Map.filterWithKey (\owner _ -> not (stale owner)))

-- A disabled observation is issued only after the normal version and site
-- witness lookup found an eligible product. It never describes a cold miss.
data PreparedBodyReuse = PreparedBodyReused | PreparedBodyMiss | PreparedBodyDisabled
  deriving (Eq, Show)

bodyReuseDisabled :: IO Bool
bodyReuseDisabled = (== Just "1") <$> lookupEnv "TIDEPOOL_DISABLE_BODY_REUSE"

type PreparedOriginalAlternative = (AdmittedFinalizedOriginal,PreparedModule)

-- Preserve each reusable site view of one canonical body, in completion order.
-- Unverifiable preparation can retain decoded Core, but has no reusable site
-- view; keep at most one such fallback until a proven view is available.
mergeOriginalAlternatives :: [PreparedOriginalAlternative] -> [PreparedOriginalAlternative]
  -> [PreparedOriginalAlternative]
mergeOriginalAlternatives = foldl' add
  where
    reusable (_,prepared) = preparedSiteDependenciesEquivalent prepared prepared
    add earlier candidate@(_,prepared)
      | not (reusable candidate) = if null earlier then [candidate] else earlier
      | any (\(_,old) -> preparedSiteDependenciesEquivalent old prepared) earlier = earlier
      | otherwise = filter reusable earlier ++ [candidate]

insertOriginalAlternative :: Module -> OriginalVersion -> PreparedOriginalAlternative
  -> Map Module (Map OriginalVersion [PreparedOriginalAlternative])
  -> Map Module (Map OriginalVersion [PreparedOriginalAlternative])
insertOriginalAlternative owner version alternative =
  Map.insertWith (Map.unionWith (flip mergeOriginalAlternatives)) owner
    (Map.singleton version [alternative])

-- Full original groups share the retained body owner with recovered subsets.
-- Canonical version and current site dependencies admit preparation separately;
-- a changed site witness can still reuse validated immutable decoded Core.
newPreparedOriginalModuleTaskPreparer :: HscEnv -> PreparedBodyCache -> ExactScope
  -> IO (Map String Id -> Module
    -> IO (Maybe (AdmittedFinalizedOriginal,PreparedBodyReuse,PreparedModuleTask)))
newPreparedOriginalModuleTaskPreparer env cache scope = do
  disabled <- bodyReuseDisabled
  admittedScope <- admitOriginalRecoveryScope env scope
  siteEnvironment <- resolvePreparedSiteEnvironment env
  pure $ \siblings owner -> do
    let key = originalVersionInRecoveryScope admittedScope owner
    alternatives <- maybe (pure []) (\version -> fromMaybe [] . lookupOwnerEntry owner version
      <$> readMVar (cachedOriginalModules cache)) key
    let previous = (,) <$> key <*> (fst <$> listToMaybe alternatives)
        hit = listToMaybe [prepared | (_,prepared) <- alternatives
          , preparedSiteDependenciesMatch siteEnvironment siblings prepared]
    original <- recoverAdmittedFinalizedOriginalWithPrevious admittedScope owner previous
    case original of
      Nothing -> pure Nothing
      Just (version,admitted) -> case hit of
        Just prepared | not disabled ->
          pure (Just (admitted,PreparedBodyReused,PreparedModuleTask (pure prepared)))
        _ -> do
          task <- acquirePreparedModuleWithSiteEnvironment siteEnvironment env
            (admittedOriginalLocation admitted) siblings (admittedOriginalModule admitted)
          let observation = case hit of
                Just _ | disabled -> PreparedBodyDisabled
                _ -> PreparedBodyMiss
          pure (Just (admitted,observation,PreparedModuleTask $ do
            prepared <- runPreparedModuleTask task
            modifyMVar_ (cachedOriginalModules cache)
              (pure . insertOriginalAlternative (originalVersionOwner version) version (admitted,prepared))
            pure prepared))

preparedBodyKey :: ConstructorWorkerPolicy -> Maybe FatOriginalVersion -> [CoreBind] -> PreparedBodyKey
preparedBodyKey workers version bindings = case version of
  Nothing -> ContextBodyKey workers census
  Just original -> FatBodyKey workers original census
  where
    census = [map (getKey . varUnique) (bindersOf binding) | binding <- bindings]
    bindersOf (NonRec binder _) = [binder]
    bindersOf (Rec pairs) = map fst pairs

-- | Prepare one owner's recovered bodies, answering from 'PreparedBodyCache'
-- when this daemon has already prepared exactly these groups for this owner.
-- A failure is never cached: it may simply not have been attempted with the
-- right toolchain state yet, the same rule 'OwnerInterfaceCache' follows.
prepareRecoveredBodies :: HscEnv -> OwnerInterfaceCache -> PreparedBodyCache
  -> Module -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedModule)
prepareRecoveredBodies hscEnv ownerCache bodyCache owner bindings = do
  prepare <- newPreparedBodyPreparer hscEnv ownerCache bodyCache
  prepare owner bindings

-- | The returned request preparer captures one admitted interface environment.
-- Callers cannot substitute another environment while reusing its site cache.
newPreparedBodyPreparer :: HscEnv -> OwnerInterfaceCache -> PreparedBodyCache
  -> IO (Module -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedModule))
newPreparedBodyPreparer env owners stable = do
  acquire <- newPreparedBodyTaskPreparer env owners stable
  pure $ \owner bindings -> acquire owner bindings >>= either (pure . Left) runPreparedBodyTask

newtype PreparedBodyTask = PreparedBodyTask (IO (Either RecoveredModuleFailure PreparedModule))

runPreparedBodyTask :: PreparedBodyTask -> IO (Either RecoveredModuleFailure PreparedModule)
runPreparedBodyTask (PreparedBodyTask action) = action

-- | Each original private component is one preparation unit. Completed pure
-- units survive subset growth; the shared per-key loader coalesces overlapping
-- growth and releases waiters on failure or cancellation. Live defining and
-- site context work is acquired before the returned task runs.
newPreparedComponentTaskPreparer :: HscEnv -> OwnerInterfaceCache -> PreparedBodyCache
  -> IO (FatIfaceSelection -> IO (Either RecoveredModuleFailure PreparedBodyTask))
newPreparedComponentTaskPreparer env owners stable = do
  disabled <- bodyReuseDisabled
  environment <- resolvePreparedSiteEnvironment env
  acquireSiteBatch <- newPreparedBodyTaskPreparerWithWorkers OmitConstructorWorkers environment env owners stable
  timing <- readTimingEnabled
  pure $ \selection -> do
    let version = fatSelectionVersion selection
        owner = fatOriginalOwner version
        components = sortOn fatComponentOrdinal (fatSelectionComponents selection)
    resolved <- acquireRecoveredContext env owners owner
    case resolved of
      Left failure -> pure (Left failure)
      Right context | not (ownerInterfaceMatchesOriginal context version) ->
        pure (Left (RecoveredModuleInterfaceFailure owner "declaring context differs from exact original interface"))
      Right context -> do
        bucket <- modifyMVar (cachedFatComponents stable) $ \buckets ->
          case Map.lookup owner buckets of
            Just cache -> pure (buckets,cache)
            Nothing -> do
              cache <- Shared.newLoadCache
              pure (Map.insert owner cache buckets,cache)
        let pureUnit component = intrinsicFree (censusPreparedIntrinsics
              (ownerInterfaceTyCons context) (IntMap.elems (fatComponentBindings component)))
            (plain, siteBearing) = partition pureUnit components
        acquired <- forM plain $ \component -> do
          let key = (version,Shared.ownerInterfaceIdentity context,OriginalComponent (fatComponentOrdinal component))
          completed <- Shared.lookupCompletedLoadCache bucket key
          task <- case completed of
            Just prepared | not disabled -> pure (Right (PreparedModuleTask (pure prepared)))
            _ -> acquireRecoveredWithWorkers OmitConstructorWorkers (Just environment) env owner context
              (IntMap.elems (fatComponentBindings component))
          pure ((component,key,disabled && maybe False (const True) completed),task)
        -- Site graphs have owner-local indexes. Until their owning type policy
        -- supplies checked rebasing, all selected site units form one typed
        -- batch; pure-unit reuse remains independent of that batch's growth.
        siteTask <- if null siteBearing then pure (Right Nothing) else
          fmap (fmap Just) (acquireSiteBatch (Just version) owner (IntMap.elems (IntMap.unions
            (map fatComponentBindings siteBearing))))
        -- CorePrep injects every defining constructor worker independently of
        -- the original body subset. Give that implicit arena one version-bound
        -- cache key and one place in assembly, including for site-bearing units.
        workerTask <- case disabled of
          False -> Shared.lookupCompletedLoadCache bucket (version,Shared.ownerInterfaceIdentity context,ConstructorWorkers) >>= \case
            Just completed -> pure (Right (PreparedModuleTask (pure completed)))
            Nothing -> acquireRecoveredWithSiteContext (Just environment) env owner context []
          True -> acquireRecoveredWithSiteContext (Just environment) env owner context []
        case (sequence [fmap ((,) pair) task | (pair,task) <- acquired], siteTask, workerTask) of
          (Left failure, _, _) -> pure (Left failure)
          (_, Left failure, _) -> pure (Left failure)
          (_, _, Left failure) -> pure (Left failure)
          (Right tasks, Right site, Right workers) -> pure (Right (PreparedBodyTask $ do
            outcome <- trySynchronous $ do
              pureResults <- forM tasks $ \((component,key,normalHitDisabled),task) -> do
                let lower = do
                      fresh <- runPreparedModuleTask task
                      unless (not (preparedUsesSiteAuthority fresh))
                        (ioError (userError "pure fat component acquired site authority"))
                      emitCount timing "prepared_recover_component_new_groups"
                        (fromIntegral (length (fatComponentOrdinals component)))
                      if normalHitDisabled then emitCount timing "prepared_recover_component_disabled_groups"
                        (fromIntegral (length (fatComponentOrdinals component))) else pure ()
                      pure (issueComponentSpellings component fresh)
                prepared <- if disabled then lower else Shared.lookupLoadCache bucket key lower
                pure (fatComponentOrdinals component,prepared)
              siteResult <- case site of
                Nothing -> pure []
                Just task -> do
                  result <- runPreparedBodyTask task >>= either (ioError . userError . show) pure
                  pure [(concatMap fatComponentOrdinals siteBearing,result)]
              workerResult <- if disabled then runPreparedModuleTask workers else
                Shared.lookupLoadCache bucket (version,Shared.ownerInterfaceIdentity context,ConstructorWorkers) (runPreparedModuleTask workers)
              assembled <- assembleComponentSelection selection workerResult (pureResults ++ siteResult)
              emitCount timing "prepared_recover_component_demanded_groups"
                (fromIntegral (fatSelectionDemandedGroupCount selection))
              emitCount timing "prepared_recover_component_prepared_groups"
                (fromIntegral (fatSelectionPreparedGroupCount selection))
              pure assembled
            pure $ case outcome of
              Left reason -> Left (RecoveredModulePreparationFailure owner reason)
              Right prepared -> Right prepared))

issueComponentSpellings :: FatIfaceComponent -> PreparedModule -> PreparedModule
issueComponentSpellings component prepared = prepared
  { preparedStableTopSpellings = Map.fromList
      [(varName binder, choose (Text.pack ("$tp.package."
          ++ show (fatComponentOrdinal component) ++ "." ++ show ordinal)))
      | (ordinal,binder) <- zip [0 :: Int ..] allTops
      , not (isExternalName (varName binder))] }
  where
    allTops = concatMap (preparedTopBinders . fst) (pmBindings prepared)
    reserved = Set.fromList (map (Text.pack . occNameString . nameOccName . varName)
      (fatComponentAllBinders component))
    choose stem | stem `Set.notMember` reserved = stem
    choose stem = head [candidate | suffix <- [1 :: Int ..]
      , let candidate = stem <> Text.pack (".reserved." ++ show suffix)
      , candidate `Set.notMember` reserved]

preparedTopBinders :: CgStgTopBinding -> [Id]
preparedTopBinders (StgTopStringLit identifier _) = [identifier]
preparedTopBinders (StgTopLifted (StgNonRec identifier _)) = [identifier]
preparedTopBinders (StgTopLifted (StgRec pairs)) = map fst pairs

-- Assembly is private to this preparation owner. Every supplied unit must be
-- an exact, disjoint part of the issued roster with disjoint emitted binders
-- and tag evidence. The separate implicit arena contains only the defining
-- constructor workers; the single site batch supplies the owner-local type graph.
assembleComponentSelection :: FatIfaceSelection -> PreparedModule -> [([Int],PreparedModule)] -> IO PreparedModule
assembleComponentSelection selection workers units = do
  timing <- readTimingEnabled
  emitCount timing "prepared_recover_assemblies" 1
  emitCount timing "prepared_recover_declaring_context_checks" (fromIntegral (1 + length units))
  let version = fatSelectionVersion selection
      owner = fatOriginalOwner version
      rosters = map fst units
      expected = Set.fromList (concatMap fatComponentOrdinals (fatSelectionComponents selection))
      supplied = concat rosters
      prepared = workers : map snd (sortOn (minimum . fst) units)
      names = concatMap (concatMap (map varName . preparedTopBinders . fst) . pmBindings) prepared
      spellings = concatMap (Map.elems . pmStableTopSpellings) prepared
      siteUnits = filter preparedUsesSiteAuthority prepared
      tagsDisjoint = snd (foldl (\(known,valid) item ->
        (plusNameEnv known (pmTagSigs item), valid && disjointNameEnv known (pmTagSigs item)))
        (emptyNameEnv,True) prepared)
      siblings = mergeConsistentIds (map pmSitedSiblings prepared)
      context = case preparedExpectedEntries workers of
        DeclaringEntries declared -> Just declared
        ProvisionalEntries _ -> Nothing
      consistentContext item = case (context, preparedExpectedEntries item) of
        (Just declared, DeclaringEntries other) -> sameOwnerInterfaceContext declared other
          && ownerInterfaceMatchesOriginal other version
        _ -> False
  unless (not (null units)
      && all (isJust . isDataConWorkId_maybe)
        (concatMap (preparedTopBinders . fst) (pmBindings workers))
      && not (preparedUsesSiteAuthority workers)
      && Map.null (pmStableTopSpellings workers)
      && all ((== version) . fatComponentVersion) (fatSelectionComponents selection)
      && all ((== owner) . pmModule) prepared
      && all ((== ExactBodySubset) . pmCoverage) prepared
      && Set.fromList supplied == expected && length supplied == Set.size expected
      && length names == Set.size (Set.fromList names)
      && length spellings == Set.size (Set.fromList spellings)
      && tagsDisjoint && length siteUnits <= 1
      && maybe False (const True) siblings && all consistentContext prepared)
    (ioError (userError "canonical fat component assembly has inconsistent or overlapping evidence"))
  let base = case siteUnits of site:_ -> site; [] -> head prepared
  pure base
    { preparedBindings = concatMap pmBindings prepared
    , preparedOriginalTopNames = Set.unions (map pmOriginalTopNames prepared)
    , preparedTagSigs = foldl plusNameEnv emptyNameEnv (map pmTagSigs prepared)
    , preparedStableTopSpellings = Map.unions (map pmStableTopSpellings prepared)
    , preparedSitedSiblings = fromMaybe Map.empty siblings
    , preparedExpectedEntries = preparedExpectedEntries workers
    }

-- Shared owner facts may occur in several units, but conflicting typed
-- identities cannot be hidden by the order of component assembly.
mergeConsistentIds :: Ord key => [Map key Id] -> Maybe (Map key Id)
mergeConsistentIds = foldl' addMap (Just Map.empty)
  where
    addMap known incoming = foldl' addId known (Map.toAscList incoming)
    addId Nothing _ = Nothing
    addId (Just known) (key, incoming) = case Map.lookup key known of
      Nothing -> Just (Map.insert key incoming known)
      Just previous
        | varName previous == varName incoming && eqType (idType previous) (idType incoming) -> Just known
        | otherwise -> Nothing

-- | Exact defining/site context acquisition stays on the coordinator. Tasks
-- contain only immutable inputs, lowering and short exact-body cache commits.
newPreparedBodyTaskPreparer :: HscEnv -> OwnerInterfaceCache -> PreparedBodyCache
  -> IO (Module -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedBodyTask))
newPreparedBodyTaskPreparer env owners bodyCache = do
  environment <- resolvePreparedSiteEnvironment env
  acquire <- newPreparedBodyTaskPreparerWithSiteEnvironment environment env owners bodyCache
  pure (acquire Nothing)

newPreparedBodyTaskPreparerWithSiteEnvironment :: PreparedSiteEnvironment -> HscEnv
  -> OwnerInterfaceCache -> PreparedBodyCache
  -> IO (Maybe FatOriginalVersion -> Module -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedBodyTask))
newPreparedBodyTaskPreparerWithSiteEnvironment = newPreparedBodyTaskPreparerWithWorkers IncludeConstructorWorkers

newPreparedBodyTaskPreparerWithWorkers :: ConstructorWorkerPolicy -> PreparedSiteEnvironment -> HscEnv
  -> OwnerInterfaceCache -> PreparedBodyCache
  -> IO (Maybe FatOriginalVersion -> Module -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedBodyTask))
newPreparedBodyTaskPreparerWithWorkers workers environment env owners bodyCache = do
  disabled <- bodyReuseDisabled
  timing <- readTimingEnabled
  scoped <- newMVar Map.empty
  pure $ \version owner bindings -> do
    let stable = cachedExactBodies bodyCache
        key = preparedBodyKey workers version bindings
    stableHit <- lookupOwnerEntry owner key <$> readMVar stable
    scopedHit <- lookupOwnerEntry owner key <$> readMVar scoped
    let matching candidates = case filter (preparedSiteDependenciesMatch environment Map.empty)
          (maybe [] id candidates) of
            hit:_ -> Just hit
            [] -> Nothing
    let normalHit = matching stableHit `orElse` matching scopedHit
    case normalHit of
      Just hit | not disabled -> pure (Right (PreparedBodyTask (pure (Right hit))))
      _ -> do
        resolved <- acquireRecoveredContext env owners owner
        acquired <- case resolved of
          Left failure -> pure (Left failure)
          Right context -> acquireRecoveredWithWorkers workers (Just environment) env owner context bindings
        pure $ fmap (\task -> PreparedBodyTask $ do
          outcome <- trySynchronous (runPreparedModuleTask task)
          case outcome of
            Left reason -> pure (Left (RecoveredModulePreparationFailure owner reason))
            Right prepared -> do
              if disabled && maybe False (const True) normalHit
                then emitCount timing "prepared_recover_body_disabled_batches" 1 else pure ()
              let cache = if preparedSiteDependenciesMatch environment Map.empty prepared then stable else scoped
              modifyMVar_ cache (pure . Map.insertWith (Map.unionWith (flip mergePreparedVariants)) owner
                (Map.singleton key [prepared]))
              pure (Right prepared)) acquired
  where
    orElse (Just hit) _ = Just hit
    orElse Nothing other = other

-- | Acquire the defining context for an exact recovered group and prepare it
-- through the same owner as source modules.  In particular, this does not
-- manufacture a 'ModSummary' for a package module (whose source path may be
-- absent) or attach the group to the caller's module.
--
-- The interface read ('readExactInterface') and typecheck
-- ('loadDefiningDetails') are the expensive, owner-only part of this call and
-- do not depend on 'bindings'; a daemon-lifetime 'OwnerInterfaceCache' lets a
-- re-preparation of the same owner (a later recovery round finds more of its
-- bindings) skip straight to 'prepareRecoveredModule'. Only a successful
-- read+typecheck is cached; see 'OwnerInterfaceCache'.
acquireRecoveredWithSiteContext :: Maybe PreparedSiteEnvironment -> HscEnv -> Module
  -> OwnerInterfaceContext -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedModuleTask)
acquireRecoveredWithSiteContext = acquireRecoveredWithWorkers IncludeConstructorWorkers

acquireRecoveredWithWorkers :: ConstructorWorkerPolicy -> Maybe PreparedSiteEnvironment -> HscEnv -> Module
  -> OwnerInterfaceContext -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedModuleTask)
acquireRecoveredWithWorkers workers environment hscEnv owner context bindings = do
  prepared <- trySynchronous $ do
    task <- acquireRecoveredModuleWithWorkers workers environment hscEnv
      (RecoveredModuleInput owner (ownerInterfaceLocation context)
        (ownerInterfaceTyCons context) bindings (ownerInterfaceEntries context))
    pure (PreparedModuleTask $ do
      output <- runPreparedModuleTask task
      pure output { preparedExpectedEntries = DeclaringEntries context })
  pure $ case prepared of
    Left reason -> Left (RecoveredModulePreparationFailure owner reason)
    Right value -> Right value

acquireRecoveredContext :: HscEnv -> OwnerInterfaceCache -> Module
  -> IO (Either RecoveredModuleFailure OwnerInterfaceContext)
acquireRecoveredContext hscEnv ownerCache owner = do
  cached <- lookupOwnerInterface ownerCache owner
  case cached of
    Just hit -> pure (Right hit)
    Nothing -> do
      exact <- readExactInterface hscEnv owner
      case exact of
        Left (ExactInterfaceFinderFailure reason) ->
          pure (Left (RecoveredModuleFinderFailure owner reason))
        Left (ExactInterfaceReadFailure reason) ->
          pure (Left (RecoveredModuleInterfaceFailure owner reason))
        Right (iface, location) -> do
          details <- trySynchronous (loadDefiningDetails hscEnv iface)
          case details of
            Left reason -> pure (Left (RecoveredModuleInterfaceFailure owner reason))
            Right (tycons, entries) -> do
              hit <- Shared.issueOwnerInterfaceContext owner (mi_iface_hash (mi_final_exts iface))
                location tycons (Map.fromList [(varName identifier,identifier) | identifier <- entries])
              cacheOwnerInterface ownerCache owner hit
              pure (Right hit)
  where
    loadDefiningDetails :: HscEnv -> ModIface -> IO ([TyCon], [Id])
    loadDefiningDetails env iface = do
      let doc = text "Tidepool recovered defining interface"
      -- Executable entry metadata belongs to the defining interface, even
      -- when an -O0 caller intentionally ignores optimization pragmas.
      let definingEnv = env { hsc_dflags = gopt_unset (hsc_dflags env) Opt_IgnoreInterfacePragmas }
      details <- initIfaceCheck doc definingEnv (typecheckIface iface)
      pure (typeEnvTyCons (md_types details), typeEnvIds (md_types details))

trySynchronous :: IO a -> IO (Either String a)
trySynchronous action = do
  outcome <- try action
  case outcome of
    Left exception -> case (fromException exception :: Maybe SomeAsyncException) of
      Just async -> throwIO async
      Nothing -> pure (Left (displayException (exception :: SomeException)))
    Right value -> pure (Right value)

acquireBindingsWithScope :: ConstructorWorkerPolicy -> Bool -> [Id] -> Map Name Id
  -> HscEnv -> Module -> ModLocation -> [TyCon] -> [CoreBind]
  -> PreparedCoverage -> Map String Id -> [YieldSite] -> [PreparedSite] -> TypeGraph
  -> [SiteRejection] -> Maybe TyCon -> Maybe PreparedSiteDependencies -> IntrinsicCensus -> IO PreparedModuleTask
acquireBindingsWithScope workers timing subsetScope entries hscEnv thisModule location tycons optimizedCore coverage
    siblings yieldSites sites graph rejections carrierTyCon dependencies census = do
  let baseFlags = hsc_dflags hscEnv
      preparedFlags =
        gopt_set
          (gopt_set
            (gopt_set
              (gopt_unset
                (gopt_unset baseFlags Opt_StgLiftLams)
                Opt_ByteCode)
              Opt_StgCSE)
            Opt_DoCoreLinting)
          Opt_DoStgLinting
      logger = hsc_logger hscEnv
      dataTyCons = case workers of
        IncludeConstructorWorkers -> filter isDataTyCon tycons
        OmitConstructorWorkers -> []
      interactiveVars = subsetScope ++ interactiveInScope (hsc_IC hscEnv)
      coreLint = lintCoreBindings preparedFlags CorePrep [] optimizedCore
      stgOptions = initStgPipelineOpts preparedFlags False
      originalTopNames = Set.fromList (map varName (bindersOfBinds optimizedCore))

  _ <- evaluate (Set.size originalTopNames)
  corePrepConfig <- initCorePrepConfig (hscEnv { hsc_dflags = preparedFlags })
  let lower = do
        displayLintResults logger False (text "Tidepool prepared-STG pre-CorePrep")
          (text "optimized Core") coreLint
        preppedCore <- corePrepPgm logger corePrepConfig
          (initCorePrepPgmConfig preparedFlags interactiveVars)
          thisModule location optimizedCore dataTyCons
        -- CorePrep can expose a different original reference through aliases
        -- or constructor wrappers. Select exact declaring facts after that
        -- transformation; supplied definitions keep their own analyzed facts.
        externalScope <- if Map.null entries then pure [] else
          mapM canonicalReference (recoveredSubsetScope thisModule preppedCore)
        -- CorePrep's configured end-pass performs the post-preparation Core lint.
        let (initialStg, _, _) =
              coreToStg (initCoreToStgOpts preparedFlags) thisModule location preppedCore
        (stgBindings, tagSigs) <-
          stg2stgWithExternalScope logger (externalScope ++ interactiveVars) externalScope
            stgOptions thisModule initialStg
        pure PreparedModule
          { preparedModule = thisModule
          , preparedCoverage = coverage
          , preparedBindings = stgBindings
          , preparedOriginalTopNames = originalTopNames
          , preparedStableTopSpellings = Map.empty
          , preparedTagSigs = tagSigs
          , preparedSitedSiblings = siblings
          , preparedYieldSites = yieldSites
          , preparedPreparedSites = sites
          , preparedTypeGraph = graph
          , preparedSiteRejections = rejections
          , preparedRequestSiteTyCon = carrierTyCon
          , preparedAuthorityDependent = not (intrinsicFree census)
          , preparedSiteDependencies = dependencies
          , preparedIntrinsicNames = Set.fromList (intrinsicNames census)
          , preparedExpectedEntries = ProvisionalEntries Map.empty
          }
      -- Wired-in declarations are deliberately absent from interface files.
      -- Their exact Names carry GHC's canonical TyThing, including implicit
      -- constructor workers. All references retain the same owner/type check.
      canonicalEntry name = case Map.lookup name entries of
        Just original -> Just original
        Nothing -> case wiredInNameTyThing_maybe name of
          Just (AnId original) -> Just original
          _ -> Nothing
      canonicalReference reference = case canonicalEntry (varName reference) of
        Just original
          | varName original == varName reference
          , nameModule_maybe (varName original) == Just thisModule
          , eqType (idType original) (idType reference) -> pure original
          | otherwise -> ioError (userError ("recovered external entry identity/type mismatch: "
              ++ showSDocUnsafe (ppr reference)))
        Nothing -> ioError (userError ("recovered external entry absent from declaring interface: "
          ++ showSDocUnsafe (ppr reference)))
  pure $ PreparedModuleTask $
    timeModuleDetailPhase timing "prepared_graph" "prepared_stg_task_service" thisModule lower
