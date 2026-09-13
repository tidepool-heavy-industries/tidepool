{-# LANGUAGE OverloadedStrings #-}

module Main (main) where

import Control.Monad (forM, unless)
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
  ( PipelineSelection(PreparedStg), PipelineResult(..), PreparedPipelineResult(..)
  , runPipelineSelected )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), recoverPreparedClosure )
import Tidepool.Json (jsonString)

data Record = Record String (Maybe String) [RecoveryFailure] Outcome

data Outcome
  = Projected FilePath SymbolIdentity
  | Rejected String

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  [] -> mappingSelfTest
  ["--self-test"] -> mappingSelfTest
  _ -> runProbe arguments

runProbe :: [String] -> IO ()
runProbe arguments = do
  (allTops, source, moduleNameArg, targetsFile, outputDir, includes) <- case arguments of
    "--all-tops" : source : moduleNameArg : targetsFile : outputDir : rest
      | not (null rest) -> pure (True, source, moduleNameArg, targetsFile, outputDir, rest)
    source : moduleNameArg : targetsFile : outputDir : rest
      | not (null rest) -> pure (False, source, moduleNameArg, targetsFile, outputDir, rest)
    _ -> ioError (userError
      "usage: execution-corpus-projection [--all-tops] SOURCE MODULE TARGETS_FILE OUTPUT_DIR INCLUDE...")
  targets <- lines <$> readFile targetsFile
  createDirectoryIfMissing True outputDir
  compiled <- trySync (runPipelineSelected PreparedStg source includes)
  (records, legacyTargets) <- case compiled of
    Left failure -> if allTops
      then ioError (userError
        ("all-tops source compilation rejected: " <> show failure))
      else pure
        ( map (rejectedRecord moduleNameArg
            ("source compilation rejected: " <> show failure)) targets
        , map (\target -> LegacyTarget target Nothing) targets
        )
    Right prepared -> do
      enumerated <- trySync (evaluate (forceIdentities
        (preparedTopIdentities (pprModules prepared))))
      case enumerated of
        Left failure -> if allTops
          then ioError (userError
            ("all-tops prepared identity enumeration rejected: " <> show failure))
          else pure
            ( map (rejectedRecord moduleNameArg
                ("prepared identity enumeration rejected: " <> show failure)) targets
            , map (\target -> LegacyTarget target Nothing) targets
            )
        Right (Left failure) -> if allTops
          then ioError (userError
            ("all-tops prepared identity enumeration rejected: " <> show failure))
          else pure
            ( map (rejectedRecord moduleNameArg
                ("prepared identity enumeration rejected: " <> show failure)) targets
            , map (\target -> LegacyTarget target Nothing) targets
            )
        Right (Right identities) -> do
          let selected = filter (inModule moduleNameArg) identities
          legacy <- mapLegacyTargets moduleNameArg identities targets
          rows <- if allTops
            then forM (zip [0 :: Int ..] selected) $ \(index, identity) ->
              projectOneIdentity prepared outputDir index identity
            else forM (zip [0 :: Int ..] (zip targets legacy)) $ \(index, (occurrence, legacyTarget)) ->
              projectOneTarget prepared moduleNameArg outputDir index occurrence
                (legacyTargetIdentity legacyTarget)
          pure (rows, legacy)
  BS.writeFile (outputDir </> "manifest.json")
    (toBytes (renderManifest records legacyTargets))

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

rejectedRecord :: String -> String -> String -> Record
rejectedRecord moduleNameArg reason occurrence =
  Record (missingName moduleNameArg occurrence) Nothing [] (Rejected reason)

data LegacyTarget = LegacyTarget String (Maybe SymbolIdentity)

legacyTargetIdentity :: LegacyTarget -> Maybe SymbolIdentity
legacyTargetIdentity (LegacyTarget _ identity) = identity

inModule :: String -> SymbolIdentity -> Bool
inModule moduleNameArg identity = symbolModule identity == Text.pack moduleNameArg

missingName :: String -> String -> String
missingName moduleNameArg occurrence =
  "<unknown>:" <> moduleNameArg <> ":value:" <> occurrence

mapLegacyTargets :: String -> [SymbolIdentity] -> [String] -> IO [LegacyTarget]
mapLegacyTargets moduleNameArg identities = mapM (mapLegacyTarget moduleNameArg identities)

