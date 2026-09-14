{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}

module Main (main) where

import Control.Monad (forM, unless)
import Control.Exception
  ( AsyncException, SomeException, evaluate, fromException, throwIO, try )
import Data.ByteString qualified as BS
import Data.List (intercalate, isInfixOf)
import Data.Text qualified as Text
import Data.Text.Encoding qualified as TextEncoding
import GHC.Builtin.PrimOps (PrimCall(..))
import GHC.Builtin.Types.Prim (intPrimTy)
import GHC.Core (AltCon(DEFAULT))
import GHC.Core.Multiplicity (pattern ManyTy)
import GHC.Data.FastString (fsLit)
import GHC.Stg.Syntax
import GHC.Types.Basic (FunctionOrData(IsFunction))
import GHC.Types.CostCentre (dontCareCCS)
import GHC.Types.Id (mkSysLocal)
import GHC.Types.Literal (Literal(LitLabel))
import GHC.Types.Name.Env (emptyNameEnv)
import GHC.Types.Unique (mkUniqueGrimily)
import GHC.Types.Var.Set (emptyDVarSet)
import GHC.Unit.Module (mkModuleName)
import GHC.Unit.Types (mainUnit, mkModule)
import System.Directory (createDirectoryIfMissing)
import System.Environment (getArgs)
import System.FilePath ((</>))
import qualified System.Info as SystemInfo

import ExecutionCorpusInventory
  ( TargetInventory, inventoryRecoveredTarget, renderTargetInventories
  , unavailableTargetInventory, renderPreparedFactsForTest )
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), TextMemchrAuthority, resolveTextPackageUnit
  , preparedTopIdentities, projectPreparedTarget )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..) )
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PipelineResult(..), PreparedPipelineResult(..)
  , runPipelineSelected )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), recoverPreparedClosure )
import Tidepool.PreparedFormatting
  ( FormattingAuthority, resolveFormattingAuthority )
import Tidepool.PreparedFacts (PreparedFacts(..), extractPreparedFacts)
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
  (records, legacyTargets, inventories) <- case compiled of
    Left failure -> if allTops
      then ioError (userError
        ("all-tops source compilation rejected: " <> show failure))
      else let reason = "source compilation rejected: " <> show failure
        in pure
          ( map (rejectedRecord moduleNameArg reason) targets
          , map (\target -> LegacyTarget target Nothing) targets
          , map (\target -> unavailableTargetInventory
              (missingName moduleNameArg target) [] reason) targets
          )
    Right prepared -> do
      formattingAuthority <- resolveFormattingAuthority
        (prHscEnv (pprPipelineResult prepared))
      textAuthority <- resolveTextPackageUnit (prHscEnv (pprPipelineResult prepared))
      enumerated <- trySync (evaluate (forceIdentities
        (preparedTopIdentities (pprModules prepared))))
      case enumerated of
        Left failure -> if allTops
          then ioError (userError
            ("all-tops prepared identity enumeration rejected: " <> show failure))
          else let reason = "prepared identity enumeration rejected: " <> show failure
            in pure
              ( map (rejectedRecord moduleNameArg reason) targets
              , map (\target -> LegacyTarget target Nothing) targets
              , map (\target -> unavailableTargetInventory
                  (missingName moduleNameArg target) [] reason) targets
              )
        Right (Left failure) -> if allTops
          then ioError (userError
            ("all-tops prepared identity enumeration rejected: " <> show failure))
          else let reason = "prepared identity enumeration rejected: " <> show failure
            in pure
              ( map (rejectedRecord moduleNameArg reason) targets
              , map (\target -> LegacyTarget target Nothing) targets
              , map (\target -> unavailableTargetInventory
                  (missingName moduleNameArg target) [] reason) targets
              )
        Right (Right identities) -> do
          let selected = filter (inModule moduleNameArg) identities
          legacy <- mapLegacyTargets moduleNameArg identities targets
          rowsWithInventory <- if allTops
            then forM (zip [0 :: Int ..] selected) $ \(index, identity) ->
              projectOneIdentity prepared formattingAuthority textAuthority outputDir index identity
            else forM (zip [0 :: Int ..] (zip targets legacy)) $ \(index, (occurrence, legacyTarget)) ->
              projectOneTarget prepared formattingAuthority textAuthority moduleNameArg outputDir index occurrence
                (legacyTargetIdentity legacyTarget)
          let (rows, targetInventories) = unzip rowsWithInventory
          pure (rows, legacy, targetInventories)
  BS.writeFile (outputDir </> "manifest.json")
    (toBytes (renderManifest records legacyTargets))
  BS.writeFile (outputDir </> "diagnostic-inventory.json")
    (toBytes (renderTargetInventories inventories))

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
  -> Maybe FormattingAuthority
  -> Maybe TextMemchrAuthority
  -> String
  -> FilePath
  -> Int
  -> String
  -> Maybe SymbolIdentity
  -> IO (Record, TargetInventory)
