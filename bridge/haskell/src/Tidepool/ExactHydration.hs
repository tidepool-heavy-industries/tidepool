{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TypeApplications #-}

module Tidepool.ExactHydration
  ( ExactIfaceArtifact(..)
  , freshExactState
  , readExactIfaceArtifacts
  , hydrateExactScope
  , installExactLexicalGraph
  ) where

import Control.Monad (forM, forM_, unless)
import Control.Exception
  ( IOException, SomeException, SomeAsyncException, bracket, try, fromException, throwIO )
import Data.Char (isHexDigit, toLower)
import Data.Function (on)
import Data.List (nubBy)
import Data.Maybe (isJust)
import qualified Data.ByteString as BS
import qualified Crypto.Hash.SHA256 as SHA256
import GHC.Driver.Env
  ( HscEnv(..), hscUpdateHPT_lazy, hsc_home_unit, hsc_HPT )
import GHC.Unit.Env (UnitEnv(..), HomeUnitEnv(..))
import GHC.Unit.External (initExternalUnitCache)
import GHC.Unit.Finder (initFinderCache)
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Driver.Env.KnotVars (emptyKnotVars)
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), emptyHomeModInfoLinkable, emptyHomePackageTable, addToHpt
  , lookupHpt )
import GHC.Iface.Load (readIface)
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Unit.Module (moduleName, moduleUnit, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.Graph
  ( ModuleGraph, ModuleGraphNode(..), NodeKey(..), ModNodeKeyWithUid(..)
  , mgModSummaries', mkModuleGraph )
import GHC.Unit.Module.Location
  ( pattern ModLocation
  , ml_hs_file, ml_hi_file, ml_dyn_hi_file, ml_obj_file, ml_dyn_obj_file, ml_hie_file )
import GHC.Unit.Module.ModSummary (ModSummary(..))
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.PkgQual (PkgQual(..))
import GHC.Types.SrcLoc (unLoc)
import GHC.Unit.Home (homeUnitId)
import GHC.Unit.Types (GenWithIsBoot(..))
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Utils.Fingerprint (fingerprintByteString)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_extra_decls)
import GHC.Unit.Types (unitString, stringToUnit)
import qualified GHC.Data.Maybe as MErr
import GHC.Utils.Outputable (text)
import Numeric (showHex)
import System.Directory (getTemporaryDirectory, removeFile)
import System.IO (fixIO)
import System.IO (hClose, hIsClosed, openBinaryTempFile)
import qualified Data.Set as Set

-- An artifact is advisory until its bytes and GHC module identity have been
-- checked in the transaction that consumes it. Requirements cover the exact
-- implementation closure and are checked by the caller before hydration.
data ExactIfaceArtifact = ExactIfaceArtifact
  { exactUnit :: String
  , exactModule :: String
  , exactPath :: FilePath
  , exactSha256 :: String
  , exactRequirements :: [(String, String)]
  } deriving (Eq, Show)

-- The resident GHC process survives requests, but its mutable package and
-- home interface tables must not carry visibility across lexical scopes.
freshExactState :: HscEnv -> IO HscEnv
freshExactState env = do
  eps <- initExternalUnitCache
  finder <- initFinderCache
  let units = hsc_unit_env env
      homes = fmap (\home -> home { homeUnitEnv_hpt = emptyHomePackageTable })
        (ue_home_unit_graph units)
  pure env
    { hsc_FC = finder
    , hsc_targets = []
    , hsc_mod_graph = mkModuleGraph []
    , hsc_type_env_vars = emptyKnotVars
    , hsc_unit_env = units { ue_eps = eps, ue_home_unit_graph = homes }
    }

-- Reading every interface before installing any of them prevents a corrupt
-- member of an implementation SCC from partially mutating the HPT.
readExactIfaceArtifacts
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String [(ExactIfaceArtifact, ModIface)])
readExactIfaceArtifacts env artifacts
  | length (nubBy ((==) `on` exactModule) artifacts)
      /= length artifacts = pure (Left "duplicate exact interface owner")
  | any (\artifact -> length (exactSha256 artifact) /= 64
      || not (all isHexDigit (exactSha256 artifact))) artifacts =
      pure (Left "invalid exact interface digest")
  | any (\artifact -> any (`notElem` owners) (exactRequirements artifact)) artifacts =
      pure (Left "incomplete exact interface dependency closure")
  | otherwise = sequence <$> forM artifacts (readOne env)
  where owners = [(exactUnit artifact, exactModule artifact) | artifact <- artifacts]

