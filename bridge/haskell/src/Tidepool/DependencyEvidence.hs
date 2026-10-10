-- | Versioned compiler dependency evidence written beside prepared artifacts.
-- The cache owner validates this document; the compiler owns producing it
-- because only the compiler knows which source and resolution paths it used.
module Tidepool.DependencyEvidence
  ( DependencyEvidence(..)
  , DependencySource(..)
  , DependencyResolution(..)
  , DependencyModule(..)
  , DependencyImport(..)
  , DependencyQualifier(..)
  , renderDependencyQualifier
  , parseDependencyQualifier
  , ProductAvailability(..)
  , sourceEvidence
  , sourceEvidenceWithFingerprint
  , revalidateDependencyEvidence
  , validateDependencyEvidence, writeDependencyEvidence
  , renderDependencyEvidence
  , selectedFreshHomeRequirements
  ) where

import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Data.List (intercalate, nub, sort, stripPrefix)
import Control.Monad (forM, unless)
import Numeric (showHex)
import System.FilePath ((</>))
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Utils.Fingerprint (fingerprintByteString)

import Tidepool.Json (jsonString)

data DependencyEvidence = DependencyEvidence
  { dependencyCacheSafe :: Bool
  , dependencySelectionComplete :: Bool
  , dependencySources :: [DependencySource]
  , dependencyResolutions :: [DependencyResolution]
  , dependencyPackages :: [String]
  , dependencyModules :: [DependencyModule]
  }

-- | The fresh-source direct graph that produced the paired module products. This is
-- post-downsweep evidence; it does not by itself authorize a future compile
-- to reuse a module without running its own downsweep and interface checks.
data DependencyModule = DependencyModule
  { dependencyModuleUnit :: String
  , dependencyModuleName :: String
  , dependencyModuleBoot :: Bool
  , dependencyModuleSource :: FilePath
  , dependencyModuleImports :: [DependencyImport]
  , dependencyModuleProduct :: ProductAvailability
  } deriving (Eq)

data ProductAvailability
  = ProductReady
  | ProductBoot
  | ProductInterfaceOnly
  | ProductMissingInterface
  | ProductProjectionRejected
  deriving (Eq, Show)

-- Constructor order preserves the ordering of the existing wire spellings.
data DependencyQualifier
  = DependencyUnqualified
  | DependencyOtherUnit String
  | DependencyThisUnit String
  deriving (Eq, Ord, Show)

-- | Encode only at a dependency-evidence or exact-compilation wire boundary.
renderDependencyQualifier :: DependencyQualifier -> String
renderDependencyQualifier DependencyUnqualified = "none"
renderDependencyQualifier (DependencyThisUnit unit) = "this:" ++ unit
renderDependencyQualifier (DependencyOtherUnit unit) = "other:" ++ unit

-- | Validate the versioned wire qualifier before admitting typed evidence.
parseDependencyQualifier :: String -> Maybe DependencyQualifier
parseDependencyQualifier "none" = Just DependencyUnqualified
parseDependencyQualifier value
  | Just unit <- stripPrefix "this:" value, not (null unit) = Just (DependencyThisUnit unit)
  | Just unit <- stripPrefix "other:" value, not (null unit) = Just (DependencyOtherUnit unit)
  | otherwise = Nothing

data DependencyImport = DependencyImport
  { dependencyImportQualifier :: DependencyQualifier
  , dependencyImportName :: String
  , dependencyImportBoot :: Bool
  , dependencyImportSelected :: Maybe FilePath
  } deriving (Eq)

data DependencySource = DependencySource
  { dependencySourcePath :: FilePath
  , dependencySourceSha256 :: String
  }

data DependencyResolution = DependencyResolution
  { dependencyResolutionQualifier :: DependencyQualifier
  , dependencyResolutionModule :: String
  , dependencyResolutionBoot :: Bool
  , dependencyResolutionSelected :: Maybe FilePath
  , dependencyResolutionCandidates :: [FilePath]
  } deriving (Eq)

-- | Selected fresh-source home imports only. Package imports have no selected
-- source; an owner is resolved through the same downsweep source selection,
-- including boot witnesses. Retained exact imports belong to ExactCompilation.
selectedFreshHomeRequirements :: DependencyEvidence -> String -> String -> Either String [(String, String)]
selectedFreshHomeRequirements evidence unit owner = do
  node <- case [node | node <- dependencyModules evidence
    , dependencyModuleUnit node == unit, dependencyModuleName node == owner
    , not (dependencyModuleBoot node)] of
    [node] -> Right node
    _ -> Left "supporting original lacks one dependency owner"
  sort . nub <$> mapM selected
    [edge | edge <- dependencyModuleImports node, Just _ <- [dependencyImportSelected edge]]
  where
    selected edge = case
        [node | node <- dependencyModules evidence
          , Just (dependencyModuleSource node) == dependencyImportSelected edge
          , dependencyModuleName node == dependencyImportName edge
          , dependencyModuleBoot node == dependencyImportBoot edge
          , dependencyModuleUnit node == unit
          , any ((== dependencyModuleSource node) . dependencySourcePath) (dependencySources evidence)] of
      [node] -> Right (dependencyModuleUnit node, dependencyModuleName node)
      _ -> Left "selected home import lacks one captured dependency owner"

