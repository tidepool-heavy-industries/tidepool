{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}

module ModuleProductRoundtripTest
  ( verifyModuleProductInterfaceRoundtrip, verifyOriginalProductCatalogue ) where

import Tidepool.PreparedStg.Internal (PreparedModule(..))
import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import Data.Maybe (listToMaybe)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Word (Word32)
import GHC
  ( backend, getSession, getSessionDynFlags, ms_mod, ms_textual_imps, unLoc
  , noBackend
  , parseModule, runGhc, setSession, setSessionDynFlags, typecheckModule )
import GHC.Driver.Env
  ( hsc_HPT, hsc_dflags, hsc_mod_graph )
import GHC.Driver.Session (GhcLink(..), ghcLink, importPaths, targetProfile)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Iface.Recomp.Flags (fingerprintDynFlags, fingerprintOptFlags)
import GHC.Iface.Recomp.Binary (putNameLiterally)
import GHC.Iface.Recomp (MaybeValidated(..), checkOldIface)
import GHC.Types.Id (idName)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module (mkModuleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries', mkModuleGraph)
import GHC.Unit.Module.ModIface (mi_extra_decls, mi_final_exts, mi_flag_hash, mi_opt_hash)
import GHC.Unit.Types (moduleName, unitString)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import System.Directory (copyFile, createDirectoryIfMissing, getFileSize, renameFile)
import System.FilePath ((</>), takeDirectory)
import Numeric (showHex)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , projectPreparedModuleGroups, projectPreparedModuleGroupsSelected, topBinders
  , projectOriginalHomeModuleProducts, preparedModuleProductOutcomes
  , prepareProjection, projectSelected )
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), freshExactState, readExactIfaceArtifacts, hydrateExactScope, newOriginalInterfaceArtifacts
  , noCheckedValueImports, installExactLexicalGraph )
import Tidepool.GhcPipeline
  ( prHscEnv, PipelineSelection(..), pprPipelineResult, pprModules, pprProductInterfaces, pprFinalizedModules
  , pprAcceptedCandidates, runPipelineSelected )
import Tidepool.PreparedSites (SiteRejection(..))
import Tidepool.PreparedStg (pmModule, pmBindings, pmSiteRejections)
import Tidepool.RetainedUnfoldings (scopeRetainedSummaryHscEnv)
import Tidepool.ModuleCandidates
  ( CandidateGroup(..), CandidateGlobal(..), ModuleCandidate(..), readModuleCandidates )
import Tidepool.Test.GenuineCandidate
  ( FixtureCompilerInput(..), captureCompilerFixture, writeGenuineCandidateManifestFor )
import Tidepool.Test.CandidateCodec (CandidateCodecCase(..), writeCandidateCodecFixture)
import Tidepool.OriginalProductRoots (requiredOriginalPackageGlobalsWithRetained)
import Tidepool.CompilerProducts qualified as Products

