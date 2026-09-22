-- | The request-local GHC handoff used to inspect the prepared program before
-- Tidepool commits to a cross-language execution schema.
--
-- This module deliberately retains GHC's stage-specific types.  It is an
-- internal compiler adapter, not the future serialized program model.
module Tidepool.PreparedStg
  ( PreparedModule(..)
  , PreparedCoverage(..)
  , PreparedElaboration(..)
  , PreparedPassProfile(..)
  , unelaboratedModule
  , prepareModule
  , RecoveredModuleInput(..)
  , RecoveredModuleFailure(..)
  , prepareRecoveredModule
  , prepareRecoveredBodies
  , PreparedBodyCache, newPreparedBodyCache, evictPreparedBodyMatching
  ) where

import Control.Exception
  ( SomeAsyncException, SomeException, displayException, fromException
  , throwIO, try )
import Control.Concurrent.MVar (MVar, modifyMVar_, newMVar, readMVar)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Word (Word64)
import GHC.Core.Lint (displayLintResults)
import GHC.Core (CoreBind, Bind(..), bindersOfBinds)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Core.Lint.Interactive (interactiveInScope)
import GHC.Core.Opt.Pipeline.Types (CoreToDo(CorePrep))
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
import GHC.Stg.Pipeline (StgCgInfos, StgPipelineOpts(..), StgToDo(..), stg2stg)
import GHC.Stg.Syntax (CgStgTopBinding)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.Var.Set (IdSet, elemVarSet, mkVarSet, unionVarSets)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Unique (getKey)
import GHC.Types.Var (Id, isId, varName, varUnique)
import GHC.Types.Name (isExternalName, nameModule_maybe)
import GHC.Unit.Types (Module)
import GHC.Unit.Module.Location (ModLocation)
import GHC.Unit.Module.ModIface (ModIface)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Module.ModSummary (ModSummary(..))
import GHC.Types.TypeEnv (typeEnvTyCons)
import GHC.Utils.Outputable (ppr, showSDocUnsafe, text)
import Tidepool.EffectSchema (YieldSite)
import Tidepool.PreparedSites (PreparedSite, SiteRejection)
import Tidepool.TypePolicy (TypeGraph(..))
import Tidepool.FatIface
  ( ExactInterfaceFailure(..), readExactInterface
  , OwnerInterfaceCache, lookupOwnerInterface, cacheOwnerInterface )
import Tidepool.PreparedFacts (PreparedFacts, extractPreparedFacts)

-- | Typed, pre-CorePrep input to the prepared pipeline.
--
-- The elaboration owner must populate the exact generated sibling 'Id's from
-- tidied home-module 'CgGuts' and rewrite typed Tidepool sites before handing
-- the bindings to 'prepareModule'. Keeping this as a GHC-typed internal record
-- prevents the later wire schema from becoming a second preparation API.
data PreparedElaboration = PreparedElaboration
  { peGuts :: CgGuts
  , peBindings :: [CoreBind]
  , peSitedSiblings :: Map String Id
  , peYieldSites :: [YieldSite]
  , pePreparedSites :: [PreparedSite]
  , peTypeGraph :: TypeGraph
  , peSiteRejections :: [SiteRejection]
  }

-- | The migration state used until the production elaborator is installed.
-- It deliberately records no site siblings or sidecar entries; prepared
-- evidence must not treat this value as proof that a module contains no sites.
unelaboratedModule :: CgGuts -> PreparedElaboration
unelaboratedModule guts = PreparedElaboration
  { peGuts = guts
  , peBindings = cg_binds guts
  , peSitedSiblings = mempty
  , peYieldSites = []
  , pePreparedSites = []
  , peTypeGraph = TypeGraph []
  , peSiteRejections = []
  }