readOne :: HscEnv -> ExactIfaceArtifact -> IO (Either String (ExactIfaceArtifact, ModIface))
readOne env artifact = do
  readResult <- try (BS.readFile (exactPath artifact)) :: IO (Either IOException BS.ByteString)
  case readResult of
    Left _ -> pure (Left ("interface unavailable: " ++ exactModule artifact))
    Right bytes -> readVerified bytes
  where
   readVerified bytes =
    if hexBytes (SHA256.hash bytes) /= map toLower (exactSha256 artifact)
    then pure (Left ("interface digest mismatch: " ++ exactModule artifact))
    else do
      -- Decode the exact bytes that passed the digest check. Reading the
      -- candidate path again would admit a different interface between the
      -- hash and GHC's decoder, even if a later revalidation observed the
      -- original bytes restored.
      let owner = mkModule (stringToUnit (exactUnit artifact))
            (mkModuleName (exactModule artifact))
      decoded <- try @SomeException (withCapturedIface bytes $ \path ->
        readIface (hsc_dflags env) (hsc_NC env) owner path)
      case decoded of
        Left failure -> case fromException failure :: Maybe SomeAsyncException of
          Just async -> throwIO async
          Nothing -> pure (Left ("interface read failed: " ++ exactModule artifact))
        Right result -> pure $ case result of
          MErr.Failed _ -> Left ("interface read failed: " ++ exactModule artifact)
          MErr.Succeeded iface
            | unitString (moduleUnit (mi_module iface)) /= exactUnit artifact
                || moduleNameString (moduleName (mi_module iface)) /= exactModule artifact ->
                Left ("interface owner mismatch: " ++ exactModule artifact)
            | isJust (mi_extra_decls iface) ->
                Left ("interface contains defining Core: " ++ exactModule artifact)
            | otherwise -> Right (artifact, iface)

withCapturedIface :: BS.ByteString -> (FilePath -> IO a) -> IO a
withCapturedIface bytes consume = do
  directory <- getTemporaryDirectory
  bracket (openBinaryTempFile directory "tidepool-exact-iface.hi")
    (\(path, handle) -> do
      closed <- hIsClosed handle
      unless closed (hClose handle)
      removeFile path)
    (\(path, handle) -> do
      BS.hPut handle bytes
      hClose handle
      consume path)

-- GHC's home-interface knot allows mutually recursive source/boot modules to
-- resolve each other's original Names while typechecking their details.
hydrateExactScope
  :: HscEnv -> [(ExactIfaceArtifact, ModIface)] -> IO HscEnv
hydrateExactScope env loaded = do
  details <- fixIO $ \recursiveDetails -> do
    let knotted = withDetails recursiveDetails
    forM loaded $ \(_, iface) ->
      initIfaceCheck (text "tidepool exact hydration") knotted (typecheckIface iface)
  pure (withDetails details)
  where
    withDetails details = hscUpdateHPT_lazy (\hpt -> foldr
      (\(index, (_, iface)) table -> addToHpt table (moduleName (mi_module iface))
        (HomeModInfo iface (details !! index) emptyHomeModInfoLinkable))
      hpt (zip [0..] loaded)) env

-- A lexical interface contributes its chosen instance/family environment to
-- GHC's graph traversal. Implementation-only HMIs remain installed but never
-- appear in this graph; their original Names can still resolve through HPT.
-- The caller supplies virtual-to-virtual edges, never implementation edges.
installExactLexicalGraph
  :: ModuleGraph -> [(ExactIfaceArtifact, [(String, String)])] -> HscEnv
  -> IO (Either String HscEnv)
