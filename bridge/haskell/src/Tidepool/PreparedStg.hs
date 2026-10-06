-- | The request-local GHC handoff used to inspect the prepared program before
-- Tidepool commits to a cross-language execution schema.
--
-- This module deliberately retains GHC's stage-specific types.  It is an
-- internal compiler adapter, not the future serialized program model.
module Tidepool.PreparedStg
  ( PreparedModule, PreparedCoverage(..)
  , pmModule, pmCoverage, pmBindings, pmStableTopSpellings, pmTagSigs, pmSitedSiblings, pmYieldSites, pmPreparedSites, pmTypeGraph, pmSiteRejections, pmRequestSiteTyCon
  , preparedBindingGroups, filterPreparedBindings, preparedRejectsIntrinsic, preparedUsesSiteAuthority, preparedExpectedEntry
  , prepareModule, PreparedModuleTask, acquirePreparedModule, runPreparedModuleTask
  , PreparedSiteEnvironment, resolvePreparedSiteEnvironment, preparedSiteDependenciesMatch, preparedSiteDependenciesEquivalent
  , acquirePreparedModuleWithSiteEnvironment, prepareModuleWithSiteEnvironment
  , RecoveredModuleInput(..)
  , RecoveredModuleFailure(..)
  , prepareRecoveredModule
  , prepareRecoveredBodies
  , PreparedBodyCache, newPreparedBodyCache, copyPreparedBodyCache, mergePreparedBodyCaches, selectPreparedBodyCaches, evictPreparedBodyMatching
  , newPreparedOriginalModuleTaskPreparer
  , newPreparedBodyPreparer, newPreparedBodyTaskPreparer, PreparedBodyTask, runPreparedBodyTask
  ) where

import Control.Exception
  ( SomeAsyncException, SomeException, displayException, fromException
  , throwIO, try )
