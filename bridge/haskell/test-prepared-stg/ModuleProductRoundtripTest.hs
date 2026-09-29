{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}

module ModuleProductRoundtripTest (verifyModuleProductInterfaceRoundtrip) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA256
import Codec.CBOR.Encoding (encodeBool, encodeListLen, encodeString)
import Codec.CBOR.Write (toLazyByteString)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
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
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module (mkModuleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries', mkModuleGraph)
import GHC.Unit.Module.ModIface (mi_extra_decls, mi_final_exts, mi_flag_hash, mi_opt_hash)
import GHC.Unit.Types (moduleName, unitString)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import System.Directory (copyFile, getFileSize, renameFile)
import System.FilePath ((</>))
import Numeric (showHex)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , projectPreparedModuleGroups, projectPreparedModuleGroupsSelected, topBinders )
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), freshExactState, readExactIfaceArtifacts, hydrateExactScope
  , installExactLexicalGraph )
import Tidepool.GhcPipeline
  ( PipelineResult(..), PipelineSelection(..), PreparedPipelineResult(..), runPipelineSelected )
import Tidepool.PreparedSites (SiteRejection(..))
import Tidepool.PreparedStg (PreparedModule(..))
import Tidepool.RetainedUnfoldings (scopeRetainedHscEnv)
import Tidepool.DependencyEvidence (DependencySource(..), sourceEvidence)
import Tidepool.ModuleCandidates (ModuleCandidate(..))

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
      { pmSiteRejections = SiteRejection binder "rejected group site"
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
      flagHash <- fingerprintDynFlags producer (ms_mod summary) putNameLiterally
      optHash <- fingerprintOptFlags (hsc_dflags producer) putNameLiterally
      unless (flagHash == mi_flag_hash (mi_final_exts iface)
        && optHash == mi_opt_hash (mi_final_exts iface)) $
        ioError (userError "stored interface options differ from producing GHC flags")
      checked <- checkOldIface (scopeRetainedHscEnv (ms_mod summary) producer) summary (Just iface)
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
  originalSource <- sourceEvidence a
  let hex = concatMap (\byte -> let s = showHex byte "" in replicate (2 - length s) '0' ++ s)
      manifest = work </> "module-candidates.cbor"
      candidate = encodeListLen 11
        <> encodeString (T.pack (unitString (moduleUnit (pmModule moduleA))))
        <> encodeString "ModuleProductA"
        <> encodeString (T.pack a)
        <> encodeString (T.pack (dependencySourceSha256 originalSource))
        <> encodeString (T.pack hi)
        <> encodeString (T.pack (hex (BS.unpack digest)))
        <> encodeString (T.replicate 64 "0")
        <> encodeString (T.replicate 64 "0")
        <> encodeString (T.replicate 64 "0")
        <> encodeListLen 1
        <> encodeListLen 4 <> encodeString "none" <> encodeString "Prelude"
        <> encodeBool False <> encodeString ""
        <> encodeListLen 0
      manifestBytes = toLazyByteString
        (encodeListLen 3 <> encodeString "TPMCAN" <> encodeString "4"
          <> encodeListLen 1 <> candidate)
  BS.writeFile manifest (BL.toStrict manifestBytes)
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
    readResult <- liftIO $ readExactIfaceArtifacts fresh [artifact]
    loaded <- case readResult of
      Right [item] -> pure item
      Left reason -> liftIO $ ioError (userError reason)
      _ -> liftIO $ ioError (userError "exact hydration did not return one module")
    tamperedResult <- liftIO $ readExactIfaceArtifacts fresh
      [artifact { exactPath = tamperedHi }]
    unless (case tamperedResult of Left _ -> True; Right _ -> False) $
      liftIO $ ioError (userError "tampered exact interface was admitted")
    let restored = snd loaded
    unless (case mi_extra_decls restored of Nothing -> True; _ -> False) $
      liftIO $ ioError (userError "serialized home product retained defining Core")
    hydrated <- liftIO $ hydrateExactScope fresh [loaded]
    let sourceGraph = mkModuleGraph
          [node | node@(ModuleNode _ summary) <- mgModSummaries' (hsc_mod_graph producer)
                , moduleNameString (moduleName (ms_mod summary)) == "ModuleProductB"]
    hidden <- liftIO $ installExactLexicalGraph sourceGraph [] hydrated
    unless (case hidden of Left _ -> True; Right _ -> False) $
      liftIO $ ioError (userError "implementation-only module became lexically importable")
    lexicalResult <- liftIO $ installExactLexicalGraph sourceGraph [(artifact, [])] hydrated
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