-- | An executable record of the selected GHC 9.12 preparation policy.
-- The concrete phase list comes from GHC's native pipeline initializer after
-- Tidepool pins linting, CSE, lambda lifting, and bytecode preparation.
data PreparedPassProfile = PreparedPassProfile
  { pppCoreLint :: Bool
  , pppStgLint :: Bool
  , pppStgPhases :: [StgToDo]
  , pppForBytecode :: Bool
  }

-- | Prepared output for one defining module.  Module identity and location
-- remain attached while GHC performs dependency sorting, capture annotation,
-- tag inference, and rewriting.
-- | A missing top in a complete source module is a producer defect. A package
-- body subset can still reference unavailable package bodies; those remain
-- explicit globals with recovery diagnostics, not fabricated home definitions.
data PreparedCoverage = CompleteSourceModule | ExactBodySubset
  deriving (Eq, Show)

data PreparedModule = PreparedModule
  { pmModule :: Module
  , pmCoverage :: PreparedCoverage
  , pmLocation :: ModLocation
  , pmPassProfile :: PreparedPassProfile
  , pmBindings :: [(CgStgTopBinding, IdSet)]
  , pmTagSigs :: StgCgInfos
  , pmSitedSiblings :: Map String Id
  , pmYieldSites :: [YieldSite]
  , pmPreparedSites :: [PreparedSite]
  , pmTypeGraph :: TypeGraph
  -- | Typed sites that failed elaboration, raised only if projection
  -- reaches their top binder. Empty for recovered package bodies.
  , pmSiteRejections :: [SiteRejection]
  , pmFacts :: PreparedFacts
  }

-- | Run the same CorePrep/Core-to-STG/STG pipeline shape as GHC's native
-- object-code path, stopping before Cmm.  The caller must supply optimized,
-- typed module guts before any Tidepool type erasure or cross-module flattening.
prepareModule :: HscEnv -> ModSummary -> PreparedElaboration -> IO PreparedModule
prepareModule hscEnv summary elaboration = do
  let guts = peGuts elaboration
  prepared <- prepareBindings hscEnv (cg_module guts) (ms_location summary)
    (cg_tycons guts) (peBindings elaboration)
    (peSitedSiblings elaboration) (peYieldSites elaboration)
  pure prepared
    { pmPreparedSites = pePreparedSites elaboration
    , pmTypeGraph = peTypeGraph elaboration
    , pmSiteRejections = peSiteRejections elaboration
    }

