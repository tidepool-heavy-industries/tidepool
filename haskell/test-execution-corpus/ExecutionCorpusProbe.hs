{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}

module Main (main) where

import Control.Monad (forM, unless, when)
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
  ( ProjectionContext(..), TextUnitAuthority, resolveTextPackageUnit
  , preparedTopIdentities, projectPreparedTarget, projectPreparedTargetWithConstructors )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..) )
import Tidepool.FatIface (newFatIfaceCache, newOwnerInterfaceCache)
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PipelineResult(..), PreparedPipelineResult(..)
  , runPipelineSelected )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), newPreparedRecovery )
import Tidepool.PreparedStg (newPreparedBodyCache)
import Tidepool.PreparedFormatting
  ( FormattingAuthority, resolveFormattingAuthority )
import Tidepool.PreparedTime (TimeAuthority, resolveTimeAuthority)
import Tidepool.PreparedFacts (PreparedFacts(..), extractPreparedFacts)
import Tidepool.Metadata
  (collectDataCons, dcToMeta, mergeMetaPreserving, targetBindingHasIO, wiredInDataCons)
import Tidepool.CborEncode (encodeMetadata)
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
runProbe rawArguments = do
  let (metadataTargets, arguments) = case rawArguments of
        "--metadata-targets" : names : rest -> (words names, rest)
        rest -> ([], rest)
  (allTops, source, moduleNameArg, targetsFile, outputDir, includes) <- case arguments of
    "--all-tops" : source : moduleNameArg : targetsFile : outputDir : rest
      | not (null rest) -> pure (True, source, moduleNameArg, targetsFile, outputDir, rest)
    source : moduleNameArg : targetsFile : outputDir : rest
      | not (null rest) -> pure (False, source, moduleNameArg, targetsFile, outputDir, rest)
    _ -> ioError (userError
      "usage: execution-corpus-projection [--metadata-targets NAMES] [--all-tops] SOURCE MODULE TARGETS_FILE OUTPUT_DIR INCLUDE...")
  targets <- lines <$> readFile targetsFile
  createDirectoryIfMissing True outputDir
  compiled <- trySync (runPipelineSelected PreparedStg source includes)
  (records, sourceTargets, inventories) <- case compiled of
    Left failure -> if allTops
      then ioError (userError
        ("all-tops source compilation rejected: " <> show failure))
      else let reason = "source compilation rejected: " <> show failure
        in pure
          ( map (rejectedRecord moduleNameArg reason) targets
          , map (\target -> SourceTarget target Nothing) targets
          , map (\target -> unavailableTargetInventory
              (missingName moduleNameArg target) [] reason) targets
          )
    Right prepared -> do
      formattingAuthority <- resolveFormattingAuthority
        (prHscEnv (pprPipelineResult prepared))
      timeAuthority <- resolveTimeAuthority (prHscEnv (pprPipelineResult prepared))
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
              , map (\target -> SourceTarget target Nothing) targets
              , map (\target -> unavailableTargetInventory
                  (missingName moduleNameArg target) [] reason) targets
              )
        Right (Left failure) -> if allTops
          then ioError (userError
            ("all-tops prepared identity enumeration rejected: " <> show failure))
          else let reason = "prepared identity enumeration rejected: " <> show failure
            in pure
              ( map (rejectedRecord moduleNameArg reason) targets
              , map (\target -> SourceTarget target Nothing) targets
              , map (\target -> unavailableTargetInventory
                  (missingName moduleNameArg target) [] reason) targets
              )
        Right (Right identities) -> do
          let selected = filter (inModule moduleNameArg) identities
          sourceTargets <- mapSourceTargets moduleNameArg identities targets
          first <- case selected of
            identity : _ -> pure identity
            [] -> fail "prepared corpus selected no target identities"
          cache <- newFatIfaceCache
          ownerCache <- newOwnerInterfaceCache
          bodyCache <- newPreparedBodyCache
          recover <- newPreparedRecovery (prHscEnv (pprPipelineResult prepared))
            cache ownerCache bodyCache
            (projectionContext formattingAuthority timeAuthority textAuthority first)
            (pprModules prepared)
          when (not (null metadataTargets)) $ do
            constructors <- fmap concat $ forM metadataTargets $ \name -> do
              identity <- case filter (matchesExternal moduleNameArg name) selected of
                [identity] -> pure identity
                _ -> fail ("metadata target missing or ambiguous: " ++ name)
              closure <- recover identity
              case projectPreparedTargetWithConstructors
                     (projectionContext formattingAuthority timeAuthority textAuthority identity)
                     (closureModules closure) of
                Left failure -> fail ("metadata projection: " ++ show failure)
                Right (_, constructors) -> pure constructors
            let result = pprPipelineResult prepared
                metadata = mergeMetaPreserving
                  [wiredInDataCons, collectDataCons (prTyCons result), map dcToMeta constructors]
                hasIO = any (targetBindingHasIO (prBinds result)) metadataTargets
            BS.writeFile (outputDir </> "meta.cbor")
              (encodeMetadata metadata hasIO (Text.pack <$> prCapturedType result)
                (map Text.pack (prWarnings result)))
          rowsWithInventory <- if allTops
            then forM (zip [0 :: Int ..] selected) $ \(index, identity) ->
              projectOneIdentity recover formattingAuthority timeAuthority textAuthority outputDir index identity
            else forM (zip [0 :: Int ..] (zip targets sourceTargets)) $ \(index, (occurrence, sourceTarget)) ->
              projectOneTarget recover formattingAuthority timeAuthority textAuthority moduleNameArg outputDir index occurrence
                (sourceTargetIdentity sourceTarget)
          let (rows, targetInventories) = unzip rowsWithInventory
          pure (rows, sourceTargets, targetInventories)
  BS.writeFile (outputDir </> "manifest.json")
    (toBytes (renderManifest records sourceTargets))
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