mapLegacyTarget :: String -> [SymbolIdentity] -> String -> IO LegacyTarget
mapLegacyTarget moduleNameArg identities legacyName = do
  mapping <- case exactExternalMapping moduleNameArg identities legacyName of
    Left reason -> ioError (userError reason)
    Right value -> pure value
  pure (LegacyTarget legacyName mapping)

exactExternalMapping
  :: String -> [SymbolIdentity] -> String
  -> Either String (Maybe SymbolIdentity)
exactExternalMapping moduleNameArg identities legacyName = case externalMatches of
  [identity] -> Right (Just identity)
  [] -> Right Nothing
  _ -> Left ("legacy target " <> show legacyName
    <> " has ambiguous exact external matches; mapping rejected")
  where
    externalMatches = filter (matchesExternal moduleNameArg legacyName) identities

matchesExternal :: String -> String -> SymbolIdentity -> Bool
matchesExternal moduleNameArg occurrence identity =
  inModule moduleNameArg identity
    && symbolNamespace identity == "value"
    && symbolOccurrence identity == Text.pack occurrence

projectOneTarget
  :: PreparedPipelineResult
  -> String
  -> FilePath
  -> Int
  -> String
  -> Maybe SymbolIdentity
  -> IO Record
projectOneTarget prepared moduleNameArg outputDir index occurrence mapped = do
  let reject reason = pure (Record (missingName moduleNameArg occurrence)
        Nothing [] (Rejected reason))
  case mapped of
    Nothing -> reject ("target " <> show occurrence <> " is missing from module " <> moduleNameArg)
    Just selected -> projectOneIdentity prepared outputDir index selected

projectOneIdentity
  :: PreparedPipelineResult
  -> FilePath
  -> Int
  -> SymbolIdentity
  -> IO Record
projectOneIdentity prepared outputDir index selected = do
  let context = projectionContext selected
      artifactName = numericArtifactName index
      name = identityName selected
      expectationKey = externalExpectationKey selected
      reject residuals reason = pure (Record name Nothing residuals (Rejected reason))
  recovered <- trySync (recoverPreparedClosure
    (prHscEnv (pprPipelineResult prepared)) context (pprModules prepared))
  case recovered of
    Left failure -> reject [] ("target " <> show (symbolOccurrence selected)
      <> " recovery failed: " <> show failure)
    Right closure -> do
      let residuals = closureFailures closure
      projected <- trySync (evaluate
        (projectPreparedTarget context (closureModules closure)))
      case projected of
        Left failure -> reject residuals ("target " <> show (symbolOccurrence selected)
          <> " projection rejected: " <> show failure)
        Right (Left failure) -> reject residuals ("target " <> show (symbolOccurrence selected)
          <> " rejected: " <> show failure)
        Right (Right program) -> do
          encoded <- trySync (evaluate (BS.copy (encodeWireProgram program)))
          case encoded of
            Left failure -> reject residuals ("target " <> show (symbolOccurrence selected)
              <> " encoding rejected: " <> show failure)
            Right bytes -> do
              BS.writeFile (outputDir </> artifactName) bytes
              pure (Record name expectationKey residuals
                (Projected artifactName selected))

identityName :: SymbolIdentity -> String
identityName identity = intercalate ":" $ case symbolRecordParent identity of
  Nothing ->
    [ Text.unpack (symbolUnit identity)
    , Text.unpack (symbolModule identity)
    , Text.unpack (symbolNamespace identity)
    , Text.unpack (symbolOccurrence identity)
    ]
  Just parent ->
    [ Text.unpack (symbolUnit identity)
    , Text.unpack (symbolModule identity)
    , Text.unpack (symbolNamespace identity)
    , Text.unpack parent
    , Text.unpack (symbolOccurrence identity)
    ]

externalExpectationKey :: SymbolIdentity -> Maybe String
externalExpectationKey identity
  | symbolNamespace identity == "value" =
      Just (Text.unpack (symbolOccurrence identity))
  | otherwise = Nothing

