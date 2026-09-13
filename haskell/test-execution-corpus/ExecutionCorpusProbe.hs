{-# LANGUAGE OverloadedStrings #-}

module Main (main) where

import Control.Monad (forM)
import Control.Exception
  ( AsyncException, SomeException, evaluate, fromException, throwIO, try )
import Data.ByteString qualified as BS
import Data.List (intercalate)
import Data.Text qualified as Text
import Data.Text.Encoding qualified as TextEncoding
import System.Directory (createDirectoryIfMissing)
import System.Environment (getArgs)
import System.FilePath ((</>))
import qualified System.Info as SystemInfo

import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), preparedTopIdentities, projectPreparedTarget )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..) )
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult(..)
  , runPipelineSelected )
import Tidepool.Json (jsonString)

data Record = Record String Outcome

data Outcome
  = Projected FilePath SymbolIdentity
  | Rejected String

main :: IO ()
main = do
  arguments <- getArgs
  (source, moduleNameArg, targetsFile, outputDir, includes) <- case arguments of
    source : moduleNameArg : targetsFile : outputDir : rest
      | not (null rest) -> pure (source, moduleNameArg, targetsFile, outputDir, rest)
    _ -> ioError (userError
      "usage: execution-corpus-projection SOURCE MODULE TARGETS_FILE OUTPUT_DIR INCLUDE...")
  targets <- lines <$> readFile targetsFile
  createDirectoryIfMissing True outputDir
  compiled <- trySync (runPipelineSelected PreparedStg source includes)
  records <- case compiled of
    Left failure -> pure (map (rejectedRecord ("source compilation rejected: " <> show failure)) targets)
    Right prepared -> do
      enumerated <- trySync (evaluate (forceIdentities
        (preparedTopIdentities (pprModules prepared))))
      case enumerated of
        Left failure -> pure (map (rejectedRecord
          ("prepared identity enumeration rejected: " <> show failure)) targets)
        Right (Left failure) -> pure (map (rejectedRecord
          ("prepared identity enumeration rejected: " <> show failure)) targets)
        Right (Right identities) -> forM (zip [0 :: Int ..] targets) $ \(index, occurrence) ->
          projectOne prepared identities moduleNameArg outputDir index occurrence
  BS.writeFile (outputDir </> "manifest.json") (toBytes (renderManifest records))

forceIdentities :: Either a [b] -> Either a [b]
forceIdentities result = case result of
  Left failure -> Left failure
  Right identities -> length identities `seq` Right identities

trySync :: IO a -> IO (Either SomeException a)
trySync action = do
  result <- try action
  case result of
    Left exception -> case fromException exception :: Maybe AsyncException of
      Just _ -> throwIO exception
      Nothing -> pure (Left exception)
    Right value -> pure (Right value)

rejectedRecord :: String -> String -> Record
rejectedRecord reason occurrence = Record occurrence (Rejected reason)

projectOne
  :: PreparedPipelineResult
  -> [SymbolIdentity]
  -> String
  -> FilePath
  -> Int
  -> String
  -> IO Record
projectOne prepared identities moduleNameArg outputDir index occurrence = do
  let candidates = filter matches identities
      matches identity = symbolModule identity == Text.pack moduleNameArg
        && symbolOccurrence identity == Text.pack occurrence
      reject reason = pure (Record occurrence (Rejected reason))
  case candidates of
    [] -> reject ("target " <> show occurrence <> " is missing from module " <> moduleNameArg)
    [selected] -> do
      let context = projectionContext selected
          artifactName = numericArtifactName index
      projected <- trySync (evaluate
        (projectPreparedTarget context (pprModules prepared)))
      case projected of
        Left failure -> reject ("target " <> show occurrence <> " projection rejected: " <> show failure)
        Right (Left failure) -> reject ("target " <> show occurrence <> " rejected: " <> show failure)
        Right (Right program) -> do
          encoded <- trySync (evaluate (BS.copy (encodeWireProgram program)))
          case encoded of
            Left failure -> reject ("target " <> show occurrence <> " encoding rejected: " <> show failure)
            Right bytes -> do
              BS.writeFile (outputDir </> artifactName) bytes
              pure (Record occurrence (Projected artifactName selected))
    _ -> reject ("target " <> show occurrence <> " is ambiguous in module "
      <> moduleNameArg <> " (" <> show (length candidates) <> " matches)")

projectionContext :: SymbolIdentity -> ProjectionContext
projectionContext identity =
  ProjectionContext
    { projectionProfile = "ghc-9.12-prepared-stg"
    , projectionToolchain = "ghc-9.12.2"
    , projectionTarget = targetDescriptor
    , projectionRetainedGenerations = mempty
    , projectionEntry = identity
    }

targetDescriptor :: TargetDescriptor
targetDescriptor = case SystemInfo.arch of
  "x86_64" -> TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
  "aarch64" -> TargetDescriptor Aarch64 LittleEndian 64 64 "aapcs64" []
  other -> error ("prepared execution is not configured for " <> other)

numericArtifactName :: Int -> FilePath
numericArtifactName index = show index <> ".prepared.cbor"

renderManifest :: [Record] -> String
renderManifest records = "{\"version\":1,\"programs\":["
  <> intercalate "," (map renderRecord records)
  <> "]}"

renderRecord :: Record -> String
renderRecord (Record name outcome) = "{\"name\":" <> jsonString name <> ","
  <> case outcome of
    Projected artifact identity -> "\"status\":\"projected\",\"artifact\":"
      <> jsonString artifact <> ",\"identity\":" <> renderIdentity identity <> "}"
    Rejected reason -> "\"status\":\"rejected\",\"reason\":"
      <> jsonString reason <> "}"

renderIdentity :: SymbolIdentity -> String
renderIdentity identity = "{\"unit\":" <> jsonString (Text.unpack (symbolUnit identity))
  <> ",\"module\":" <> jsonString (Text.unpack (symbolModule identity))
  <> ",\"namespace\":" <> jsonString (Text.unpack (symbolNamespace identity))
  <> ",\"occurrence\":" <> jsonString (Text.unpack (symbolOccurrence identity))
  <> "}"

toBytes :: String -> BS.ByteString
toBytes = TextEncoding.encodeUtf8 . Text.pack