projectOneTarget prepared formattingAuthority textAuthority moduleNameArg outputDir index occurrence mapped = do
  let name = missingName moduleNameArg occurrence
      reject reason = pure
        ( Record name Nothing [] (Rejected reason)
        , unavailableTargetInventory name [] reason
        )
  case mapped of
    Nothing -> reject ("target " <> show occurrence <> " is missing from module " <> moduleNameArg)
    Just selected -> projectOneIdentity prepared formattingAuthority textAuthority outputDir index selected

projectOneIdentity
  :: PreparedPipelineResult
  -> Maybe FormattingAuthority
  -> Maybe TextMemchrAuthority
  -> FilePath
  -> Int
  -> SymbolIdentity
  -> IO (Record, TargetInventory)
projectOneIdentity prepared formattingAuthority textAuthority outputDir index selected = do
  let context = projectionContext formattingAuthority textAuthority selected
      artifactName = numericArtifactName index
      name = identityName selected
      expectationKey = externalExpectationKey selected
      unavailable residuals reason = unavailableTargetInventory name residuals reason
      reject residuals inventory reason = pure
        (Record name Nothing residuals (Rejected reason), inventory)
  recovered <- trySync (recoverPreparedClosure
    (prHscEnv (pprPipelineResult prepared)) context (pprModules prepared))
  case recovered of
    Left failure -> let reason = "target " <> show (symbolOccurrence selected) <> " recovery failed: " <> show failure
      in reject [] (unavailable [] reason) reason
    Right closure -> do
      let residuals = closureFailures closure
          inventory = inventoryRecoveredTarget context selected closure
      projected <- trySync (evaluate
        (projectPreparedTarget context (closureModules closure)))
      case projected of
        Left failure -> reject residuals inventory ("target " <> show (symbolOccurrence selected)
          <> " projection rejected: " <> show failure)
        Right (Left failure) -> reject residuals inventory ("target " <> show (symbolOccurrence selected)
          <> " rejected: " <> show failure)
        Right (Right program) -> do
          encoded <- trySync (evaluate (BS.copy (encodeWireProgram program)))
          case encoded of
            Left failure -> reject residuals inventory ("target " <> show (symbolOccurrence selected)
              <> " encoding rejected: " <> show failure)
            Right bytes -> do
              BS.writeFile (outputDir </> artifactName) bytes
              pure
                ( Record name expectationKey residuals (Projected artifactName selected)
                , inventory
                )

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
  inventorySelfTest assert
  putStrLn "execution-corpus-projection mapping self-test: ok"

inventorySelfTest :: (String -> Bool -> IO ()) -> IO ()
inventorySelfTest assert = do
  let modul = mkModule mainUnit (mkModuleName "InventorySelfTest")
      topBinder = mkSysLocal (fsLit "inventory_top") (mkUniqueGrimily 7001)
        ManyTy intPrimTy
      caseBinder = mkSysLocal (fsLit "inventory_case") (mkUniqueGrimily 7002)
        ManyTy intPrimTy
      first = StgPrimCallOp (PrimCall (fsLit "stg_inventory_first") mainUnit)
      second = StgPrimCallOp (PrimCall (fsLit "stg_inventory_second") mainUnit)
      label = LitLabel (fsLit "inventory_label") IsFunction
      body = StgCase (StgOpApp first [] intPrimTy) caseBinder PolyAlt
        [GenStgAlt DEFAULT [] (StgOpApp second [StgLitArg label] intPrimTy)]
      rhs = StgRhsClosure emptyDVarSet dontCareCCS ReEntrant [] body intPrimTy
      facts = extractPreparedFacts modul emptyNameEnv
        [StgTopLifted (StgNonRec topBinder rhs)]
      rendered = renderPreparedFactsForTest modul facts
  assert "inventory retains both nested unsupported operations"
    (length (preparedOperations facts) == 2
      && "stg_inventory_first" `isInfixOf` rendered
      && "stg_inventory_second" `isInfixOf` rendered)
  assert "inventory retains label literals after unsupported operations"
    (case preparedLiterals facts of
      [LitLabel found IsFunction] -> found == fsLit "inventory_label"
        && "inventory_label" `isInfixOf` rendered
      _ -> False)

mapLegacyTargetsPure
  :: String -> [SymbolIdentity] -> [String] -> [Either String LegacyTarget]
mapLegacyTargetsPure moduleNameArg identities = map $ \legacyName ->
  fmap (LegacyTarget legacyName) (exactExternalMapping moduleNameArg identities legacyName)

projectionContext :: Maybe FormattingAuthority -> Maybe TextMemchrAuthority
  -> SymbolIdentity -> ProjectionContext
projectionContext formattingAuthority textAuthority identity =
  ProjectionContext
    { projectionProfile = "ghc-9.12-prepared-stg"
    , projectionToolchain = "ghc-9.12.2"
    , projectionTarget = targetDescriptor
    , projectionRetainedGenerations = mempty
    , projectionEntry = identity
    , projectionFormattingAuthority = formattingAuthority
    , projectionTextUnit = textAuthority
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