-- | Exact optimized bindings retain their defining module and interface
-- context. No fabricated ModSummary or cross-module Core grouping is needed:
-- source and recovered inputs use the same preparation owner below.
data RecoveredModuleInput = RecoveredModuleInput
  { recoveredModule :: Module
  , recoveredLocation :: ModLocation
  , recoveredTyCons :: [TyCon]
  , recoveredBindings :: [CoreBind]
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
  prepared <- prepareBindingsWithScope
    (recoveredSubsetScope (recoveredModule input) (recoveredBindings input))
    hscEnv (recoveredModule input) (recoveredLocation input)
    (recoveredTyCons input) (recoveredBindings input) mempty []
  pure prepared { pmCoverage = ExactBodySubset }

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

-- | Daemon-lifetime cache of already prepared recovered bodies, keyed by the
-- owner and the exact binding groups asked for. Both halves of that key are
-- exact: a group is identified by its binders' 'Unique's, which are stable
-- for one process, and a binder's body comes from the owner's single cached
-- fat interface, so one key names one body set for as long as the cache
-- lives.
--
-- What the value does NOT depend on is what makes this sound to reuse across
-- requests: 'prepareBindingsWithScope' reads only CorePrep/STG configuration
-- from 'HscEnv', and a resident request changes nothing there — it changes
-- import paths, build-products flags, and the retained-generation set, which
-- steer GHC's own compile of the request's home module, not the preparation
-- of Core that was already read from an interface.
--
-- Evicted at the request boundary under the SAME predicate as the two caches
-- next to it (see 'Tidepool.FatIface.evictOwnerInterfaceMatching'): an owner
-- whose interface can change between requests must not keep prepared bodies
-- read from the old one.
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
  let PreparedBodyCache bodyRef = bodyCache
      key = preparedBodyKey owner bindings
  alreadyPrepared <- Map.lookup key <$> readMVar bodyRef
  case alreadyPrepared of
    Just hit -> pure (Right hit)
    Nothing -> do
      outcome <- prepareRecoveredBodiesUncached hscEnv ownerCache owner bindings
      case outcome of
        Right prepared ->
          modifyMVar_ bodyRef (pure . Map.insert key prepared)
        Left _ -> pure ()
      pure outcome

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
            Right tycons -> do
              let hit = (iface, location, tycons)
              cacheOwnerInterface ownerCache owner hit
              pure (Right hit)
  case resolved of
    Left failure -> pure (Left failure)
    Right (_iface, location, tycons) -> do
      prepared <- trySynchronous (prepareRecoveredModule hscEnv
        (RecoveredModuleInput owner location tycons bindings))
      pure $ case prepared of
        Left reason -> Left (RecoveredModulePreparationFailure owner reason)
        Right value -> Right value
  where
    loadDefiningDetails :: HscEnv -> ModIface -> IO [TyCon]
    loadDefiningDetails env iface = do
      let doc = text "Tidepool recovered defining interface"
      details <- initIfaceCheck doc env (typecheckIface iface)
      pure (typeEnvTyCons (md_types details))

    trySynchronous :: IO a -> IO (Either String a)
    trySynchronous action = do
      outcome <- try action
      case outcome of
        Left exception -> case (fromException exception :: Maybe SomeAsyncException) of
          Just async -> throwIO async
          Nothing -> pure (Left (displayException (exception :: SomeException)))
        Right value -> pure (Right value)

prepareBindings :: HscEnv -> Module -> ModLocation -> [TyCon] -> [CoreBind]
  -> Map String Id -> [YieldSite] -> IO PreparedModule
prepareBindings = prepareBindingsWithScope []

prepareBindingsWithScope :: [Id] -> HscEnv -> Module -> ModLocation -> [TyCon] -> [CoreBind]
  -> Map String Id -> [YieldSite] -> IO PreparedModule
prepareBindingsWithScope subsetScope hscEnv thisModule location tycons optimizedCore siblings yieldSites = do
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
      profile = PreparedPassProfile
        { pppCoreLint = True
        , pppStgLint = True
        , pppStgPhases = stgPipeline_phases stgOptions
        , pppForBytecode = stgPipeline_forBytecode stgOptions
        }

  displayLintResults logger False (text "Tidepool prepared-STG pre-CorePrep")
    (text "optimized Core") coreLint
  corePrepConfig <- initCorePrepConfig (hscEnv { hsc_dflags = preparedFlags })
  preppedCore <- corePrepPgm logger corePrepConfig
    (initCorePrepPgmConfig preparedFlags interactiveVars)
    thisModule location optimizedCore dataTyCons
  -- CorePrep's configured end-pass performs the post-preparation Core lint.
  let (initialStg, _, _) =
        coreToStg (initCoreToStgOpts preparedFlags) thisModule location preppedCore
  (preparedBindings, tagSigs) <-
    stg2stg logger interactiveVars stgOptions thisModule initialStg
  pure PreparedModule
    { pmModule = thisModule
    , pmCoverage = CompleteSourceModule
    , pmLocation = location
    , pmPassProfile = profile
    , pmBindings = preparedBindings
    , pmTagSigs = tagSigs
    , pmSitedSiblings = siblings
    , pmYieldSites = yieldSites
    , pmPreparedSites = []
    , pmTypeGraph = TypeGraph []
    , pmSiteRejections = []
    , pmFacts = extractPreparedFacts thisModule tagSigs (map fst preparedBindings)
    }