data SourceTarget = SourceTarget String (Maybe SymbolIdentity)

sourceTargetIdentity :: SourceTarget -> Maybe SymbolIdentity
sourceTargetIdentity (SourceTarget _ identity) = identity

inModule :: String -> SymbolIdentity -> Bool
inModule moduleNameArg identity = symbolModule identity == Text.pack moduleNameArg

missingName :: String -> String -> String
missingName moduleNameArg occurrence =
  "<unknown>:" <> moduleNameArg <> ":value:" <> occurrence

mapSourceTargets :: String -> [SymbolIdentity] -> [String] -> IO [SourceTarget]
mapSourceTargets moduleNameArg identities = mapM (mapSourceTarget moduleNameArg identities)

mapSourceTarget :: String -> [SymbolIdentity] -> String -> IO SourceTarget
mapSourceTarget moduleNameArg identities sourceName = do
  mapping <- case exactExternalMapping moduleNameArg identities sourceName of
    Left reason -> ioError (userError reason)
    Right value -> pure value
  pure (SourceTarget sourceName mapping)

exactExternalMapping
  :: String -> [SymbolIdentity] -> String
  -> Either String (Maybe SymbolIdentity)
exactExternalMapping moduleNameArg identities sourceName = case externalMatches of
  [identity] -> Right (Just identity)
  [] -> Right Nothing
  _ -> Left ("source target " <> show sourceName
    <> " has ambiguous exact external matches; mapping rejected")
  where
    externalMatches = filter (matchesExternal moduleNameArg sourceName) identities

matchesExternal :: String -> String -> SymbolIdentity -> Bool
matchesExternal moduleNameArg occurrence identity =
  inModule moduleNameArg identity
    && symbolNamespace identity == "value"
    && symbolOccurrence identity == Text.pack occurrence

projectOneTarget
  :: (SymbolIdentity -> IO RecoveredClosure)
  -> Maybe FormattingAuthority
  -> Maybe TimeAuthority
  -> Maybe TextUnitAuthority
  -> String
  -> FilePath
  -> Int
  -> String
  -> Maybe SymbolIdentity
  -> IO (Record, TargetInventory)
projectOneTarget recover formattingAuthority timeAuthority textAuthority moduleNameArg outputDir index occurrence mapped = do
  let name = missingName moduleNameArg occurrence
      reject reason = pure
        ( Record name Nothing [] (Rejected reason)
        , unavailableTargetInventory name [] reason
        )
  case mapped of
    Nothing -> reject ("target " <> show occurrence <> " is missing from module " <> moduleNameArg)
    Just selected -> projectOneIdentity recover formattingAuthority timeAuthority textAuthority outputDir index selected

projectOneIdentity
  :: (SymbolIdentity -> IO RecoveredClosure)
  -> Maybe FormattingAuthority
  -> Maybe TimeAuthority
  -> Maybe TextUnitAuthority
  -> FilePath
  -> Int
  -> SymbolIdentity
  -> IO (Record, TargetInventory)
projectOneIdentity recover formattingAuthority timeAuthority textAuthority outputDir index selected = do
  let context = projectionContext formattingAuthority timeAuthority textAuthority selected
      artifactName = numericArtifactName index
      name = identityName selected
      expectationKey = externalExpectationKey selected
      unavailable residuals reason = unavailableTargetInventory name residuals reason
      reject residuals inventory reason = pure
        (Record name Nothing residuals (Rejected reason), inventory)
  recovered <- trySync (recover selected)
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
  assert "source mapping keeps exact external identity" $
    case mapSourceTargetsPure "Suite" identities ["answer"] of
      [Right target] -> sourceTargetIdentity target == Just external
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
  assert "duplicate source inputs are preserved" $
    length (mapSourceTargetsPure "Suite" identities ["answer", "answer"]) == 2
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

mapSourceTargetsPure
  :: String -> [SymbolIdentity] -> [String] -> [Either String SourceTarget]
mapSourceTargetsPure moduleNameArg identities = map $ \sourceName ->
  fmap (SourceTarget sourceName) (exactExternalMapping moduleNameArg identities sourceName)

projectionContext :: Maybe FormattingAuthority -> Maybe TimeAuthority -> Maybe TextUnitAuthority
  -> SymbolIdentity -> ProjectionContext
projectionContext formattingAuthority timeAuthority textAuthority identity =
  ProjectionContext
    { projectionProfile = "ghc-9.12-prepared-stg"
    , projectionToolchain = "ghc-9.12.2"
    , projectionTarget = targetDescriptor
    , projectionRetainedGenerations = mempty
    , projectionEntry = identity
    , projectionAuxiliaryRoots = []
    , projectionFormattingAuthority = formattingAuthority
    , projectionTimeAuthority = timeAuthority
    , projectionTextUnit = textAuthority
    }

targetDescriptor :: TargetDescriptor
targetDescriptor = case SystemInfo.arch of
  "x86_64" -> TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
  "aarch64" -> TargetDescriptor Aarch64 LittleEndian 64 64 "aapcs64" []
  other -> error ("prepared execution is not configured for " <> other)

numericArtifactName :: Int -> FilePath
numericArtifactName index = show index <> ".prepared.cbor"

renderManifest :: [Record] -> [SourceTarget] -> String
renderManifest records sourceTargets = "{\"version\":2,\"source_targets\":["
  <> intercalate "," (map renderSourceTarget sourceTargets)
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

renderSourceTarget :: SourceTarget -> String
renderSourceTarget (SourceTarget sourceName identity) =
  "{\"source_name\":" <> jsonString sourceName <> ",\"identity\":"
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
