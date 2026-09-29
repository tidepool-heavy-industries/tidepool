{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}

module ModuleProductRoundtripTest (verifyModuleProductInterfaceRoundtrip) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC
  ( backend, getSession, getSessionDynFlags, ms_mod
  , noBackend
  , parseModule, runGhc, setSession, setSessionDynFlags, typecheckModule )
import GHC.Driver.Env
  ( hsc_FC, hsc_HPT, hsc_NC, hsc_dflags, hsc_home_unit, hsc_mod_graph, hscUpdateHPT )
import GHC.Driver.Session (GhcLink(..), ghcLink, importPaths, targetProfile)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Iface.Load (readIface)
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), addHomeModInfoToHpt, emptyHomeModInfoLinkable, lookupHpt )
import GHC.Unit.Module (mkModuleName, moduleNameString)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries')
import GHC.Unit.Module.Location
  ( ModLocation, pattern ModLocation
  , ml_dyn_hi_file, ml_dyn_obj_file, ml_hi_file, ml_hie_file, ml_hs_file, ml_obj_file )
import GHC.Unit.Module.ModIface (mi_extra_decls)
import GHC.Unit.Types (GenWithIsBoot(..), ModuleNameWithIsBoot, mkModule, moduleName)
import GHC.Utils.Outputable (text)
import qualified GHC.Data.Maybe as MErr
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import System.Directory (copyFile, getFileSize, renameFile)
import System.FilePath ((</>))
import System.IO (hPutStrLn, stderr)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , projectPreparedModuleGroups, projectPreparedModuleGroupsSelected, topBinders )
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.GhcPipeline
  ( PipelineResult(..), PipelineSelection(..), PreparedPipelineResult(..), runPipelineSelected )
import Tidepool.PreparedSites (SiteRejection(..))
import Tidepool.PreparedStg (PreparedModule(..))

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
  result <- runPipelineSelected PreparedProducts b [work]
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
  hPutStrLn stderr ("module-product-interface-bytes home=" ++ show productSize)
  digest <- SHA256.hash <$> BS.readFile hi
  copyFile hi tamperedHi
  BS.appendFile tamperedHi (BS.singleton 0)
  tampered <- SHA256.hash <$> BS.readFile tamperedHi
  unless (tampered /= digest) $
    ioError (userError "paired interface digest did not change with bytes")
  renameFile a (work </> "ModuleProductA.hidden")

  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags
      { importPaths = [work], backend = noBackend, ghcLink = NoLink }
    fresh <- getSession
    let owner = mkModule (homeUnitAsUnit (hsc_home_unit fresh)) name
    readResult <- liftIO $ readIface (hsc_dflags fresh) (hsc_NC fresh) owner hi
    restored <- case readResult of
      MErr.Failed _ -> liftIO $ ioError (userError "serialized product interface did not read")
      MErr.Succeeded value -> pure value
    unless (case mi_extra_decls restored of Nothing -> True; _ -> False) $
      liftIO $ ioError (userError "serialized home product retained defining Core")
    details <- liftIO $ initIfaceCheck (text "module product rehydration") fresh
      (typecheckIface restored)
    let hmi = HomeModInfo restored details emptyHomeModInfoLinkable
        location = ModLocation
          { ml_hs_file = Nothing, ml_hi_file = hi, ml_dyn_hi_file = hi
          , ml_obj_file = hi, ml_dyn_obj_file = hi, ml_hie_file = hi }
        finderName = GWIB name NotBoot :: ModuleNameWithIsBoot
    setSession ((hscUpdateHPT (addHomeModInfoToHpt hmi) fresh)
      { hsc_mod_graph = hsc_mod_graph producer })
    _ <- liftIO $ addHomeModuleToFinder (hsc_FC fresh) (hsc_home_unit fresh)
      finderName (location :: ModLocation)
    summary <- case consumerSummaries of
      [value] -> pure value
      _ -> liftIO $ ioError (userError "source-less consumer summary absent")
    parsed <- parseModule summary
    _ <- typecheckModule parsed
    pure ()