import Control.Monad (unless)
import Control.Concurrent.MVar (MVar, modifyMVar_, newMVar, readMVar)
import Data.List (foldl')
import Data.Maybe (fromMaybe, listToMaybe)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text (Text)
import Data.Word (Word32, Word64)
import GHC.Core.Lint (displayLintResults)
import GHC.Core (CoreBind, Bind(..), bindersOfBinds)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Core.Lint.Interactive (interactiveInScope)
import GHC.Core.Opt.Pipeline.Types (CoreToDo(CorePrep))
import GHC.Core.Opt.Arity (etaExpand)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Types.Id
  ( idArity, idType, idDmdSig, idCprSig
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
import GHC.Stg.Pipeline (StgCgInfos, stg2stg)
import GHC.Stg.Syntax (CgStgTopBinding)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.Var.Set (IdSet, elemVarSet, mkVarSet, unionVarSets)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Unique (getKey)
import GHC.Types.Var (Id, isId, varName, varUnique)
import GHC.Types.Name (Name, isExternalName, nameModule_maybe)
import GHC.Unit.Types (Module)
import GHC.Unit.Module.Location (ModLocation)
import GHC.Unit.Module.ModIface (ModIface)
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
import Tidepool.Timing (readTimingEnabled, timePhase, timeSection, emitDetailPhase)
import Tidepool.TypePolicy (TypeGraph, emptyTypeGraph)
import Tidepool.FatIface
  ( ExactInterfaceFailure(..), readExactInterface
  , OwnerInterfaceContext(..), OwnerInterfaceCache, lookupOwnerInterface, cacheOwnerInterface )

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
  Map.lookup (varName identifier) (preparedExpectedEntries prepared)

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
  , recoveredEntries :: [Id]
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
acquireRecoveredModule hscEnv input = do
  let entries = Map.fromList [(varName identifier, identifier) | identifier <- recoveredEntries input]
  bindings <- mapM (restoreRecoveredEntries entries) (recoveredBindings input)
  task <- acquireTypedBindings ExactBodySubset
    hscEnv (recoveredModule input) (recoveredLocation input)
    (recoveredTyCons input) Map.empty bindings
  pure $ PreparedModuleTask $ do
    prepared <- runPreparedModuleTask task
    pure prepared { preparedExpectedEntries = entries }

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
acquireTypedBindingsWithSiteEnvironment selected coverage env owner location tycons imported bindings = do
  timing <- readTimingEnabled
  let census = censusPreparedIntrinsics tycons bindings
      ownedSiblings = resolvePreparedSiblings bindings
  (rewritten, sites, preparedSites, graph, rejections, carrier, dependencies) <-
    if intrinsicFree census then pure (bindings, [], [], emptyTypeGraph, [], Nothing, Nothing)
    else do
      environment <- maybe (timePhase timing "prepared_site_authority" (resolvePreparedSiteEnvironment env))
        pure selected
      (bodies, yields, issued, types, failures, evidence) <- timePhase timing "prepared_sites"
        (elaboratePreparedSitesWithDependencies env environment ownedSiblings imported bindings)
      pure (bodies, yields, issued, types, failures, Sites.preparedSiteRequestAuthority environment, Just evidence)
  let subset = case coverage of
        CompleteSourceModule -> []
        ExactBodySubset -> recoveredSubsetScope owner rewritten
  acquireBindingsWithScope timing subset env owner location tycons
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

-- | Completed bodies retain their owner identity. Each acquiring preparer
-- checks its canonical/exact inputs and preparation dependencies before reuse.
data PreparedBodyCache = PreparedBodyCache
  { cachedExactBodies :: MVar (Map Module (Map [[Word64]] PreparedModule))
  , cachedOriginalModules :: MVar (Map Module (Map OriginalVersion [PreparedOriginalAlternative]))
  }

newPreparedBodyCache :: IO PreparedBodyCache
newPreparedBodyCache = PreparedBodyCache <$> newMVar Map.empty <*> newMVar Map.empty

-- | Attempt additions are private until their compiler context is promoted.
copyPreparedBodyCache :: PreparedBodyCache -> IO PreparedBodyCache
copyPreparedBodyCache cache = PreparedBodyCache
  <$> (readMVar (cachedExactBodies cache) >>= newMVar)
  <*> (readMVar (cachedOriginalModules cache) >>= newMVar)

-- | Select completed acceleration for the validated owner roster. Both exact
-- subsets and full canonical versions retain earlier-input key priority.
mergePreparedBodyCaches :: [(PreparedBodyCache, Module -> Bool)] -> IO PreparedBodyCache
mergePreparedBodyCaches sources = do
  selected <- mapM (\(cache, keep) -> do
    exact <- Map.filterWithKey (\owner _ -> keep owner) <$> readMVar (cachedExactBodies cache)
    originals <- Map.filterWithKey (\owner _ -> keep owner)
      <$> readMVar (cachedOriginalModules cache)
    pure (exact, originals)) sources
  PreparedBodyCache <$> newMVar (Map.unionsWith Map.union (map fst selected))
    <*> newMVar (Map.unionsWith (Map.unionWith mergeOriginalAlternatives) (map snd selected))

-- | Home activation reads only validated owner buckets. Within an owner,
-- exact subsets and canonical versions keep earlier-source key priority.
selectPreparedBodyCaches :: [(PreparedBodyCache, Set.Set Module)] -> IO PreparedBodyCache
selectPreparedBodyCaches sources = do
  selected <- mapM (\(cache, owners) -> do
    exact <- selectOwnerBuckets owners <$> readMVar (cachedExactBodies cache)
    originals <- selectOwnerBuckets owners <$> readMVar (cachedOriginalModules cache)
    pure (exact, originals)) sources
  PreparedBodyCache <$> newMVar (Map.unionsWith Map.union (map fst selected))
    <*> newMVar (Map.unionsWith (Map.unionWith mergeOriginalAlternatives) (map snd selected))

selectOwnerBuckets :: Set.Set Module -> Map Module value -> Map Module value
selectOwnerBuckets owners entries = Map.fromAscList
  [(owner, value) | owner <- Set.toAscList owners, Just value <- [Map.lookup owner entries]]

lookupOwnerEntry :: Ord key => Module -> key -> Map Module (Map key value) -> Maybe value
lookupOwnerEntry owner key entries = Map.lookup owner entries >>= Map.lookup key

insertOwnerEntry :: Ord key => Module -> key -> value -> Map Module (Map key value) -> Map Module (Map key value)
insertOwnerEntry owner key value = Map.insertWith Map.union owner (Map.singleton key value)

evictPreparedBodyMatching :: PreparedBodyCache -> (Module -> Bool) -> IO ()
evictPreparedBodyMatching cache stale = do
  modifyMVar_ (cachedExactBodies cache) (pure . Map.filterWithKey (\owner _ -> not (stale owner)))
  modifyMVar_ (cachedOriginalModules cache)
    (pure . Map.filterWithKey (\owner _ -> not (stale owner)))

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
    -> IO (Maybe (AdmittedFinalizedOriginal,Bool,PreparedModuleTask)))
newPreparedOriginalModuleTaskPreparer env cache scope = do
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
        Just prepared ->
          pure (Just (admitted,True,PreparedModuleTask (pure prepared)))
        _ -> do
          task <- acquirePreparedModuleWithSiteEnvironment siteEnvironment env
            (admittedOriginalLocation admitted) siblings (admittedOriginalModule admitted)
          pure (Just (admitted,False,PreparedModuleTask $ do
            prepared <- runPreparedModuleTask task
            modifyMVar_ (cachedOriginalModules cache)
              (pure . insertOriginalAlternative (originalVersionOwner version) version (admitted,prepared))
            pure prepared))

preparedBodyKey :: [CoreBind] -> [[Word64]]
preparedBodyKey bindings =
  [map (getKey . varUnique) (bindersOf binding) | binding <- bindings]
  where
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

-- | Exact defining/site context acquisition stays on the coordinator. Tasks
-- contain only immutable inputs, lowering and short exact-body cache commits.
newPreparedBodyTaskPreparer :: HscEnv -> OwnerInterfaceCache -> PreparedBodyCache
  -> IO (Module -> [CoreBind] -> IO (Either RecoveredModuleFailure PreparedBodyTask))
newPreparedBodyTaskPreparer env owners bodyCache = do
  scoped <- newMVar Map.empty
  pure $ \owner bindings -> do
    let stable = cachedExactBodies bodyCache
        key = preparedBodyKey bindings
    stableHit <- lookupOwnerEntry owner key <$> readMVar stable
    scopedHit <- lookupOwnerEntry owner key <$> readMVar scoped
    case stableHit `orElse` scopedHit of
      Just hit -> pure (Right (PreparedBodyTask (pure (Right hit))))
      Nothing -> do
        acquired <- acquireRecoveredBodiesUncached env owners owner bindings
        pure $ fmap (\task -> PreparedBodyTask $ do
          outcome <- trySynchronous (runPreparedModuleTask task)
          case outcome of
            Left reason -> pure (Left (RecoveredModulePreparationFailure owner reason))
            Right prepared -> do
              let cache = if preparedAuthorityDependent prepared then scoped else stable
              modifyMVar_ cache (pure . insertOwnerEntry owner key prepared)
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
acquireRecoveredBodiesUncached :: HscEnv -> OwnerInterfaceCache -> Module -> [CoreBind]
  -> IO (Either RecoveredModuleFailure PreparedModuleTask)
acquireRecoveredBodiesUncached hscEnv ownerCache owner bindings = do
  cached <- lookupOwnerInterface ownerCache owner
  resolved <- case cached of
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
              let hit = OwnerInterfaceContext location tycons entries
              cacheOwnerInterface ownerCache owner hit
              pure (Right hit)
  case resolved of
    Left failure -> pure (Left failure)
    Right context -> do
      prepared <- trySynchronous (acquireRecoveredModule hscEnv
        (RecoveredModuleInput owner (ownerInterfaceLocation context)
          (ownerInterfaceTyCons context) bindings (ownerInterfaceEntries context)))
      pure $ case prepared of
        Left reason -> Left (RecoveredModulePreparationFailure owner reason)
        Right value -> Right value
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

acquireBindingsWithScope :: Bool -> [Id] -> HscEnv -> Module -> ModLocation -> [TyCon] -> [CoreBind]
  -> PreparedCoverage -> Map String Id -> [YieldSite] -> [PreparedSite] -> TypeGraph
  -> [SiteRejection] -> Maybe TyCon -> Maybe PreparedSiteDependencies -> IntrinsicCensus -> IO PreparedModuleTask
acquireBindingsWithScope timing subsetScope hscEnv thisModule location tycons optimizedCore coverage
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
      dataTyCons = filter isDataTyCon tycons
      interactiveVars = subsetScope ++ interactiveInScope (hsc_IC hscEnv)
      coreLint = lintCoreBindings preparedFlags CorePrep [] optimizedCore
      stgOptions = initStgPipelineOpts preparedFlags False

  corePrepConfig <- initCorePrepConfig (hscEnv { hsc_dflags = preparedFlags })
  let lower = do
        displayLintResults logger False (text "Tidepool prepared-STG pre-CorePrep")
          (text "optimized Core") coreLint
        preppedCore <- corePrepPgm logger corePrepConfig
          (initCorePrepPgmConfig preparedFlags interactiveVars)
          thisModule location optimizedCore dataTyCons
        -- CorePrep's configured end-pass performs the post-preparation Core lint.
        let (initialStg, _, _) =
              coreToStg (initCoreToStgOpts preparedFlags) thisModule location preppedCore
        (stgBindings, tagSigs) <-
          stg2stg logger interactiveVars stgOptions thisModule initialStg
        pure PreparedModule
          { preparedModule = thisModule
          , preparedCoverage = coverage
          , preparedBindings = stgBindings
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
          , preparedExpectedEntries = Map.empty
          }
  pure $ PreparedModuleTask $ do
    (prepared, serviceMs) <- timeSection lower
    emitDetailPhase timing "prepared_graph" "prepared_stg_task_service" serviceMs
    pure prepared
