-- | The request-local GHC handoff used to inspect the prepared program before
-- Tidepool commits to a cross-language execution schema.
--
-- This module deliberately retains GHC's stage-specific types.  It is an
-- internal compiler adapter, not the future serialized program model.
module Tidepool.PreparedStg
  ( PreparedModule(..)
  , PreparedElaboration(..)
  , PreparedPassProfile(..)
  , unelaboratedModule
  , prepareModule
  , RecoveredModuleInput(..)
  , prepareRecoveredModule
  ) where

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
import GHC.Stg.Pipeline (StgCgInfos, StgPipelineOpts(..), StgToDo(..), stg2stg)
import GHC.Stg.Syntax (CgStgTopBinding)
import GHC.Types.Var.Set (IdSet)
import GHC.Types.Var (Id)
import GHC.Unit.Types (Module)
import GHC.Unit.Module.Location (ModLocation)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Module.ModSummary (ModSummary(..))
import GHC.Utils.Outputable (text)
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
data PreparedModule = PreparedModule
  { pmModule :: Module
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

prepareRecoveredModule :: HscEnv -> RecoveredModuleInput -> IO PreparedModule
prepareRecoveredModule hscEnv input =
  prepareBindings hscEnv (recoveredModule input) (recoveredLocation input)
    (recoveredTyCons input) (recoveredBindings input) mempty []

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
    , pmLocation = location
    , pmPassProfile = profile
    , pmBindings = preparedBindings
    , pmTagSigs = tagSigs
    , pmSitedSiblings = siblings
    , pmYieldSites = yieldSites
    , pmFacts = extractPreparedFacts thisModule tagSigs (map fst preparedBindings)
    }
