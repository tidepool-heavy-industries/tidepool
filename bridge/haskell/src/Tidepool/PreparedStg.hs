-- | The request-local GHC handoff used to inspect the prepared program before
-- Tidepool commits to a cross-language execution schema.
--
-- This module deliberately retains GHC's stage-specific types.  It is an
-- internal compiler adapter, not the future serialized program model.
module Tidepool.PreparedStg
  ( PreparedModule, PreparedCoverage(..)
  , pmModule, pmCoverage, pmBindings, pmTagSigs, pmSitedSiblings, pmYieldSites, pmPreparedSites, pmTypeGraph, pmSiteRejections, pmRequestSiteTyCon
  , preparedBindingGroups, filterPreparedBindings, preparedRejectsIntrinsic, preparedUsesSiteAuthority, preparedExpectedEntry
  , prepareModule, PreparedModuleTask, acquirePreparedModule, runPreparedModuleTask
  , RecoveredModuleInput(..)
  , RecoveredModuleFailure(..)
  , prepareRecoveredModule
  , prepareRecoveredBodies
  , PreparedBodyCache, newPreparedBodyCache, evictPreparedBodyMatching
  , newPreparedBodyPreparer
  ) where

import Control.Exception
  ( SomeAsyncException, SomeException, displayException, fromException
  , throwIO, try )
import Control.Monad (unless)
import Control.Concurrent.MVar (MVar, modifyMVar_, newMVar, readMVar)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
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
  , elaboratePreparedSites, resolvePreparedSiblings, resolvePreparedInterfaceSiblings
  , resolveRecoveredSiblings, resolveSiteAuthority, requestSiteAuthority )
import Tidepool.PreparedStg.Internal
import Tidepool.FinalizedModule (FinalizedModule, finalizedTidyGuts)
import Tidepool.Timing (readTimingEnabled, timePhase)
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

-- | Complete fresh and admitted retained originals share this preparation owner.
prepareModule :: HscEnv -> ModLocation -> Map String Id -> FinalizedModule -> IO PreparedModule
prepareModule env location siblings finalized =
  acquirePreparedModule env location siblings finalized >>= runPreparedModuleTask

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
prepareRecoveredModule hscEnv input = do
  let entries = Map.fromList [(varName identifier, identifier) | identifier <- recoveredEntries input]
  bindings <- mapM (restoreRecoveredEntries entries) (recoveredBindings input)
  prepared <- prepareTypedBindings ExactBodySubset
    hscEnv (recoveredModule input) (recoveredLocation input)
    (recoveredTyCons input) Map.empty bindings
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
prepareTypedBindings :: PreparedCoverage -> HscEnv -> Module -> ModLocation
  -> [TyCon] -> Map String Id -> [CoreBind] -> IO PreparedModule
prepareTypedBindings coverage env owner location tycons imported bindings =
  acquireTypedBindings coverage env owner location tycons imported bindings
    >>= runPreparedModuleTask

acquireTypedBindings :: PreparedCoverage -> HscEnv -> Module -> ModLocation
  -> [TyCon] -> Map String Id -> [CoreBind] -> IO PreparedModuleTask
acquireTypedBindings coverage env owner location tycons imported bindings = do
  timing <- readTimingEnabled
  let census = censusPreparedIntrinsics tycons bindings
      ownedSiblings = resolvePreparedSiblings bindings
  (rewritten, sites, preparedSites, graph, rejections, carrier) <-
    if intrinsicFree census then pure (bindings, [], [], emptyTypeGraph, [], Nothing)
    else do
      recovered <- resolveRecoveredSiblings env bindings
      let siblings = Map.unions
            [ownedSiblings, imported, resolvePreparedInterfaceSiblings env, recovered]
      authority <- timePhase timing "prepared_site_authority" (resolveSiteAuthority env)
      (bodies, yields, issued, types, failures) <- timePhase timing "prepared_sites"
        (elaboratePreparedSites env authority siblings bindings)
      pure (bodies, yields, issued, types, failures, requestSiteAuthority authority)
  let subset = case coverage of
        CompleteSourceModule -> []
        ExactBodySubset -> recoveredSubsetScope owner rewritten
  acquireBindingsWithScope timing subset env owner location tycons
    rewritten coverage ownedSiblings sites preparedSites graph rejections carrier census

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

-- | Only an owner-issued census of intrinsic-free exact bodies permits
-- daemon reuse. Authority-dependent preparation belongs to one admitted request.
newtype PreparedBodyCache =
  PreparedBodyCache (MVar (Map (Module, [[Word64]]) PreparedModule))

newPreparedBodyCache :: IO PreparedBodyCache
newPreparedBodyCache = PreparedBodyCache <$> newMVar Map.empty

evictPreparedBodyMatching :: PreparedBodyCache -> (Module -> Bool) -> IO ()
evictPreparedBodyMatching (PreparedBodyCache cacheRef) stale =
  modifyMVar_ cacheRef (pure . Map.filterWithKey (\(owner, _) _ -> not (stale owner)))

preparedBodyKey :: Module -> [CoreBind] -> (Module, [[Word64]])
preparedBodyKey owner bindings =
  (owner, [map (getKey . varUnique) (bindersOf binding) | binding <- bindings])
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
  scoped <- newMVar Map.empty
  pure (prepareRecoveredBodiesWithSites env owners stable scoped)

prepareRecoveredBodiesWithSites :: HscEnv -> OwnerInterfaceCache -> PreparedBodyCache
  -> MVar (Map (Module, [[Word64]]) PreparedModule) -> Module -> [CoreBind]
  -> IO (Either RecoveredModuleFailure PreparedModule)
prepareRecoveredBodiesWithSites hscEnv ownerCache bodyCache scoped owner bindings = do
  let PreparedBodyCache stable = bodyCache
      key = preparedBodyKey owner bindings
  stableHit <- Map.lookup key <$> readMVar stable
  scopedHit <- Map.lookup key <$> readMVar scoped
  case stableHit `orElse` scopedHit of
    Just hit -> pure (Right hit)
    Nothing -> do
      outcome <- prepareRecoveredBodiesUncached hscEnv ownerCache owner bindings
      case outcome of
        Right prepared -> do
          let cache = if preparedAuthorityDependent prepared then scoped else stable
          modifyMVar_ cache (pure . Map.insert key prepared)
        Left _ -> pure ()
      pure outcome
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
prepareRecoveredBodiesUncached :: HscEnv -> OwnerInterfaceCache -> Module -> [CoreBind]
  -> IO (Either RecoveredModuleFailure PreparedModule)
prepareRecoveredBodiesUncached hscEnv ownerCache owner bindings = do
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
      prepared <- trySynchronous (prepareRecoveredModule hscEnv
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
  -> [SiteRejection] -> Maybe TyCon -> IntrinsicCensus -> IO PreparedModuleTask
acquireBindingsWithScope timing subsetScope hscEnv thisModule location tycons optimizedCore coverage
    siblings yieldSites sites graph rejections carrierTyCon census = do
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
  pure $ PreparedModuleTask $ timePhase timing "prepared_stg" $ do
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
      , preparedTagSigs = tagSigs
      , preparedSitedSiblings = siblings
      , preparedYieldSites = yieldSites
      , preparedPreparedSites = sites
      , preparedTypeGraph = graph
      , preparedSiteRejections = rejections
      , preparedRequestSiteTyCon = carrierTyCon
      , preparedAuthorityDependent = not (intrinsicFree census)
      , preparedIntrinsicNames = Set.fromList (intrinsicNames census)
      , preparedExpectedEntries = Map.empty
      }