installExactLexicalGraph sourceGraph lexical env
  | length (nubBy ((==) `on` (exactModule . fst)) lexical)
      /= length lexical = pure (Left "duplicate virtual lexical owner")
  | any (\(_, deps) -> any (`Set.notMember` owners) deps) lexical =
      pure (Left "virtual lexical edge leaves admitted graph")
  | any (\node -> case node of
      ModuleNode _ summary -> keyOf summary `Set.member` owners
      _ -> False) (mgModSummaries' sourceGraph) =
      pure (Left "virtual lexical owner collides with source graph")
  | any (\(artifact, _) -> case lookupHpt (hsc_HPT env)
        (mkModuleName (exactModule artifact)) of
      Nothing -> True
      Just hmi -> mi_module (hm_iface hmi) /=
        mkModule (stringToUnit (exactUnit artifact))
          (mkModuleName (exactModule artifact))) lexical =
      pure (Left "virtual lexical interface missing from exact HPT")
  | any unadmittedHomeEdge sourceNodes =
      pure (Left "source graph imports unadmitted home implementation")
  | otherwise = do
      forM_ lexical $ \(artifact, _) ->
        addHomeModuleToFinder (hsc_FC env) (hsc_home_unit env)
          (GWIB (mkModuleName (exactModule artifact)) NotBoot)
          (ms_location (virtualSummary env artifact))
      pure (Right env { hsc_mod_graph = mkModuleGraph (sourceNodes ++ virtualNodes) })
  where
    owners = Set.fromList
      [(exactUnit artifact, exactModule artifact) | (artifact, _) <- lexical]
    home = homeUnitId (hsc_home_unit env)
    known = Set.union owners (Set.fromList
      [keyOf summary | ModuleNode _ summary <- mgModSummaries' sourceGraph])
    -- Downsweep intentionally excludes admitted exact owners. It may therefore
    -- omit the corresponding edge; textual imports still cannot expose an
    -- implementation-only HPT entry outside the selected lexical graph.
    unadmittedHomeEdge (ModuleNode edges summary) = any missing edges
      || any hiddenImport (ms_textual_imps summary ++ ms_srcimps summary)
    unadmittedHomeEdge _ = False
    hiddenImport (qualifier, imported) =
      let name = unLoc imported
          local = case qualifier of
            NoPkgQual -> True
            ThisPkg unit -> unit == home
            OtherPkg _ -> False
      in local && case lookupHpt (hsc_HPT env) name of
        Nothing -> False
        Just hmi ->
          (unitString (moduleUnit (mi_module (hm_iface hmi))), moduleNameString name)
            `Set.notMember` known
    missing (NodeKey_Module (ModNodeKeyWithUid (GWIB name _) unit)) =
      unit == home && (unitString unit, moduleNameString name) `Set.notMember` known
    missing _ = False
    keyOf summary =
      (unitString (moduleUnit (ms_mod summary)), moduleNameString (moduleName (ms_mod summary)))
    nodeKey (_, moduleName') = NodeKey_Module
      (ModNodeKeyWithUid (GWIB (mkModuleName moduleName') NotBoot) home)
    sourceNodes = [case node of
      ModuleNode edges summary ->
        let imports =
              [ (unitString home, moduleNameString (unLoc imported))
              | (qualifier, imported) <- ms_textual_imps summary
              , case qualifier of
                  NoPkgQual -> True
                  ThisPkg unit -> unit == home
                  OtherPkg _ -> False
              ]
            added = [nodeKey owner | owner <- imports, owner `Set.member` owners]
        in ModuleNode (Set.toList (Set.fromList (edges ++ added))) summary
      other -> other
      | node <- mgModSummaries' sourceGraph]
    virtualNodes =
      [ ModuleNode (map nodeKey deps) (virtualSummary env artifact)
      | (artifact, deps) <- lexical ]

virtualSummary :: HscEnv -> ExactIfaceArtifact -> ModSummary
virtualSummary env artifact = ModSummary
  { ms_mod = mkModule (stringToUnit (exactUnit artifact))
      (mkModuleName (exactModule artifact))
  , ms_hsc_src = HsSrcFile
  , ms_location = location
  , ms_hs_hash = fingerprintByteString BS.empty
  , ms_obj_date = Nothing
  , ms_dyn_obj_date = Nothing
  , ms_iface_date = Nothing
  , ms_hie_date = Nothing
  , ms_srcimps = []
  , ms_textual_imps = []
  , ms_ghc_prim_import = False
  , ms_parsed_mod = Nothing
  , ms_hspp_file = exactPath artifact
  , ms_hspp_opts = hsc_dflags env
  , ms_hspp_buf = Nothing
  }
  where location = ModLocation
          { ml_hs_file = Nothing
          , ml_hi_file = exactPath artifact
          , ml_dyn_hi_file = exactPath artifact
          , ml_obj_file = exactPath artifact
          , ml_dyn_obj_file = exactPath artifact
          , ml_hie_file = exactPath artifact
          }

hexBytes :: BS.ByteString -> String
hexBytes = concatMap (\byte -> let s = showHex byte "" in replicate (2 - length s) '0' ++ s)
  . BS.unpack
