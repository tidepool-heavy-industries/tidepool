-- | Versioned compiler dependency evidence written beside prepared artifacts.
-- The cache owner validates this document; the compiler owns producing it
-- because only the compiler knows which source and resolution paths it used.
module Tidepool.DependencyEvidence
  ( DependencyEvidence(..)
  , DependencySource(..)
  , DependencyResolution(..)
  , DependencyModule(..)
  , DependencyImport(..)
  , ProductAvailability(..)
  , sourceEvidence
  , sourceEvidenceWithFingerprint
  , revalidateDependencyEvidence
  , renderDependencyEvidence
  ) where

import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import Data.List (intercalate)
import Control.Monad (forM)
import Numeric (showHex)
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

-- | The direct graph that produced the paired module products. This is
-- post-downsweep evidence; it does not by itself authorize a future compile
-- to reuse a module without running its own downsweep and interface checks.
data DependencyModule = DependencyModule
  { dependencyModuleUnit :: String
  , dependencyModuleName :: String
  , dependencyModuleBoot :: Bool
  , dependencyModuleSource :: FilePath
  , dependencyModuleImports :: [DependencyImport]
  , dependencyModuleProduct :: ProductAvailability
  }

data ProductAvailability
  = ProductReady
  | ProductBoot
  | ProductInterfaceOnly
  | ProductMissingInterface
  | ProductProjectionRejected
  deriving (Eq, Show)

data DependencyImport = DependencyImport
  { dependencyImportQualifier :: String
  , dependencyImportName :: String
  , dependencyImportBoot :: Bool
  , dependencyImportSelected :: Maybe FilePath
  }

data DependencySource = DependencySource
  { dependencySourcePath :: FilePath
  , dependencySourceSha256 :: String
  }

data DependencyResolution = DependencyResolution
  { dependencyResolutionQualifier :: String
  , dependencyResolutionModule :: String
  , dependencyResolutionBoot :: Bool
  , dependencyResolutionSelected :: Maybe FilePath
  , dependencyResolutionCandidates :: [FilePath]
  }

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
      "{\"qualifier\":" ++ jsonString (dependencyResolutionQualifier item)
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
      "{\"qualifier\":" ++ jsonString (dependencyImportQualifier item)
        ++ ",\"module\":" ++ jsonString (dependencyImportName item)
        ++ ",\"boot\":" ++ bool (dependencyImportBoot item)
        ++ ",\"selected\":" ++ maybe "null" jsonString (dependencyImportSelected item) ++ "}"
    productAvailability ProductReady = "ready"
    productAvailability ProductBoot = "boot"
    productAvailability ProductInterfaceOnly = "interface_only"
    productAvailability ProductMissingInterface = "missing_interface"
    productAvailability ProductProjectionRejected = "projection_rejected"