mappingSelfTest :: IO ()
mappingSelfTest = do
  let external = SymbolIdentity "unit" "Suite" "value" "answer" Nothing
      internal = SymbolIdentity "unit" "Suite" "local" "answer" Nothing
      anotherExternal = SymbolIdentity "unit" "Suite" "value" "answer" Nothing
      recordExternal = SymbolIdentity "unit" "Suite" "value" "answer" (Just "RecordA")
      otherRecordExternal = SymbolIdentity "unit" "Suite" "value" "answer" (Just "RecordB")
      identities = [external, internal]
      assert label condition = unless condition
        (ioError (userError ("mapping self-test failed: " <> label)))
  assert "exact external mapping" $
    exactExternalMapping "Suite" identities "answer" == Right (Just external)
  assert "legacy projection keeps exact external identity" $
    case mapLegacyTargetsPure "Suite" identities ["answer"] of
      [Right target] -> legacyTargetIdentity target == Just external
      _ -> False
  assert "internal same-occurrence does not replace external" $
    exactExternalMapping "Suite" [internal] "answer" == Right Nothing
  assert "suffixes are not stripped" $
    exactExternalMapping "Suite" identities "answer.1" == Right Nothing
  assert "ambiguous exact external mapping rejects" $
    case exactExternalMapping "Suite" [external, anotherExternal] "answer" of
      Left _ -> True
      Right _ -> False
  assert "canonical identity name" (identityName external == "unit:Suite:value:answer")
  assert "record parent identity name" $
    identityName recordExternal == "unit:Suite:value:RecordA:answer"
  assert "record parent keeps field identities distinct" $
    identityName recordExternal /= identityName otherRecordExternal
  assert "external expectation key" (externalExpectationKey external == Just "answer")
  assert "internal expectation key is absent" (externalExpectationKey internal == Nothing)
  assert "empty identity input is unmapped" $
    exactExternalMapping "Suite" [] "answer" == Right Nothing
  assert "duplicate legacy inputs are preserved" $
    length (mapLegacyTargetsPure "Suite" identities ["answer", "answer"]) == 2
  putStrLn "execution-corpus-projection mapping self-test: ok"

mapLegacyTargetsPure
  :: String -> [SymbolIdentity] -> [String] -> [Either String LegacyTarget]
mapLegacyTargetsPure moduleNameArg identities = map $ \legacyName ->
  fmap (LegacyTarget legacyName) (exactExternalMapping moduleNameArg identities legacyName)

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

renderManifest :: [Record] -> [LegacyTarget] -> String
renderManifest records legacyTargets = "{\"version\":2,\"legacy_targets\":["
  <> intercalate "," (map renderLegacyTarget legacyTargets)
  <> "],\"programs\":["
  <> intercalate "," (map renderRecord records)
  <> "]}"

renderRecord :: Record -> String
renderRecord (Record name expectationKey residuals outcome) = "{\"name\":" <> jsonString name
  <> ",\"expectation_key\":" <> renderMaybeString expectationKey <> ","
  <> renderResiduals residuals
  <> case outcome of
    Projected artifact identity -> "\"status\":\"projected\",\"artifact\":"
      <> jsonString artifact <> ",\"identity\":" <> renderIdentity identity <> "}"
    Rejected reason -> "\"status\":\"rejected\",\"reason\":"
      <> jsonString reason <> "}"

renderResiduals :: [RecoveryFailure] -> String
renderResiduals [] = ""
renderResiduals failures = "\"recovery_failures\":["
  <> intercalate "," (map (jsonString . show) failures) <> "],"

renderLegacyTarget :: LegacyTarget -> String
renderLegacyTarget (LegacyTarget legacyName identity) =
  "{\"legacy_name\":" <> jsonString legacyName <> ",\"identity\":"
  <> maybe "null" renderIdentity identity <> "}"

renderMaybeString :: Maybe String -> String
renderMaybeString = maybe "null" jsonString

renderIdentity :: SymbolIdentity -> String
renderIdentity identity = "{\"unit\":" <> jsonString (Text.unpack (symbolUnit identity))
  <> ",\"module\":" <> jsonString (Text.unpack (symbolModule identity))
  <> ",\"namespace\":" <> jsonString (Text.unpack (symbolNamespace identity))
  <> ",\"occurrence\":" <> jsonString (Text.unpack (symbolOccurrence identity))
  <> ",\"record_parent\":" <> maybe "null" (jsonString . Text.unpack) (symbolRecordParent identity)
  <> "}"

toBytes :: String -> BS.ByteString
toBytes = TextEncoding.encodeUtf8 . Text.pack
