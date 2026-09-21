-- | Versioned compiler dependency evidence written beside prepared artifacts.
-- The cache owner validates this document; the compiler owns producing it
-- because only the compiler knows which source and resolution paths it used.
module Tidepool.DependencyEvidence
  ( DependencyEvidence(..)
  , DependencySource(..)
  , DependencyResolution(..)
  , sourceEvidence
  , revalidateDependencyEvidence
  , renderDependencyEvidence
  ) where

import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import Data.List (intercalate)
import Control.Monad (forM)
import Numeric (showHex)

import Tidepool.Json (jsonString)

data DependencyEvidence = DependencyEvidence
  { dependencyCacheSafe :: Bool
  , dependencySelectionComplete :: Bool
  , dependencySources :: [DependencySource]
  , dependencyResolutions :: [DependencyResolution]
  , dependencyPackages :: [String]
  }

data DependencySource = DependencySource
  { dependencySourcePath :: FilePath
  , dependencySourceSha256 :: String
  }

data DependencyResolution = DependencyResolution
  { dependencyResolutionModule :: String
  , dependencyResolutionSelected :: Maybe FilePath
  , dependencyResolutionCandidates :: [FilePath]
  }

sourceEvidence :: FilePath -> IO DependencySource
sourceEvidence path = do
  bytes <- BS.readFile path
  pure DependencySource
    { dependencySourcePath = path
    , dependencySourceSha256 = concatMap hexByte (BS.unpack (SHA256.hash bytes))
    }
  where
    hexByte byte = let rendered = showHex byte "" in replicate (2 - length rendered) '0' ++ rendered

-- | Refuse publication when any consumed source changed after compilation.
revalidateDependencyEvidence :: DependencyEvidence -> IO Bool
revalidateDependencyEvidence evidence = and <$> forM (dependencySources evidence) (\expected -> do
  actual <- sourceEvidence (dependencySourcePath expected)
  pure (dependencySourceSha256 actual == dependencySourceSha256 expected))

renderDependencyEvidence :: DependencyEvidence -> String
renderDependencyEvidence evidence =
  "{\"version\":1"
    ++ ",\"cache_safe\":" ++ bool (dependencyCacheSafe evidence)
    ++ ",\"selection_complete\":" ++ bool (dependencySelectionComplete evidence)
    ++ ",\"sources\":[" ++ comma (map source (dependencySources evidence)) ++ "]"
    ++ ",\"resolutions\":[" ++ comma (map resolution (dependencyResolutions evidence)) ++ "]"
    ++ ",\"packages\":[" ++ comma (map jsonString (dependencyPackages evidence)) ++ "]}"
  where
    bool True = "true"
    bool False = "false"
    comma = intercalate ","
    source item =
      "{\"path\":" ++ jsonString (dependencySourcePath item)
        ++ ",\"sha256\":" ++ jsonString (dependencySourceSha256 item) ++ "}"
    resolution item =
      "{\"module\":" ++ jsonString (dependencyResolutionModule item)
        ++ ",\"selected\":" ++ maybe "null" jsonString (dependencyResolutionSelected item)
        ++ ",\"candidates\":["
        ++ comma (map jsonString (dependencyResolutionCandidates item)) ++ "]}"