-- Exercise the skinny interface retained for a prepared defining module,
-- then import it from a new GHC session with its source absent.
verifyModuleProductInterfaceRoundtrip :: FilePath -> IO ()
verifyModuleProductInterfaceRoundtrip work = do
  let a = work </> "ModuleProductA.hs"
      b = work </> "ModuleProductB.hs"
      hi = work </> "ModuleProductA.hi"
      tamperedHi = work </> "ModuleProductA.tampered.hi"
      name = mkModuleName "ModuleProductA"
  copyFile "test-prepared-stg/ModuleProductA.hs" a
  copyFile "test-prepared-stg/ModuleProductB.hs" b
  ordinary <- runPipelineSelected PreparedStg b [work]
  unless (Map.null (pprProductInterfaces ordinary)) $
    ioError (userError "ordinary prepared compilation retained product interfaces")
  result <- runPipelineSelected (PreparedProducts Nothing) b [work]
  let prepared = [module_ | module_ <- pprModules result
                          , moduleNameString (moduleName (pmModule module_))
                              == "ModuleProductA"]
      producer = prHscEnv (pprPipelineResult result)
      consumerSummaries = [summary | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph producer)
                                   , moduleNameString (moduleName (ms_mod summary)) == "ModuleProductB"]
  moduleA <- case prepared of
    [value] | not (null (pmBindings value)) -> pure value
    _ -> ioError (userError "prepared product omitted ModuleProductA definitions")
  let context = ProjectionContext
        { projectionProfile = "ghc-9.12-prepared-stg"
        , projectionToolchain = "ghc-9.12.2"
        , projectionTarget = Schema.TargetDescriptor Schema.X86_64
            Schema.LittleEndian 64 64 "sysv64" []
        , projectionRetainedGenerations = mempty
        , projectionCurrentOriginals = mempty
        , projectionEntry = Schema.SymbolIdentity "main" "ModuleProductA"
            "value" "produce" Nothing
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTimeAuthority = Nothing
        , projectionJsonAuthority = Nothing
        , projectionTextUnit = Nothing
        }
  groups <- either (ioError . userError . show) pure
    (projectPreparedModuleGroups context moduleA)
  case [ binder | (binding, _) <- pmBindings moduleA
                , binder <- topBinders binding ] of
    binder : _ -> case projectPreparedModuleGroups context (moduleA
      { preparedSiteRejections = SiteRejection binder "rejected group site"
          : pmSiteRejections moduleA }) of
      Left (RejectedTypedSite "rejected group site") -> pure ()
      outcome -> ioError (userError ("group projection admitted rejected site: "
        ++ show outcome))
    [] -> ioError (userError "module product has no top binder for rejection test")
  unless (length groups >= 2) $
    ioError (userError "module product omitted second STG group")
  unless (any ((>= 2) . length . projectedBinders) groups) $
    ioError (userError "module product split a recursive STG group")
  let selectedOrdinal = projectedOriginalOrdinal (groups !! 1)
  selected <- either (ioError . userError . show) pure
    (projectPreparedModuleGroupsSelected context moduleA
      (Just (Set.singleton selectedOrdinal)))
  unless (selected == [groups !! 1]) $
    ioError (userError "demand selection renumbered an original STG group")
  let produceGroups =
        [ group | group <- groups
        , any ((== "produce") . Schema.symbolOccurrence) (projectedBinders group) ]
      otherBinders = Set.fromList
        [ binder | group <- groups
        , not (any ((== "produce") . Schema.symbolOccurrence) (projectedBinders group))
        , binder <- projectedBinders group ]
  unless (case produceGroups of
      [group] -> any ((`Set.member` otherBinders) . Schema.globalIdentity)
        (projectedGlobals (projectedBody group))
      _ -> False) $
    ioError (userError ("cross-group dependency was not projected as a global: "
      ++ show [ (map Schema.symbolOccurrence (projectedBinders group),
                   map (Schema.symbolOccurrence . Schema.globalIdentity)
                     (projectedGlobals (projectedBody group)))
              | group <- groups ]))
  iface <- case Map.lookup name (pprProductInterfaces result) of
    Just value -> pure value
    Nothing -> ioError (userError "prepared product omitted ModuleProductA interface")
  let sourceSummaries = [summary | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph producer)
                                 , moduleNameString (moduleName (ms_mod summary)) == "ModuleProductA"]
  case sourceSummaries of
    [summary] -> do
      unless ("Prelude" `elem` map (moduleNameString . unLoc . snd) (ms_textual_imps summary)) $
        ioError (userError "GHC summary omitted implicit Prelude import")
      let moduleEnvironment = scopeRetainedSummaryHscEnv summary producer
      flagHash <- fingerprintDynFlags moduleEnvironment (ms_mod summary) putNameLiterally
      optHash <- fingerprintOptFlags (hsc_dflags moduleEnvironment) putNameLiterally
      unless (flagHash == mi_flag_hash (mi_final_exts iface)
        && optHash == mi_opt_hash (mi_final_exts iface)) $
        ioError (userError "stored interface options differ from producing GHC flags")
      checked <- checkOldIface moduleEnvironment summary (Just iface)
      unless (case checked of UpToDateItem _ -> True; OutOfDateItem _ _ -> False) $
        ioError (userError "scoped recompilation rejected freshly produced interface")
    _ -> ioError (userError "source summary absent for interface flag probe")
  unless (Map.member (mkModuleName "ModuleProductB") (pprProductInterfaces result)) $
    ioError (userError "product mode omitted the leaf module interface")
  skinny <- case lookupHpt (hsc_HPT producer) name of
    Just hmi -> pure (hm_iface hmi)
    Nothing -> ioError (userError "prepared HPT omitted ModuleProductA interface")
  unless (case mi_extra_decls iface of Nothing -> True; _ -> False) $
    ioError (userError "home product interface retained defining Core")
  unless (case mi_extra_decls skinny of Nothing -> True; _ -> False) $
    ioError (userError "prepared HPT retained defining Core")
  writeBinIface (targetProfile (hsc_dflags producer)) QuietBinIFace
    NormalCompression hi iface
  productSize <- getFileSize hi
  unless (productSize > 0) $
    ioError (userError "module product interface was empty")
  digest <- SHA256.hash <$> BS.readFile hi
  copyFile hi tamperedHi
  BS.appendFile tamperedHi (BS.singleton 0)
  tampered <- SHA256.hash <$> BS.readFile tamperedHi
  unless (tampered /= digest) $
    ioError (userError "paired interface digest did not change with bytes")
  let hex = concatMap (\byte -> let s = showHex byte "" in replicate (2 - length s) '0' ++ s)
      manifest = work </> "module-candidates.cbor"
  -- Capture the cold result once at its original authored target. The existing
  -- Rust owner certifies these originals and publishes A's candidate; no
  -- structural packet or fabricated package descriptor grants reuse authority.
  capture <- captureCompilerFixture (FixtureCompilerInput work b [work]) result
  writeGenuineCandidateManifestFor ["ModuleProductA"] work capture
  hydratedCompile <- runPipelineSelected (PreparedProducts (Just manifest)) b [work]
  unless (map candidateModule (pprAcceptedCandidates hydratedCompile) == ["ModuleProductA"]
    && all ((/= "ModuleProductA") . moduleNameString . moduleName . pmModule)
       (pprModules hydratedCompile)) $
    ioError (userError "same-transaction candidate was not reused without source preparation")
  originalA <- BS.readFile a
  BS.appendFile a "\n-- changed source invalidates candidate\n"
  changedCompile <- runPipelineSelected (PreparedProducts (Just manifest)) b [work]
  unless (null (pprAcceptedCandidates changedCompile)
    && any ((== "ModuleProductA") . moduleNameString . moduleName . pmModule)
       (pprModules changedCompile)) $
    ioError (userError "changed candidate source did not compile afresh")
  BS.writeFile a originalA
  renameFile a (work </> "ModuleProductA.hidden")

  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags
      { importPaths = [work], backend = noBackend, ghcLink = NoLink }
    fresh <- liftIO . freshExactState =<< getSession
    let artifact = ExactIfaceArtifact
          (unitString (moduleUnit (pmModule moduleA)))
          "ModuleProductA" hi (concatMap (\byte ->
            let s = showHex byte "" in replicate (2 - length s) '0' ++ s)
            (BS.unpack digest)) []
    -- Metadata refusal precedes every path read, including for duplicate
    -- module names whose advertised units differ.
    let unreadable = artifact { exactPath = error "preflight attempted an interface read" }
        otherUnit = unreadable { exactUnit = "other-unit" }
    duplicate <- liftIO $ readExactIfaceArtifacts fresh [unreadable, otherUnit]
    unless (case duplicate of Left "duplicate exact interface owner" -> True; _ -> False) $
      liftIO $ ioError (userError "same module name in distinct units was not refused before reading")
    incomplete <- liftIO $ readExactIfaceArtifacts fresh
      [unreadable { exactRequirements = [("other-unit", "ModuleProductA")] }]
    unless (case incomplete of Left "incomplete exact interface dependency closure" -> True; _ -> False) $
      liftIO $ ioError (userError "dependency closure ignored its exact unit or read before preflight")
    readResult <- liftIO $ readExactIfaceArtifacts fresh [artifact]
    loaded <- case readResult of
      Right [item] -> pure item
      Left reason -> liftIO $ ioError (userError reason)
      _ -> liftIO $ ioError (userError "exact hydration did not return one module")
    tamperedResult <- liftIO $ readExactIfaceArtifacts fresh
      [artifact { exactPath = tamperedHi }]
    unless (case tamperedResult of Left _ -> True; Right _ -> False) $
      liftIO $ ioError (userError "tampered exact interface was admitted")
    leafIface <- case Map.lookup (mkModuleName "ModuleProductB") (pprProductInterfaces result) of
      Just value -> pure value
      Nothing -> liftIO $ ioError (userError "missing leaf interface for ordered hydration")
    let leafPath = work </> "ModuleProductB.exact.hi"
    liftIO $ writeBinIface (targetProfile (hsc_dflags producer)) QuietBinIFace NormalCompression leafPath leafIface
    leafHash <- liftIO $ SHA256.hash <$> BS.readFile leafPath
    let leafArtifact = artifact
          { exactModule = "ModuleProductB", exactPath = leafPath
          , exactSha256 = hex (BS.unpack leafHash)
          , exactRequirements = [(exactUnit artifact, exactModule artifact)] }
    ordered <- liftIO $ readExactIfaceArtifacts fresh [leafArtifact, artifact]
    interfaces <- case ordered of
      Right values | map (exactModule . fst) values == ["ModuleProductB", "ModuleProductA"] -> pure values
      Left reason -> liftIO $ ioError (userError reason)
      _ -> liftIO $ ioError (userError "exact interface indexing reordered original inputs")
    corruptMember <- liftIO $ readExactIfaceArtifacts fresh
      [leafArtifact, artifact { exactPath = tamperedHi }]
    unless (case corruptMember of Left _ -> True; Right _ -> False) $
      liftIO $ ioError (userError "corrupt member of an exact graph was admitted")
    unless (all (\owner -> case lookupHpt (hsc_HPT fresh) (mkModuleName owner) of
        Nothing -> True; Just _ -> False) ["ModuleProductA", "ModuleProductB"]) $
      liftIO $ ioError (userError "failed exact graph partially installed its interfaces")
    let restored = snd loaded
    unless (case mi_extra_decls restored of Nothing -> True; _ -> False) $
      liftIO $ ioError (userError "serialized home product retained defining Core")
    hydrated <- liftIO $ hydrateExactScope fresh interfaces
    let sourceGraph = mkModuleGraph
          [node | node@(ModuleNode _ summary) <- mgModSummaries' (hsc_mod_graph producer)
                , moduleNameString (moduleName (ms_mod summary)) == "ModuleProductB"]
    duplicateLexical <- liftIO $ installExactLexicalGraph sourceGraph [(artifact, []), (otherUnit, [])] noCheckedValueImports hydrated
    unless (case duplicateLexical of Left "duplicate virtual lexical owner" -> True; _ -> False) $
      liftIO $ ioError (userError "virtual lexical module names were not unique across units")
    hidden <- liftIO $ installExactLexicalGraph sourceGraph [] noCheckedValueImports hydrated
    unless (case hidden of Left _ -> True; Right _ -> False) $
      liftIO $ ioError (userError "implementation-only module became lexically importable")
    lexicalResult <- liftIO $ installExactLexicalGraph sourceGraph [(artifact, [])] noCheckedValueImports hydrated
    lexical <- case lexicalResult of
      Right env -> pure env
      Left reason -> liftIO $ ioError (userError reason)
    setSession lexical
    summary <- case consumerSummaries of
      [value] -> pure value
      _ -> liftIO $ ioError (userError "source-less consumer summary absent")
    parsed <- parseModule summary
    _ <- typecheckModule parsed
    pure ()

