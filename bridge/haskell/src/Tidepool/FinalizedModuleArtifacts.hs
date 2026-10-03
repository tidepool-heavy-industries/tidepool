{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE LambdaCase #-}

-- | Captured finalization files and their exact dependency seals. The receipt
-- names these bounded files; it never duplicates their payloads inline.
module Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, captureFinalizedModuleArtifacts
  , emptyFinalizedModuleArtifacts, encodeFinalizedModuleArtifacts, finalizedInterfaceSeals ) where

import Codec.CBOR.Encoding
import Control.Exception (Exception, throwIO)
import Control.Monad (forM, unless, when)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.List (find, sortOn)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC.Driver.Env (HscEnv, hsc_all_home_unit_ids)
import GHC.Unit.Home.ModInfo (HomeModInfo(..))
import GHC.Unit.Module (ModuleName, moduleName, moduleUnit, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.ModIface (mi_module)
import GHC.Unit.Types (unitIdString, unitString, stringToUnit)
import Numeric (showHex)
import System.FilePath ((</>))

import Tidepool.DependencyEvidence (DependencyEvidence(..), DependencyModule(..), DependencySource(..))
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, ExactIfaceArtifact(..), originalInterfaceBytes, originalInterfaceSha256 )
import Tidepool.FinalizedModule (FinalizedModule(..), homeInterfaceUsageOwners)
import Tidepool.FinalizedCore (captureFinalizedCore, isUnsupportedFinalizedCore)
import Tidepool.PackageWitness (PackageImportEvidence, encodePackageImports)

-- The producer's strict profile limits match the owning Rust receipt decoder.
interfaceLimit, packageLimit, coreLimit, payloadLimit :: Int
interfaceLimit = 32 * 1024 * 1024
packageLimit = 4 * 1024 * 1024
coreLimit = 32 * 1024 * 1024
payloadLimit = 128 * 1024 * 1024

data FinalizedArtifactFailure
  = MissingFinalizedSource String String
  | MissingFinalizedInterface String String
  | MissingFinalizedPackages String String
  | MissingFinalizedDependency String String
  | FinalizedPayloadTooLarge String
  | FinalizedInventoryTooLarge
  deriving Show
instance Exception FinalizedArtifactFailure

data CapturedPayload = CapturedPayload FilePath T.Text Int

data CapturedModule = CapturedModule
  T.Text T.Text T.Text CapturedPayload CapturedPayload (Maybe CapturedPayload)
  [(T.Text,T.Text,T.Text)]

data FinalizedModuleArtifacts = FinalizedModuleArtifacts [T.Text] [CapturedModule]

-- Owner-free targets still carry the complete compiler home-unit census.
emptyFinalizedModuleArtifacts :: HscEnv -> FinalizedModuleArtifacts
emptyFinalizedModuleArtifacts env = FinalizedModuleArtifacts
  (map (T.pack . unitIdString) (Set.toAscList (hsc_all_home_unit_ids env))) []

captureFinalizedModuleArtifacts
  :: OriginalInterfaceArtifacts -> HscEnv -> Map.Map ModuleName FinalizedModule
  -> Map.Map ModuleName PackageImportEvidence -> DependencyEvidence -> FilePath
  -> IO FinalizedModuleArtifacts
