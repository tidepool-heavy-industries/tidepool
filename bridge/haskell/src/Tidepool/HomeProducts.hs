-- | Revalidate immutable home products in one fresh compiler transaction.
-- Boot declarations are source inputs, not executable products. A boot SCC
-- receives fresh GHC load validation before its original prepared bodies reuse.
module Tidepool.HomeProducts
  ( hydrateCandidateHomeProducts, hydrateCandidateHomeProductsWithOriginals ) where

import Control.Exception
  ( SomeException, SomeAsyncException, displayException, fromException, throwIO, try )
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
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
import GHC.Unit.Home.ModInfo (addToHpt, eltsHpt, lookupHpt)
import GHC.Unit.Module.Graph
  ( ModuleGraph, ModuleGraphNode(..), NodeKey(..), mgModSummaries', mkModuleGraph
  , mgTransDeps, mkNodeKey, nodeDependencies )
import GHC.Driver.Make (load')
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Utils.Outputable (ppr, renderWithContext, defaultSDocContext)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact, freshExactState, hydrateExactScope )
import Tidepool.CompileInputPolicy (pluginInputIssues)
import Tidepool.FamilyConsistency (validateEnvironmentFamilies)
import Tidepool.RetainedUnfoldings (scopeRetainedHscEnv, scopeRetainedModuleGraph)
import Tidepool.Timing (emitCount, readTimingEnabled, timeDetailPhase)

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
hydrateCandidateHomeProducts initial loadGraph interfaces =
  hydrateCandidateHomeProductsWithOriginals initial loadGraph interfaces [] (\_ -> pure . Right)

-- Original HMIs contribute implementation Names; only the selected lexical
-- graph contributes instances and families during current interface checks.
hydrateCandidateHomeProductsWithOriginals
  :: HscEnv -> ModuleGraph -> [(ExactIfaceArtifact, ModIface)]
  -> [(ExactIfaceArtifact, ModIface)]
  -> (ModuleGraph -> HscEnv -> IO (Either String HscEnv))
  -> [ModSummary] -> [ModSummary] -> Ghc (Either String HscEnv)
hydrateCandidateHomeProductsWithOriginals initial loadGraph interfaces originals installLexical
    summaries boots = reifyGhc $ \session -> do
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
      timing <- liftIO readTimingEnabled
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
      forM_ (summaries ++ boots) $ \summary -> do
        let flags = ms_hspp_opts summary
        unless (not (gopt Opt_Pp flags) && not (any
            (`xopt` flags)
            [LangExt.Cpp, LangExt.TemplateHaskell, LangExt.QuasiQuotes])) $
          liftIO (ioError (userError "cached home input has untracked compile-time inputs"))
        case pluginInputIssues flags of
          [] -> pure ()
          issues -> liftIO (ioError (userError
            ("cached home input has untracked compiler plugin inputs: " ++ show issues)))
      validationBase <- if null boots then pure initial else do
        -- In a boot SCC, an early extraction pass can consume a load-produced
        -- ordinary interface before that owner gets its prepared interface.
        -- Recreate that context for every boot owner and its GHC-selected
        -- dependencies. Other accepted products retain the ordinary hydration
        -- path; their interfaces are checked in the original dependency order.
        sourceGraph <- either (liftIO . ioError . userError) pure
          (sourceValidationGraph selectedGraph boots)
        setSession initial {hsc_mod_graph = sourceGraph}
        flag <- timeDetailPhase timing "ghc_setup" "home_products_source_load" $
          load' Nothing LoadAllTargets mkUnknownDiagnostic Nothing
            (scopeRetainedModuleGraph sourceGraph)
        unless (case flag of Succeeded -> True; Failed -> False) $
          liftIO (ioError (userError "cached home SOURCE graph failed fresh load"))
        loaded <- getSession
        let current = loaded {hsc_mod_graph = selectedGraph}
        setSession current
        liftIO $ emitCount timing "home_products_source_load_owners"
          (toInteger (length (eltsHpt (hsc_HPT current))))
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
      combined <- liftIO (hydrateExactScope validationBase (originals ++ interfaces))
      hydrated <- liftIO (installLexical selectedGraph combined)
        >>= either (liftIO . ioError . userError) pure
      liftIO (validateEnvironmentFamilies hydrated)
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

-- GHC's cached reachability retains boot nodes and their real downsweep
-- edges. Root both forms of every boot owner, keep the original node order,
-- and refuse a dangling home edge before allowing any candidate to skip.
sourceValidationGraph :: ModuleGraph -> [ModSummary] -> Either String ModuleGraph
sourceValidationGraph graph boots = do
  let nodes = mgModSummaries' graph
      bootNames = Set.fromList (map ms_mod_name boots)
      roots = [mkNodeKey node | node@(ModuleNode _ summary) <- nodes
        , ms_mod_name summary `Set.member` bootNames]
      ordinaryRoots = Set.fromList [ms_mod_name summary
        | ModuleNode _ summary <- nodes, ms_hsc_src summary == HsSrcFile
        , ms_mod_name summary `Set.member` bootNames]
  unless (ordinaryRoots == bootNames) (Left "cached SOURCE ordinary owner is absent")
  closures <- mapM (\root -> maybe (Left "cached SOURCE reachability is absent")
    (Right . Set.insert root) (Map.lookup root (mgTransDeps graph))) roots
  let required = Set.unions closures
      selected = [node | node <- nodes, mkNodeKey node `Set.member` required]
      homeDependencies = [dependency | node <- selected, dependency <- nodeDependencies False node
        , NodeKey_Module _ <- [dependency]]
  unless (all (`Set.member` required) homeDependencies)
    (Left "cached SOURCE fresh-load closure is incomplete")
  pure (mkModuleGraph selected)