verifyOriginalProductCatalogue :: FilePath -> IO ()
verifyOriginalProductCatalogue work = do
  let directory = work </> "original-product-catalogue"
      a = directory </> "ModuleProductCatalogA.hs"
      b = directory </> "ModuleProductCatalogB.hs"
      replyInternal = directory </> "Tidepool" </> "Agent" </> "Reply" </> "Internal.hs"
  createDirectoryIfMissing True directory
  -- Only unit/module/groups enter the pure cached-closure walk below. This
  -- structural codec input makes no candidate admission or certification claim.
  manifest <- writeCandidateCodecFixture directory EmptyCandidateInventory
  candidateResult <- readModuleCandidates manifest
  baseCandidate <- case candidateResult of
    Right [candidate] -> pure candidate
    Left reason -> ioError (userError reason)
    _ -> ioError (userError "candidate template did not decode to one module")
  copyFile "test-prepared-stg/ModuleProductCatalogA.hs" a
  copyFile "test-prepared-stg/ModuleProductCatalogB.hs" b
  createDirectoryIfMissing True (takeDirectory replyInternal)
  copyFile "test-prepared-stg/ReplyInternalFixture.hs" replyInternal
  result <- runPipelineSelected (PreparedProducts Nothing) b [directory]
  moduleA <- case [prepared | prepared <- pprModules result
      , moduleNameString (moduleName (pmModule prepared)) == "ModuleProductCatalogA"] of
    [prepared] -> pure prepared
    _ -> ioError (userError "catalogue pipeline omitted its defining module")
  moduleB <- case [prepared | prepared <- pprModules result
      , moduleNameString (moduleName (pmModule prepared)) == "ModuleProductCatalogB"] of
    [prepared] -> pure prepared
    _ -> ioError (userError "catalogue pipeline omitted its target module")
  let context = ProjectionContext
        { projectionProfile = "ghc-9.12-prepared-stg"
        , projectionToolchain = "ghc-9.12.2"
        , projectionTarget = Schema.TargetDescriptor Schema.X86_64
            Schema.LittleEndian 64 64 "sysv64" []
        , projectionRetainedGenerations = mempty
        , projectionCurrentOriginals = mempty
        , projectionEntry = Schema.SymbolIdentity "main" "ModuleProductCatalogB"
            "value" "consumeSafe" Nothing
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTimeAuthority = Nothing
        , projectionJsonAuthority = Nothing
        , projectionTextUnit = Nothing
        }
      contextFor modul occurrence = context
        { projectionEntry = Schema.SymbolIdentity
            (T.pack (unitString (moduleUnit (pmModule moduleA))))
            modul "value" occurrence Nothing
        }
      selectedProgram modules modul occurrence = do
        (program, _) <- prepareProjection (contextFor modul occurrence) modules
          >>= projectSelected
        pure program
      products = projectOriginalHomeModuleProducts (prHscEnv (pprPipelineResult result))
        (pprProductInterfaces result) context mempty (pprModules result)
      fresh =
        [ (unitString (moduleUnit owner), moduleNameString (moduleName owner),
            either (Left . show) Right groups)
        | (owner, groups) <- preparedModuleProductOutcomes products ]
      productA = case [groups | (owner, Right groups) <- preparedModuleProductOutcomes products
          , moduleNameString (moduleName owner) == "ModuleProductCatalogA"] of
        [groups] -> groups
        _ -> []
      freshClosure program = requiredOriginalPackageGlobalsWithRetained
        fresh [] [] Set.empty (Schema.programGlobals program)
      candidateGroup group = CandidateGroup
        (fromIntegral (projectedOriginalOrdinal group))
        (projectedBinders group)
        [ CandidateGlobal
            { candidateGlobalIdentity = Schema.globalIdentity global
            , candidateGlobalRep = Schema.globalRep global
            , candidateGlobalSignature = Schema.globalEntrySignature global >>= \(Schema.SignatureId index) ->
                listToMaybe (drop (fromIntegral index) (projectedSignatures body))
            , candidateGlobalEvaluated = Schema.globalRequiredEvaluated global
            , candidateGlobalGeneration = fromIntegral <$> Schema.globalRequiredGeneration global
            }
        | global <- projectedGlobals body ]
        where body = projectedBody group
      cachedProduct = baseCandidate
        { candidateUnit = unitString (moduleUnit (pmModule moduleA))
        , candidateModule = "ModuleProductCatalogA"
        , candidateGroups = map candidateGroup productA
        }
      assertRejected label modul occurrence =
        case selectedProgram (pprModules result) modul occurrence of
          Left (RejectedTypedSite _) -> pure ()
          other -> ioError (userError (label ++ " was not rejected: " ++ show other))
  safe <- either (ioError . userError . ("safe target projection failed: " ++) . show)
    pure (selectedProgram (pprModules result) "ModuleProductCatalogB" "consumeSafe")
  case freshClosure safe of
    Right _ -> pure ()
    Left reason -> ioError (userError ("fresh safe original product closure failed: " ++ reason))
  originalInterfaces <- newOriginalInterfaceArtifacts (prHscEnv (pprPipelineResult result))
    (pprFinalizedModules result) [] directory
  (originalModules, originalContext) <- Products.prepareOriginalProducts
    (prHscEnv (pprPipelineResult result)) Nothing (pprProductInterfaces result)
    context Set.empty (pprModules result)
  admitted <- Products.admitCurrentOriginalProducts originalInterfaces directory result originalContext
  inventory <- maybe (ioError (userError "catalogue did not issue its current original inventory")) pure
    (Products.preparedCurrentOriginalInventory admitted)
  let targetContext = contextFor "ModuleProductCatalogB" "consumeSafe"
      currentContext = targetContext { projectionCurrentOriginals =
        Products.currentOriginalBindingsExcept inventory (Set.singleton (projectionEntry targetContext)) }
  current <- either (ioError . userError . show) (pure . fst)
    (prepareProjection currentContext originalModules >>= projectSelected)
  let defined = Set.fromList
        [identity | group <- Schema.programBindings current
          , Schema.TopBinding identity _ <- case group of
              Schema.NonRecursive binding -> [binding]
              Schema.Recursive bindings -> bindings]
      currentSafe = [Schema.globalIdentity global | global <- Schema.programGlobals current
        , Schema.symbolModule (Schema.globalIdentity global) == "ModuleProductCatalogA"
        , Schema.symbolOccurrence (Schema.globalIdentity global) == "safeValue"]
  unless (not (Map.null (projectionCurrentOriginals currentContext))
      && projectionEntry currentContext `Set.member` defined
      && length currentSafe == 1 && all (`Set.notMember` defined) currentSafe) $
    ioError (userError "target projection did not reference its compiler-admitted current original")
  assertRejected "direct raw progress helper" "ModuleProductCatalogA" "rawProgress"
  assertRejected "helper depending on raw progress" "ModuleProductCatalogA" "dependentProgress"
  assertRejected "transitive target depending on raw progress"
    "ModuleProductCatalogB" "consumeDependentProgress"
  case productA of
    groups@(_ : _) -> do
      let groupNames = map (map Schema.symbolOccurrence . projectedBinders) groups
          safePresent = any (elem "safeValue") groupNames
          invalidPresent = any (any (`elem` ["rawProgress", "dependentProgress"])) groupNames
          sourceGroups = Set.fromList
            [ (fromIntegral index :: Word32,
                Set.fromList (map (T.pack . occNameString . nameOccName . idName)
                  (topBinders binding)))
            | (index, (binding, _)) <- zip [0 :: Int ..] (pmBindings moduleA) ]
          retainedGroups =
            [ (projectedOriginalOrdinal group, Set.fromList (map Schema.symbolOccurrence
                (projectedBinders group))) | group <- groups ]
          -- GHC may replace the source names with worker names. Multiple
          -- binders in one original group still prove the recursive pair.
          hasRecursiveGroup = any ((> 1) . length . projectedBinders) groups
      unless (safePresent && not invalidPresent && hasRecursiveGroup
          && all (`Set.member` sourceGroups) retainedGroups) $
        ioError (userError ("fresh catalogue has missing/invalid group evidence: "
          ++ show (safePresent, invalidPresent, hasRecursiveGroup, retainedGroups)))
      unless (length groups < length (pmBindings moduleA)) $
        ioError (userError "fresh catalogue did not omit rejected and dependent groups")
    _ -> ioError (userError "fresh catalogue omitted eligible defining module")
  cachedSafe <- either
    (ioError . userError . ("cached safe target projection failed: " ++) . show)
    pure (selectedProgram [moduleB] "ModuleProductCatalogB" "consumeSafe")
  case requiredOriginalPackageGlobalsWithRetained [] [cachedProduct] [] Set.empty
      (Schema.programGlobals cachedSafe) of
    Right _ -> pure ()
    Left reason -> ioError (userError ("cached safe product closure failed: " ++ reason))
  cachedDependent <- either
    (ioError . userError . ("cached dependent target projection failed: " ++) . show)
    pure (selectedProgram [moduleB] "ModuleProductCatalogB" "consumeDependentProgress")
  case requiredOriginalPackageGlobalsWithRetained [] [cachedProduct] [] Set.empty
      (Schema.programGlobals cachedDependent) of
    Left _ -> pure ()
    Right _ -> ioError (userError "cached original closure included its omitted dependent helper")
  let safeIdentity = Schema.SymbolIdentity
        (T.pack (unitString (moduleUnit (pmModule moduleA))))
        "ModuleProductCatalogA" "value" "safeValue" Nothing
      cachedWithoutSafeValue = cachedProduct
        { candidateGroups = filter (not . any ((== "safeValue") . Schema.symbolOccurrence)
            . candidateGroupBinders) (candidateGroups cachedProduct) }
  case requiredOriginalPackageGlobalsWithRetained [] [cachedWithoutSafeValue] []
      (Set.singleton safeIdentity) (Schema.programGlobals cachedSafe) of
    Right _ -> pure ()
    Left reason -> ioError (userError ("retained generation did not close its imported original: " ++ reason))
