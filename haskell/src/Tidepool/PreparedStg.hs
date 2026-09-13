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
  ) where

import Control.Exception
  ( SomeAsyncException, SomeException, displayException, fromException
  , throwIO, try )
import Data.Map.Strict (Map)
import GHC.Core.Lint (displayLintResults)
import GHC.Core (CoreBind)
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
import GHC.Iface.Errors.Types (ReadInterfaceError(..))
import GHC.Iface.Load (readIface)
import GHC.IfaceToCore (typecheckIface)
import GHC.Stg.Pipeline (StgCgInfos, StgPipelineOpts(..), StgToDo(..), stg2stg)
import GHC.Stg.Syntax (CgStgTopBinding)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.Var.Set (IdSet)
import GHC.Types.Var (Id)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module (moduleName)
import GHC.Unit.Types (Module, moduleUnit, toUnitId)
import GHC.Unit.Module.Location (ModLocation(ml_hi_file))
import GHC.Data.Maybe (MaybeErr(..))
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Module.ModSummary (ModSummary(..))
import GHC.Types.PkgQual (PkgQual(OtherPkg))
import GHC.Types.TypeEnv (typeEnvTyCons)
import GHC.Utils.Outputable (ppr, showSDocUnsafe, text)
import Tidepool.EffectSchema (YieldSite)
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
  , pmFacts :: PreparedFacts
  }

-- | Run the same CorePrep/Core-to-STG/STG pipeline shape as GHC's native
-- object-code path, stopping before Cmm.  The caller must supply optimized,
-- typed module guts before any Tidepool type erasure or cross-module flattening.
prepareModule :: HscEnv -> ModSummary -> PreparedElaboration -> IO PreparedModule
prepareModule hscEnv summary elaboration =
  let guts = peGuts elaboration
  in prepareBindings hscEnv (cg_module guts) (ms_location summary)
       (cg_tycons guts) (peBindings elaboration)
       (peSitedSiblings elaboration) (peYieldSites elaboration)

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
  prepared <- prepareBindings hscEnv (recoveredModule input) (recoveredLocation input)
    (recoveredTyCons input) (recoveredBindings input) mempty []
  pure prepared { pmCoverage = ExactBodySubset }

-- | Acquire the defining context for an exact recovered group and prepare it
-- through the same owner as source modules.  In particular, this does not
-- manufacture a 'ModSummary' for a package module (whose source path may be
-- absent) or attach the group to the caller's module.
prepareRecoveredBodies :: HscEnv -> Module -> [CoreBind]
  -> IO (Either RecoveredModuleFailure PreparedModule)
prepareRecoveredBodies hscEnv owner bindings = do
  found <- trySynchronous (findImportedModule hscEnv (moduleName owner)
    (OtherPkg (toUnitId (moduleUnit owner))))
  case found of
    Left reason -> pure (Left (RecoveredModuleFinderFailure owner reason))
    Right (Found location foundOwner)
      | foundOwner == owner -> do
          details <- trySynchronous (loadDefiningDetails hscEnv owner location)
          case details of
            Left reason -> pure (Left (RecoveredModuleInterfaceFailure owner reason))
            Right tycons -> do
              prepared <- trySynchronous (prepareRecoveredModule hscEnv
                (RecoveredModuleInput owner location tycons bindings))
              pure $ case prepared of
                Left reason -> Left (RecoveredModulePreparationFailure owner reason)
                Right value -> Right value
      | otherwise -> pure (Left (RecoveredModuleFinderFailure owner
          ("finder returned " ++ renderModule foundOwner)))
    Right other -> pure (Left (RecoveredModuleFinderFailure owner
      (renderFindResult other)))
  where
    loadDefiningDetails :: HscEnv -> Module -> ModLocation -> IO [TyCon]
    loadDefiningDetails env modul location = do
      let doc = text "Tidepool recovered defining interface"
      readResult <- readIface (hsc_dflags env) (hsc_NC env) modul (ml_hi_file location)
      iface <- case readResult of
        Succeeded value -> pure value
        Failed failure -> ioError (userError (renderReadInterfaceError failure))
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

    renderModule = showSDocUnsafe . ppr
    renderReadInterfaceError failure = case failure of
      ExceptionOccurred path exception -> path ++ ": " ++ displayException exception
      HiModuleNameMismatchWarn path expected actual ->
        path ++ ": expected " ++ renderModule expected
          ++ ", found " ++ renderModule actual
    renderFindResult result = case result of
      Found _ foundOwner -> "found " ++ renderModule foundOwner
      NoPackage _ -> "no package"
      FoundMultiple _ -> "multiple matching modules"
      NotFound{} -> "module not found"

prepareBindings :: HscEnv -> Module -> ModLocation -> [TyCon] -> [CoreBind]
  -> Map String Id -> [YieldSite] -> IO PreparedModule
prepareBindings hscEnv thisModule location tycons optimizedCore siblings yieldSites = do
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
      interactiveVars = interactiveInScope (hsc_IC hscEnv)
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
    , pmFacts = extractPreparedFacts thisModule tagSigs (map fst preparedBindings)
    }
