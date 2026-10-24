module RetainedPluginTest (verifyCompilerReuse, verifyPreparedScope, verifyNativeBoundary) where

import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.IORef
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC
import GHC.Driver.Env (hsc_HPT)
import GHC.Driver.Env.Types (HscEnv(..))
import GHC.Driver.Make (load', newIfaceCache)
import GHC.Driver.Plugins
import GHC.Driver.Session (updOptLevel, gopt_set, GeneralFlag(Opt_ExposeAllUnfoldings))
import GHC.Core (Bind(..), CoreBind, Unfolding(..), isCompulsoryUnfolding, isBuiltinRule)
import GHC.Core.FVs (exprSomeFreeVarsList)
import GHC.Core.Class (classOpItems)
import GHC.Core.TyCon (tyConClass_maybe, tyConDataCons, isNewTyCon)
import GHC.Core.DataCon (dataConWrapId)
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries')
import GHC.Unit.Module.ModIface (mi_final_exts, mi_plugin_hash)
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Unit.Module.ModDetails (md_types, md_rules)
import GHC.Types.TypeEnv (typeEnvIds, typeEnvTyCons)
import GHC.Types.Id
  ( Id, isId, idName, isLocalId, isDataConId, isDFunId, isClassOpId
  , idInlinePragma, realIdUnfolding, idCoreRules )
import GHC.Types.Basic (isOpaquePragma)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Unit.Types (unitString)
import GHC.Unit.Module (moduleUnit)
import System.Directory (copyFile, renameFile)
import System.FilePath ((</>))
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.RetainedUnfoldings
  ( installRetainedUnfoldingsPlugin, scopeRetainedModuleGraph
  , scopeRetainedSummaryHscEnv, retainedContext, emptyRetainedContext
  , withNativeDeclarationBoundary, extendRetainedContext, moduleUnfoldingPolicyFor )
import Tidepool.GhcPipeline
  ( PipelineSelection(..), pprPipelineResult, pprModules, CompilePurpose(..), prHscEnv
  , withResidentPipelineSelected )
import Tidepool.PreparedStg (pmModule, pmBindings)

-- Count actual compiler passes per module, independently of Tidepool's
-- separate Core memo, through the resident daemon's warm interface cache.
-- Reusing the same home interfaces must skip Core for an
-- unchanged set. A changed set recompiles the module that defines the
-- retained identities, and its consumer through the changed interface; a
-- library module that defines none of them is never recompiled.
verifyCompilerReuse :: FilePath -> IO ()
verifyCompilerReuse dir = do
  forM_ ["ImportProducerExposed.hs", "ImportConsumerExposed.hs", "RetainedUnrelatedLibrary.hs"] $ \name ->
    copyFile ("test-prepared-stg" </> name) (dir </> name)
  compiledRef <- newIORef (Map.empty :: Map.Map String Int)
  libdir <- getLibdir
  let a = Set.fromList [identity "producerValue", identity "producerFn"]
      b = Set.singleton (identity "producerFn")
      identity name = SymbolIdentity "main" "ImportProducerExposed" "value" name Nothing
      producer = "ImportProducerExposed"
      consumer = "ImportConsumerExposed"
      library = "RetainedUnrelatedLibrary"
      observer = defaultPlugin
        { pluginRecompile = purePlugin
        , installCoreToDos = \_ todos -> pure
            (CoreDoPluginPass "CountHomeCompiles" (\guts -> do
               let name = moduleNameString (moduleName (mg_module guts))
               liftIO (modifyIORef' compiledRef (Map.insertWith (+) name 1))
               pure guts) : todos)
        }
      counting = StaticPlugin (PluginWithArgs observer []) True
      steps =
        [ (Set.empty, [producer, consumer, library])
        , (a, [producer, consumer])
        , (a, [])
        , (b, [producer, consumer])
        , (a, [producer, consumer])
        , (Set.empty, [producer, consumer])
        ]
  cache <- newIfaceCache
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags ((updOptLevel 2 flags)
      { importPaths = [dir], ghcLink = NoLink
      , objectDir = Just dir, hiDir = Just dir })
    env <- getSession
    let installed = installRetainedUnfoldingsPlugin emptyRetainedContext env
        plugins = hsc_plugins installed
    setSession (installed { hsc_plugins = plugins
      { staticPlugins = counting : staticPlugins plugins } })
    forM_ steps $ \(retained, expected) -> do
      liftIO (writeIORef compiledRef Map.empty)
      current <- getSession
      setSession (installRetainedUnfoldingsPlugin (retainedContext retained) current)
      targets <- mapM (\name -> guessTarget (dir </> name ++ ".hs") Nothing Nothing)
        [consumer, library]
      setTargets targets
      graph <- depanal [] False
      result <- load' (Just cache) LoadAllTargets mkUnknownDiagnostic Nothing
        (scopeRetainedModuleGraph graph)
      compiled <- liftIO (readIORef compiledRef)
      let expectedCounts = Map.fromList [(name, 1) | name <- expected]
      liftIO $ unless (succeeded result && compiled == expectedCounts)
        (ioError (userError ("retained plugin recompilation under "
          ++ show (Set.toList retained) ++ ": expected "
          ++ show expectedCounts ++ ", got " ++ show compiled)))
  where
    succeeded Succeeded = True
    succeeded Failed = False

-- The custom prepared path rebuilds an interface after GHC's load phase.
-- Its saved HscEnv must have the same module scope as load's compileOne path.
-- A large unrelated retained set must not perturb that interface's plugin
-- fingerprint or the prepared result on a warm resident compiler.
verifyPreparedScope :: FilePath -> IO ()
verifyPreparedScope dir = do
  let target = dir </> "RetainedTarget.hs"
      producerFile = dir </> "ImportProducerExposed.hs"
      replacementFile = dir </> "ImportProducerExposed.next"
      relevant = SymbolIdentity "main" "ImportProducerExposed" "value" "producerFn" Nothing
      unrelated = Set.fromList
        [ SymbolIdentity "main" "Unrelated" "value" ("symbol" <> Text.pack (show n)) Nothing
        | n <- [1 .. 1000 :: Int] ]
      shape result =
        [ (moduleNameString (moduleName (pmModule prepared)), length (pmBindings prepared))
        | prepared <- pprModules result ]
      preparedPluginHash result = case lookupHpt
          (hsc_HPT (prHscEnv (pprPipelineResult result)))
          (mkModuleName "ImportProducerExposed") of
        Just info -> mi_plugin_hash (mi_final_exts (hm_iface info))
        Nothing -> error "prepared producer interface was not registered"
  writeFile target $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module RetainedTarget where"
    , "import ImportConsumerExposed (consumerResult)"
    , "result :: Int"
    , "result = consumerResult"
    ]
  originalProducer <- readFile producerFile
  let producerSource = "{-# LANGUAGE QuasiQuotes #-}\n" ++ originalProducer
  let replaceProducer contents = do
        writeFile replacementFile contents
        renameFile replacementFile producerFile
  replaceProducer producerSource
  producerSummary <- withResidentPipelineSelected [dir] $ \compile -> do
    base <- compile PreparedStg (Set.singleton relevant) GeneralCompile Nothing target [] Nothing
    warm <- compile PreparedStg (Set.insert relevant unrelated) GeneralCompile Nothing target [] Nothing
    replaceProducer (producerSource ++ "\n-- force fresh prepared interface\n")
    crowded <- compile PreparedStg (Set.insert relevant unrelated) GeneralCompile Nothing target [] Nothing
    replaceProducer producerSource
    recovered <- compile PreparedStg (Set.singleton relevant) GeneralCompile Nothing target [] Nothing
    changed <- compile PreparedStg Set.empty GeneralCompile Nothing target [] Nothing
    unless (shape base == shape warm && shape warm == shape crowded
        && shape crowded == shape recovered)
      (ioError (userError "unrelated retained identities changed prepared module shape"))
    unless (preparedPluginHash base == preparedPluginHash warm
        && preparedPluginHash warm == preparedPluginHash crowded
        && preparedPluginHash crowded == preparedPluginHash recovered
        && preparedPluginHash base /= preparedPluginHash changed)
      (ioError (userError "prepared interface used an unscoped retained fingerprint"))
    case [summary | ModuleNode _ summary <- mgModSummaries'
            (hsc_mod_graph (prHscEnv (pprPipelineResult base)))
          , ms_mod_name summary == mkModuleName "ImportProducerExposed"] of
      [summary] -> pure summary
      _ -> ioError (userError "prepared producer omitted its actual module summary")
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags
    env <- getSession
    let installed = installRetainedUnfoldingsPlugin emptyRetainedContext env
        original = installRetainedUnfoldingsPlugin (retainedContext (Set.singleton relevant)) env
        pluginFor scoped = case staticPlugins (hsc_plugins scoped) of
          plugin : _ -> spPlugin plugin
          [] -> error "retained plugin was not installed"
        fingerprint owningEnvironment = do
          let plugin = pluginFor (scopeRetainedSummaryHscEnv producerSummary owningEnvironment)
          result <- liftIO (pluginRecompile (paPlugin plugin) (paArguments plugin))
          case result of
            MaybeRecompile hash -> pure hash
            _ -> error "custom module environment was not scoped"
    baseline <- fingerprint original
    withUnrelated <- fingerprint (installRetainedUnfoldingsPlugin
      (retainedContext (Set.insert relevant unrelated)) env)
    changed <- fingerprint installed
    recovered <- fingerprint original
    let owner = ms_mod producerSummary
        native = withNativeDeclarationBoundary owner (retainedContext (Set.singleton relevant))
        extended = extendRetainedContext unrelated native
        ownAddition = relevant {symbolOccurrence="producerValue"}
        more = extendRetainedContext (Set.singleton ownAddition) native
        expectedMore = withNativeDeclarationBoundary owner (retainedContext (Set.fromList [relevant,ownAddition]))
    nativeHash <- fingerprint (installRetainedUnfoldingsPlugin native env)
    extendedHash <- fingerprint (installRetainedUnfoldingsPlugin extended env)
    liftIO $ unless (nativeHash /= baseline && nativeHash == extendedHash
        && moduleUnfoldingPolicyFor owner native == moduleUnfoldingPolicyFor owner extended
        && moduleUnfoldingPolicyFor owner more == moduleUnfoldingPolicyFor owner expectedMore
        && moduleUnfoldingPolicyFor owner native /= moduleUnfoldingPolicyFor owner (retainedContext (Set.singleton relevant)))
      (ioError (userError "native declaration policy was lost, unscoped, or absent from its fingerprint"))
    let unscoped = pluginFor installed
    unscopedResult <- liftIO (pluginRecompile (paPlugin unscoped) (paArguments unscoped))
    liftIO $ case unscopedResult of
      ForceRecompile -> pure ()
      _ -> ioError (userError "unscoped retained plugin accepted an interface fingerprint")
    let other = StaticPlugin (PluginWithArgs defaultPlugin ["other-plugin", "argument"]) True
        installedPlugins = hsc_plugins installed
        withOther = installed { hsc_plugins = installedPlugins
          { staticPlugins = other : staticPlugins installedPlugins } }
        scopedOthers = staticPlugins (hsc_plugins (scopeRetainedSummaryHscEnv producerSummary withOther))
    liftIO $ case scopedOthers of
      first : _ | paArguments (spPlugin first) == ["other-plugin", "argument"]
                  && spInitialised first -> pure ()
      _ -> ioError (userError "retained scope changed another static plugin")
    liftIO $ unless (baseline == withUnrelated && baseline /= changed && baseline == recovered)
      (ioError (userError "scoped interface fingerprint changed with unrelated retained identities"))

-- One fixed graph exercises actual desugaring, optimization, interfaces and
-- warm GHC reuse. Runtime capture/effect settlement has its own session tests.
verifyNativeBoundary :: FilePath -> IO ()
verifyNativeBoundary dir = do
  let source = "NativeBoundarySource"
      producer = "NativeBoundaryProducer"
      instances = "NativeBoundaryInstances"
      consumer = "NativeBoundaryConsumer"
      owners = [producer, instances]
      names = [source, producer, instances, consumer]
      bindingPairs (NonRec identifier rhs) = [(identifier, rhs)]
      bindingPairs (Rec pairs) = pairs
      ordinary identifier = not (isDataConId identifier || isDFunId identifier || isClassOpId identifier)
      freeIds bindings = concatMap (exprSomeFreeVarsList isId . snd)
        (concatMap bindingPairs bindings)
      occurrence = occNameString . nameOccName . idName
      defining identifier = moduleNameString . moduleName <$> nameModule_maybe (idName identifier)
      sourceSeed identifier = defining identifier == Just source && occurrence identifier == "seed"
      require condition message = unless condition (ioError (userError message))
      rhsIds unfolding = case unfolding of
        CoreUnfolding {uf_tmpl=rhs} -> exprSomeFreeVarsList isId rhs
        DFunUnfolding {df_args=args} -> concatMap (exprSomeFreeVarsList isId) args
        _ -> []
  forM_ names $ \name -> copyFile ("test-prepared-stg" </> name ++ ".hs") (dir </> name ++ ".hs")
  snapshots <- newIORef Map.empty
  compiledRef <- newIORef (Map.empty :: Map.Map String Int)
  let observe phase guts = do
        let owner = moduleNameString (moduleName (mg_module guts))
        liftIO $ modifyIORef' snapshots (Map.insert (phase,owner) guts)
        whenFinal phase $ liftIO $ modifyIORef' compiledRef (Map.insertWith (+) owner 1)
        pure guts
      whenFinal phase action = if phase == "after" then action else pure ()
      observer = defaultPlugin
        { pluginRecompile=purePlugin
        , installCoreToDos = \_ todos -> pure
            (CoreDoPluginPass "ObserveNativeBoundaryBefore" (observe "before")
              : todos ++ [CoreDoPluginPass "ObserveNativeBoundaryAfter" (observe "after")]) }
      observing = StaticPlugin (PluginWithArgs observer []) True
  cache <- newIfaceCache
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (gopt_set ((updOptLevel 2 flags)
      {importPaths=[dir],ghcLink=NoLink,objectDir=Just dir,hiDir=Just dir}) Opt_ExposeAllUnfoldings)
    initial <- getSession
    let installed = installRetainedUnfoldingsPlugin emptyRetainedContext initial
        plugins = hsc_plugins installed
    setSession installed {hsc_plugins=plugins {staticPlugins=observing:staticPlugins plugins}}
    forM_ (zip [0 :: Int ..] [(False,False), (True,False), (True,False), (True,True), (False,False), (True,False)]) $ \(step,(native,crowded)) -> do
      liftIO (writeIORef compiledRef Map.empty)
      target <- guessTarget (dir </> consumer ++ ".hs") Nothing Nothing
      setTargets [target]
      graph <- depanal [] False
      let summaries = [summary | ModuleNode _ summary <- mgModSummaries' graph]
          moduleFor name = case [ms_mod summary | summary <- summaries, ms_mod_name summary == mkModuleName name] of
            [owner] -> owner
            _ -> error ("native boundary fixture lacks one actual summary: " ++ name)
          sourceOwner = moduleFor source
          retained = Set.singleton (SymbolIdentity (Text.pack (unitString (moduleUnit sourceOwner)))
            (Text.pack source) "value" "seed" Nothing)
          base = retainedContext retained
          selected = if native then foldr (withNativeDeclarationBoundary . moduleFor) base owners else base
          context = if crowded then extendRetainedContext (Set.singleton
            (SymbolIdentity "main" "AbsentUnrelatedModule" "value" "unrelated" Nothing)) selected else selected
      current <- getSession
      setSession (installRetainedUnfoldingsPlugin context current)
      result <- load' (Just cache) LoadAllTargets mkUnknownDiagnostic Nothing (scopeRetainedModuleGraph graph)
      unless (case result of Succeeded -> True; Failed -> False) $ liftIO $
        ioError (userError "native boundary fixture failed GHC load")
      observed <- liftIO (readIORef snapshots)
      let snapshot phase name = maybe (error ("missing native boundary phase " ++ phase ++ ":" ++ name)) id
            (Map.lookup (phase,name) observed)
          consumerIds = freeIds (mg_binds (snapshot "after" consumer))
          providerIds = concatMap (map fst . bindingPairs) (mg_binds (snapshot "before" producer))
          instanceIds = concatMap (map fst . bindingPairs) (mg_binds (snapshot "before" instances))
          defaultNames = [defaultName | tycon <- mg_tcs (snapshot "before" producer)
            , clas <- maybe [] pure (tyConClass_maybe tycon)
            , (_,Just (defaultName,_)) <- classOpItems clas]
          defaultIds = [identifier | identifier <- providerIds, idName identifier `elem` defaultNames]
      liftIO $ require (length defaultIds == 2
        && all (isCompulsoryUnfolding . realIdUnfolding) defaultIds
        && any (not . null . idCoreRules) providerIds
        && any (\identifier -> isDFunId identifier && not (null (idCoreRules identifier))) instanceIds
        && not (null (mg_rules (snapshot "before" producer)))
        && all isLocalId (filter ordinary providerIds))
        "native boundary controls lack compulsory defaults, authored rules, or LocalId executable tops"
      if not native then liftIO $ require (any sourceSeed consumerIds)
        "unprotected native fixture failed to expose its retained seed" else do
        liftIO $ do
          require (not (any sourceSeed consumerIds)) "native boundary exposed retained seed in its consumer"
          let named = Set.fromList [occurrence identifier | identifier <- consumerIds, defining identifier == Just producer]
          require (Set.fromList ["alias","inlineAlias","poly","strictPair"] `Set.isSubsetOf` named)
            ("native ordinary calls changed identity: " ++ show named)
          require (not (any (\identifier -> defining identifier == Just source && occurrence identifier == "sourceRule") consumerIds))
            "native policy disabled the imported library rule"
          require (any (\identifier -> defining identifier == Just source && occurrence identifier == "sourceId") consumerIds)
            "native module's authored imported-head rule survived"
          require (null (mg_rules (snapshot "after" producer))) "native producer published authored module rules"
        environment <- getSession
        forM_ owners $ \name -> case lookupHpt (hsc_HPT environment) (mkModuleName name) of
          Nothing -> liftIO (ioError (userError ("missing native boundary HMI: " ++ name)))
          Just home -> liftIO $ do
            let identifiers = typeEnvIds (md_types (hm_details home))
                defaults = [defaultName | tycon <- typeEnvTyCons (md_types (hm_details home))
                  , clas <- maybe [] pure (tyConClass_maybe tycon)
                  , (_,Just (defaultName,_)) <- classOpItems clas]
                selectors = [selector | tycon <- typeEnvTyCons (md_types (hm_details home))
                  , clas <- maybe [] pure (tyConClass_maybe tycon), (selector,_) <- classOpItems clas]
                newtypeConstructors = [dataConWrapId constructor
                  | tycon <- typeEnvTyCons (md_types (hm_details home)), isNewTyCon tycon
                  , constructor <- tyConDataCons tycon]
                expected = if name == producer then ["alias","inlineAlias","poly","strictPair"] else []
            forM_ expected $ \wanted -> case [identifier | identifier <- identifiers, occurrence identifier == wanted] of
              [identifier] -> require (isOpaquePragma (idInlinePragma identifier)) (wanted ++ " lost OPAQUE in final HMI")
              _ -> ioError (userError (wanted ++ " lost its final HMI identity"))
            forM_ defaults $ \defaultName -> case [identifier | identifier <- identifiers, idName identifier == defaultName] of
              [identifier] -> require (isOpaquePragma (idInlinePragma identifier)
                && not (isCompulsoryUnfolding (realIdUnfolding identifier))) "default method retained compulsory executable metadata"
              _ -> ioError (userError "default method is absent from its final HMI")
            let dfuns = filter isDFunId identifiers
                methods = [identifier | dictionary <- dfuns
                  , identifier <- rhsIds (realIdUnfolding dictionary)
                  , defining identifier == Just name, ordinary identifier]
            require (all (not . isOpaquePragma . idInlinePragma) dfuns) "native policy erased dictionary representation"
            require (all (isOpaquePragma . idInlinePragma) methods) "dictionary metadata kept stale ordinary method IdInfo"
            require (all (all isBuiltinRule . idCoreRules) identifiers) "native final HMI retained authored specialization rules"
            require (all isBuiltinRule (md_rules (hm_details home))) "native final HMI published authored rules"
            require (all (not . isOpaquePragma . idInlinePragma) selectors
              && all (isCompulsoryUnfolding . realIdUnfolding) newtypeConstructors)
              "native policy lost class-selector or newtype constructor representation"
            if name == producer
              then require (not (null newtypeConstructors) && any (any isBuiltinRule . idCoreRules) selectors)
                "constructor and class-selector representation controls are vacuous"
              else pure ()
            whenInstances name $ require (not (null dfuns) && not (null methods)) "dictionary control did not expose actual methods"
      compiled <- liftIO (readIORef compiledRef)
      liftIO $ do
        require (all (<= 1) (Map.elems compiled)) "native policy compiled one module more than once in a load"
        if step `elem` [2,3]
          then require (Map.null compiled) "same native policy or unrelated retained inputs recompiled the graph"
          else whenChanged step $ require (all (`Map.member` compiled) owners)
            "changed native policy reused an old producer interface"
  where
    whenInstances name action = if name == "NativeBoundaryInstances" then action else pure ()
    whenChanged step action = if step > 0 then action else pure ()