sourceEvidence :: FilePath -> IO DependencySource
sourceEvidence path = fst <$> sourceEvidenceWithFingerprint path

-- | Compute both digest families from one immutable read. The GHC fingerprint
-- can be compared with a 'ModSummary'; SHA-256 is persisted cache evidence.
sourceEvidenceWithFingerprint :: FilePath -> IO (DependencySource, Fingerprint)
sourceEvidenceWithFingerprint path = do
  bytes <- BS.readFile path
  pure
    ( DependencySource
        { dependencySourcePath = path
        , dependencySourceSha256 = concatMap hexByte (BS.unpack (SHA256.hash bytes))
        }
    , fingerprintByteString bytes
    )
  where
    hexByte byte = let rendered = showHex byte "" in replicate (2 - length rendered) '0' ++ rendered

-- | Refuse publication when any consumed source changed after compilation.
revalidateDependencyEvidence :: DependencyEvidence -> IO Bool
revalidateDependencyEvidence evidence = and <$> forM (dependencySources evidence) (\expected -> do
  actual <- sourceEvidence (dependencySourcePath expected)
  pure (dependencySourceSha256 actual == dependencySourceSha256 expected))

-- Publication keeps the consumed source receipts paired with these artifacts.
validateDependencyEvidence :: DependencyEvidence -> IO ()
validateDependencyEvidence evidence = do
  unchanged <- revalidateDependencyEvidence evidence
  unless unchanged (ioError (userError "source changed while compiler artifacts were being published"))

writeDependencyEvidence :: FilePath -> DependencyEvidence -> IO BS.ByteString
writeDependencyEvidence outDir evidence = do
  validateDependencyEvidence evidence
  let bytes = TE.encodeUtf8 (T.pack (renderDependencyEvidence evidence))
  BS.writeFile (outDir </> "dependencies.json") bytes
  pure bytes

renderDependencyEvidence :: DependencyEvidence -> String
renderDependencyEvidence evidence =
  "{\"version\":4"
    ++ ",\"cache_safe\":" ++ bool (dependencyCacheSafe evidence)
    ++ ",\"selection_complete\":" ++ bool (dependencySelectionComplete evidence)
    ++ ",\"sources\":[" ++ comma (map source (dependencySources evidence)) ++ "]"
    ++ ",\"resolutions\":[" ++ comma (map resolution (dependencyResolutions evidence)) ++ "]"
    ++ ",\"packages\":[" ++ comma (map jsonString (dependencyPackages evidence)) ++ "]"
    ++ ",\"modules\":[" ++ comma (map moduleNode (dependencyModules evidence)) ++ "]}"
  where
    bool True = "true"
    bool False = "false"
    comma = intercalate ","
    source item =
      "{\"path\":" ++ jsonString (dependencySourcePath item)
        ++ ",\"sha256\":" ++ jsonString (dependencySourceSha256 item) ++ "}"
    resolution item =
      "{\"qualifier\":" ++ jsonString (renderDependencyQualifier (dependencyResolutionQualifier item))
        ++ ",\"module\":" ++ jsonString (dependencyResolutionModule item)
        ++ ",\"boot\":" ++ bool (dependencyResolutionBoot item)
        ++ ",\"selected\":" ++ maybe "null" jsonString (dependencyResolutionSelected item)
        ++ ",\"candidates\":["
        ++ comma (map jsonString (dependencyResolutionCandidates item)) ++ "]}"
    moduleNode item =
      "{\"unit\":" ++ jsonString (dependencyModuleUnit item)
        ++ ",\"module\":" ++ jsonString (dependencyModuleName item)
        ++ ",\"boot\":" ++ bool (dependencyModuleBoot item)
        ++ ",\"source\":" ++ jsonString (dependencyModuleSource item)
        ++ ",\"imports\":[" ++ comma (map importNode (dependencyModuleImports item)) ++ "]"
        ++ ",\"product\":" ++ jsonString (productAvailability (dependencyModuleProduct item)) ++ "}"
    importNode item =
      "{\"qualifier\":" ++ jsonString (renderDependencyQualifier (dependencyImportQualifier item))
        ++ ",\"module\":" ++ jsonString (dependencyImportName item)
        ++ ",\"boot\":" ++ bool (dependencyImportBoot item)
        ++ ",\"selected\":" ++ maybe "null" jsonString (dependencyImportSelected item) ++ "}"
    productAvailability ProductReady = "ready"
    productAvailability ProductBoot = "boot"
    productAvailability ProductInterfaceOnly = "interface_only"
    productAvailability ProductMissingInterface = "missing_interface"
    productAvailability ProductProjectionRejected = "projection_rejected"