captureFinalizedModuleArtifacts originals env finalized packages evidence directory = do
  let homeUnits = map (T.pack . unitIdString) (Set.toAscList (hsc_all_home_unit_ids env))
  when (length homeUnits > 128 || Map.size finalized > 128) $ throwIO FinalizedInventoryTooLarge
  rows <- forM (sortOn originalKey (Map.elems finalized)) $ \original -> do
    let iface = hm_iface (finalizedHomeModInfo original)
        owner = mi_module iface
        unit = unitString (moduleUnit owner)
        name = moduleNameString (moduleName owner)
    source <- maybe (throwIO (MissingFinalizedSource unit name)) pure (consumedSource unit name)
    interfaceBytes <- originalInterfaceBytes originals owner
      >>= maybe (throwIO (MissingFinalizedInterface unit name)) pure
    roots <- maybe (throwIO (MissingFinalizedPackages unit name)) pure (Map.lookup (moduleName owner) packages)
    let interfaceSHA = digest interfaceBytes
        sidecar = encodePackageImports (ExactIfaceArtifact unit name "" (T.unpack interfaceSHA) []) roots
    interface <- capture "hi" interfaceLimit interfaceBytes
    package <- capture "packages.cbor" packageLimit sidecar
    core <- captureFinalizedCore env original directory >>= \case
      Right bytes -> Just <$> capture "core" coreLimit bytes
      Left refusal | isUnsupportedFinalizedCore refusal -> pure Nothing
                   | otherwise -> throwIO refusal
    requirements <- forM (homeInterfaceUsageOwners env iface) $ \(requiredUnit,requiredName) -> do
      unless (T.pack requiredUnit `elem` homeUnits) $
        throwIO (MissingFinalizedDependency requiredUnit requiredName)
      let requiredOwner = mkModule (stringToUnit requiredUnit)
            (mkModuleName requiredName)
      sha <- originalInterfaceSha256 originals requiredOwner
        >>= maybe (throwIO (MissingFinalizedDependency requiredUnit requiredName)) pure
      pure (T.pack requiredUnit,T.pack requiredName,T.pack sha)
    pure (CapturedModule (T.pack unit) (T.pack name) source interface package core requirements)
  let total = sum [size iface + size packages' + maybe 0 size core
        | CapturedModule _ _ _ iface packages' core _ <- rows]
  when (total > payloadLimit) $ throwIO (FinalizedPayloadTooLarge "aggregate")
  pure (FinalizedModuleArtifacts homeUnits rows)
  where
    originalKey original = let owner = mi_module (hm_iface (finalizedHomeModInfo original))
      in (unitString (moduleUnit owner),moduleNameString (moduleName owner))
    consumedSource unit name = do
      node <- find (\node -> dependencyModuleUnit node == unit
        && dependencyModuleName node == name && not (dependencyModuleBoot node)) (dependencyModules evidence)
      source <- find ((== dependencyModuleSource node) . dependencySourcePath) (dependencySources evidence)
      pure (T.pack (dependencySourceSha256 source))
    capture suffix limit bytes = do
      when (BS.null bytes || BS.length bytes > limit) $ throwIO (FinalizedPayloadTooLarge suffix)
      let sha = digest bytes
          relative = T.unpack sha ++ ".finalized." ++ suffix
      BS.writeFile (directory </> relative) bytes
      pure (CapturedPayload relative sha (BS.length bytes))
    size (CapturedPayload _ _ count) = count

finalizedInterfaceSeals :: FinalizedModuleArtifacts -> [((T.Text,T.Text),T.Text)]
finalizedInterfaceSeals (FinalizedModuleArtifacts _ rows) =
  [((unit,name),sha) | CapturedModule unit name _ (CapturedPayload _ sha _) _ _ _ <- rows]

encodeFinalizedModuleArtifacts :: FinalizedModuleArtifacts -> Encoding
encodeFinalizedModuleArtifacts (FinalizedModuleArtifacts units rows) =
  array [encodeString "tidepool-ghc-finalized-module-v1",list encodeString units,list encodeModule rows]
  where
    encodeModule (CapturedModule unit name source interface package core requirements) =
      array ([encodeString unit,encodeString name,encodeString source]
        ++ payloadFields interface ++ payloadFields package
        ++ [maybe encodeNull (array . payloadFields) core,list encodeRequirement requirements])
    encodeRequirement (unit,name,sha) = array (map encodeString [unit,name,sha])
    payloadFields (CapturedPayload path sha count) =
      [encodeString (T.pack path),encodeString sha,encodeWord64 (fromIntegral count)]

digest :: BS.ByteString -> T.Text
digest = T.pack . concatMap byteHex . BS.unpack . SHA256.hash
  where byteHex byte = let rendered = showHex byte "" in replicate (2-length rendered) '0' ++ rendered

array :: [Encoding] -> Encoding
array values = encodeListLen (fromIntegral (length values)) <> foldMap id values
list :: (a -> Encoding) -> [a] -> Encoding
list encode values = encodeListLen (fromIntegral (length values)) <> foldMap encode values
