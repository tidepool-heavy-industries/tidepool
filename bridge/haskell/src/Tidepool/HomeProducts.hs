-- | Revalidate immutable home products in one fresh compiler transaction.
-- Boot declarations are source inputs, not executable products. A boot SCC
-- receives fresh GHC load validation before its original prepared bodies reuse.
module Tidepool.HomeProducts
  ( hydrateCandidateHomeProducts ) where

import Control.Exception
  ( SomeException, SomeAsyncException, displayException, fromException, throwIO, try )
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.Map.Strict qualified as Map
import Data.IORef (readIORef)
import GHC
  ( Ghc, ModSummary(..), getSession, parseModule, setSession, typecheckModule
  , tm_internals_, ms_mod_name, LoadHowMuch(LoadAllTargets), SuccessFlag(..)
  , topSortModuleGraph )
import GHC.Driver.Env
  ( HscEnv(..), hsc_HPT, hscUpdateHPT )
import GHC.Driver.Monad (reflectGhc, reifyGhc)
import GHC.Iface.Recomp (MaybeValidated(..), checkOldIface)
import GHC.Driver.Session (GeneralFlag(Opt_Pp), gopt, xopt)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Tc.Types (tcg_dependent_files)
import GHC.Types.SourceFile (HscSource(..))
import GHC.Unit.Home.ModInfo (addToHpt, lookupHpt)
import GHC.Unit.Module.Graph
  ( ModuleGraph, ModuleGraphNode(..), mgModSummaries', mkModuleGraph )
import GHC.Driver.Make (load')
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Utils.Outputable (ppr, renderWithContext, defaultSDocContext)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact, freshExactState, hydrateExactScope )
import Tidepool.RetainedUnfoldings (scopeRetainedHscEnv, scopeRetainedModuleGraph)

-- Every ordinary summary and every boot summary is from the current
-- downsweep. The caller has already checked source/package witnesses and
-- closed the candidate set over all selected home imports, including SOURCE.
-- No target summary belongs to this set. The load graph uses the same
-- representation flags as the producer load pass; validation order uses the
-- original downsweep graph retained in the environment.
hydrateCandidateHomeProducts
  :: HscEnv -> ModuleGraph -> [(ExactIfaceArtifact, ModIface)]
  -> [ModSummary] -> [ModSummary]
  -> Ghc (Either String HscEnv)
hydrateCandidateHomeProducts initial loadGraph interfaces summaries boots = reifyGhc $ \session -> do
  result <- try (reflectGhc hydrate session)
  case result of
    Left failure | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
    Left (failure :: SomeException) -> do
      -- Typechecking may have populated mutable EPS/finder state before the
      -- refusal. Ordinary source fallback starts from empty mutable tables.
      fresh <- freshExactState initial
      reflectGhc (setSession fresh) session
      pure (Left (displayException failure))
    Right environment -> pure (Right environment)
  where
    hydrate = do
      let selected = Map.fromList [(ms_mod_name summary, ()) | summary <- summaries]
          selectedGraph = mkModuleGraph
            [node | node@(ModuleNode _ summary) <- mgModSummaries' loadGraph
              , Map.member (ms_mod_name summary) selected]
          ordered = [summary | ModuleNode _ summary <- flattenSCCs
            (topSortModuleGraph True (hsc_mod_graph initial) Nothing)
            , ms_hsc_src summary == HsSrcFile
            , Map.member (ms_mod_name summary) selected]
          ordinaryByName = Map.fromList
            [(moduleName (mi_module iface), iface) | (_, iface) <- interfaces]
          ordinaryNames = Map.fromList [(ms_mod_name summary, ()) | summary <- ordered]
          bootNames = Map.fromList [(ms_mod_name summary, ()) | summary <- boots]
          graphBootNames = Map.fromList [(ms_mod_name summary, ())
            | ModuleNode _ summary <- mgModSummaries' selectedGraph
            , ms_hsc_src summary == HsBootFile]
      unless (Map.keysSet selected == Map.keysSet ordinaryByName
          && Map.keysSet selected == Map.keysSet ordinaryNames
          && length summaries == Map.size selected
          && length interfaces == Map.size ordinaryByName
          && length boots == Map.size bootNames
          && Map.keysSet bootNames == Map.keysSet graphBootNames) $
        liftIO (ioError (userError "cached home owner/graph inventory differs"))
      forM_ (summaries ++ boots) $ \summary ->
        unless (not (gopt Opt_Pp (ms_hspp_opts summary)) && not (any
            (`xopt` ms_hspp_opts summary)
            [LangExt.Cpp, LangExt.TemplateHaskell, LangExt.QuasiQuotes])) $
          liftIO (ioError (userError "cached home input has untracked compile-time inputs"))
      validationBase <- if null boots then pure initial else do
        -- In a boot SCC, an early extraction pass can consume a load-produced
        -- ordinary interface before that owner gets its prepared interface.
        -- Recreate that compiler state rather than guessing provenance from
        -- SOURCE syntax. The immutable prepared bodies still avoid extraction
        -- and lowering; this conservative path does not avoid GHC's load pass.
        setSession initial {hsc_mod_graph = selectedGraph}
        flag <- load' Nothing LoadAllTargets mkUnknownDiagnostic Nothing
          (scopeRetainedModuleGraph selectedGraph)
        unless (case flag of Succeeded -> True; Failed -> False) $
          liftIO (ioError (userError "cached home SOURCE graph failed fresh load"))
        current <- getSession
        forM_ boots $ \summary -> do
          unless (ms_hsc_src summary == HsBootFile) $
            liftIO (ioError (userError "cached home boot input is not a boot summary"))
          parsed <- parseModule summary
          typed <- typecheckModule parsed
          dependentFiles <- liftIO (readIORef (tcg_dependent_files (fst (tm_internals_ typed))))
          unless (null dependentFiles) $
            liftIO (ioError (userError "cached home boot input read untracked dependent files"))
        setSession current
        pure current
      hydrated <- liftIO (hydrateExactScope validationBase interfaces)
      setSession (if null boots then hydrated else validationBase)
      forM_ ordered $ \summary -> do
        iface <- maybe
          (liftIO (ioError (userError "cached interface owner missing"))) pure
          (Map.lookup (ms_mod_name summary) ordinaryByName)
        current <- getSession
        decision <- liftIO $ checkOldIface
          (scopeRetainedHscEnv (ms_mod summary) current) summary (Just iface)
        case decision of
          UpToDateItem _ -> pure ()
          OutOfDateItem reason _ -> liftIO (ioError (userError
            ("cached home product failed fresh interface validation: "
              ++ moduleNameString (ms_mod_name summary) ++ ": "
              ++ renderWithContext defaultSDocContext (ppr reason))))
        hmi <- maybe
          (liftIO (ioError (userError "hydrated home product owner missing"))) pure
          (lookupHpt (hsc_HPT hydrated) (ms_mod_name summary))
        setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name summary) hmi) current)
      final <- getSession
      let restored = final {hsc_mod_graph = hsc_mod_graph initial}
      setSession restored
      pure restored
