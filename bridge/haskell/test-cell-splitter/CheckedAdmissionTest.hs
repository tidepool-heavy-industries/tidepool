{-# LANGUAGE OverloadedStrings #-}

module CheckedAdmissionTest (checkedAdmissionChecks) where

import qualified Codec.CBOR.Encoding as Cbor
import qualified Codec.CBOR.Write as Cbor
import Data.Maybe (isJust, isNothing)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Control.Exception (IOException, bracket, try)
import Control.Monad (unless, forM_)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Numeric (readHex)
import System.Directory (getTemporaryDirectory, removeDirectoryRecursive, createDirectory, removeFile)
import System.FilePath ((</>))
import System.IO (openTempFile, hClose)
import System.IO.Error (isDoesNotExistError)
import Tidepool.Binders (StmtBinders(..), TurnKind(..))
import Tidepool.CheckedAdmission
import Tidepool.CheckedCell (CheckedSignature(..))
import Tidepool.CheckedRecipe (writeCheckedItemReceipt, writeCheckedDisplayReceipt, checkedRecipeSource)
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExactScope
import Tidepool.ExtractRequest (RequestField(..), WorkerRequest(..), InspectionRequest(..), workerArgv, workerRequestFromArgv)
import Tidepool.ExtractUtil (shaHex)

checkedAdmissionChecks :: IO ()
checkedAdmissionChecks = withScratch $ \root -> do
  let first = root </> "first"
      second = root </> "second"
      source = "let café = 1"
      wrapper = "wrapper λ"
      inventory = [("bind", shaHex "first"), ("expr", shaHex "second"), ("bind", shaHex "first")]
      fields = [BindGen 7, InjectVal "Val7", TurnTemplate "bind" first,
        TurnTemplate "expr" second, TurnTemplate "bind" first]
      cell = CheckedCellAdmission "cell-admission" (digest source) (digest wrapper)
        inventory ["Val7"] [] [] Nothing AuthoredCellCheck
      item = CheckedItemAdmission AuthoredCheckedItem "item-admission" "cell-receipt" 2
        (digest source) "bind" ["café"] inventory ["Val7"]
        [CheckedSignature "__tidepool_cell_pin_2_café" "Int" (BS.singleton 0) []]
        Nothing Nothing 7 "prefix" [] Nothing Nothing [] []
      display = CheckedDisplayAdmission "display-admission" "cell-receipt" 2
        "observation" 6 7 "prefix" 32 [] inventory ["Val7"] [] "rendered" Nothing [] []
      verdict = StmtBinders KBind ["café"] []
      displayVerdict = StmtBinders KBind (checkedDisplayBinders display) []
      observation = SymbolIdentity "main" "Tidepool.Session.Val.G6" "value" "observation" Nothing
  BS.writeFile first "first"
  BS.writeFile second "second"
  args <- request fields
  let displayArgs = args { requestRetainedGenerations = Map.singleton observation 6 }
      checkCell = validateCheckedCellAdmission args cell source wrapper
      checkItem = validateCheckedItemAdmission args item source verdict
      checkDisplay = validateCheckedDisplayAdmission displayArgs display "" displayVerdict
  checkCell
  checkItem
  checkDisplay
  -- All three checkpoints must detect a recipe changed since a previous pass.
  BS.writeFile first "changed"
  expectFailure "cell template mutation" cellError checkCell
  expectFailure "item template mutation" itemError checkItem
  expectFailure "display template mutation" displayError checkDisplay
  BS.writeFile first "first"
  checkCell
  checkItem
  checkDisplay
  forM_ [("reordered", drop 1 (requestTurnTemplates args) ++ take 1 (requestTurnTemplates args)), ("deduplicated", take 2 (requestTurnTemplates args))] $ \(label,templates) -> do
    let changed = args { requestTurnTemplates = templates }
    expectFailure (label ++ " cell recipes") cellError (validateCheckedCellAdmission changed cell source wrapper)
    expectFailure (label ++ " item recipes") itemError (validateCheckedItemAdmission changed item source verdict)
    expectFailure (label ++ " display recipes") displayError
      (validateCheckedDisplayAdmission (changed { requestRetainedGenerations = requestRetainedGenerations displayArgs }) display "" displayVerdict)
  expectFailure "UTF-8 cell bytes" cellError (validateCheckedCellAdmission args cell (source ++ " ") wrapper)
  expectFailure "wrapper bytes" cellError (validateCheckedCellAdmission args cell source (wrapper ++ " "))
  expectFailure "injected order" itemError (validateCheckedItemAdmission (args { requestInjectVals = ["Val7", "Val7"] }) item source verdict)
  expectFailure "generation" itemError (validateCheckedItemAdmission (args { requestBindGen = Just 8 }) item source verdict)
  expectFailure "verdict" itemError (validateCheckedItemAdmission args item source (StmtBinders KExpr [] []))
  expectFailure "signature key" itemError (validateCheckedItemAdmission args (item { itemSignatures = [CheckedSignature "wrong" "Int" (BS.singleton 0) []] }) source verdict)
  let reserved = "let __tidepool_checked_annotation_0 = 1"
  expectFailure "reserved annotation" "authored checked item uses a compiler-reserved annotation name"
    (validateCheckedItemAdmission args (item { itemSourceDigest = digest reserved }) reserved verdict)
  expectFailure "preview display" displayError (validateCheckedDisplayAdmission (displayArgs { requestActivationPreview = True }) display "" displayVerdict)
  expectFailure "display source" displayError (validateCheckedDisplayAdmission displayArgs display "authored" displayVerdict)
  expectFailure "observation generation" "display lacks its exact retained observation generation"
    (validateCheckedDisplayAdmission (displayArgs { requestRetainedGenerations = Map.singleton observation 5 }) display "" displayVerdict)
  expectFailure "missing observation" "display lacks its exact retained observation generation"
    (validateCheckedDisplayAdmission args display "" displayVerdict)
  missing <- try (validateCheckedCellAdmission (args { requestTurnTemplates = [("bind", root </> "absent")] }) cell "changed body" wrapper) :: IO (Either IOException ())
  unless (either isDoesNotExistError (const False) missing) (fail "missing recipe did not retain file-read error precedence")
  -- Literal placement must never rescan authored text for protected markers.
  let template = "__result = do {\n{{TURN_STMT}}\npure ({{BINDERS}})\n}"
      authored = "let café = \"{{BINDERS}}\""
  rendered <- checkedRecipeSource item template authored
  unless ("\"{{BINDERS}}\"" `T.isInfixOf` T.pack rendered) (fail "authored placeholder was rescanned")
  let inspect = args { requestInspections = [InspectNameInfo "visible"], requestInspectOut = Just "inspection.cbor", requestBindGen = Nothing }
  unless (matchesInspectionAdmission inspect ["Val7"]) (fail "inspection cap rejected matching inputs")
  forM_ [ inspect { requestTurn = True }, inspect { requestCell = True }
        , inspect { requestCheckSource = True }, inspect { requestCellPlan = True }
        , inspect { requestClassify = True }, inspect { requestCertifyHomeProducts = True }
        , inspect { requestActivationPreview = True }, inspect { requestDeclarationJoin = Just "join" }
        , inspect { requestBindGen = Just 7 }, inspect { requestTarget = Just "run" }
        , inspect { requestInjectVals = ["sibling"] }, inspect { requestInspectOut = Nothing }
        , inspect { requestRetainedGenerations = Map.singleton observation 6 }
        ] $ \wrong -> unless (not (matchesInspectionAdmission wrong ["Val7"]))
          (fail "inspection authority admitted another operation or input inventory")
  inspectionScopeChecks root
  receiptChecks root item display
  putStrLn "checked admission: mutation, ordering, authority, recipe and receipt checks passed"
  where
    digest = shaHex . TE.encodeUtf8 . T.pack
    cellError = "cell body, wrapper or injected interfaces differ from immutable admission"
    itemError = "checked item body, verdict, generation, signatures or recipe differs from its protected offer"
    displayError = "display request differs from its completed observation admission"

request :: [RequestField] -> IO WorkerRequest
request fields = case workerRequestFromArgv (workerArgv fields) of
  Right (Just args) -> pure args
  other -> fail ("request fixture failed: " ++ show other)

expectFailure :: String -> String -> IO () -> IO ()
expectFailure label message action = do
  result <- try action :: IO (Either IOException ())
  case result of
    Left exception | T.pack message `T.isInfixOf` T.pack (show exception) -> pure ()
    other -> fail (label ++ ": unexpected result " ++ show other)

withScratch :: (FilePath -> IO a) -> IO a
withScratch = bracket acquire removeDirectoryRecursive
  where
    acquire = do
      tmp <- getTemporaryDirectory
      (path, handle) <- openTempFile tmp "checked-admission-"
      hClose handle
      -- Replace the exclusively created file with a private test directory.
      removeFile path
      createDirectory path
      pure path

receiptChecks :: FilePath -> CheckedItemAdmission -> CheckedDisplayAdmission -> IO ()
receiptChecks root item display = do
  let scope = ExactScope "manifest" "request" "producer" "semantic" [] [] [] [] []
        Nothing Nothing Nothing Nothing Nothing Set.empty
      source = "recipe λ"
  writeCheckedItemReceipt root scope item source
  check "checked-item.cbor" itemGolden
  BS.writeFile (root </> "activation-type.cbor") "witness"
  writeCheckedItemReceipt root scope (item { itemPurpose = HostActivationInput }) source
  check "activation-input.cbor" activationGolden
  writeCheckedDisplayReceipt root scope display source
  check "checked-display.cbor" displayGolden
  where
    check file golden = do
      actual <- BS.readFile (root </> file)
      unless (actual == unhex golden) (fail (file ++ " changed its protected receipt bytes"))
    unhex [] = BS.empty
    unhex (a:b:rest) = case readHex [a,b] of
      [(byte,"")] -> BS.cons byte (unhex rest)
      _ -> error "invalid receipt golden"
    unhex _ = error "odd receipt golden"
    itemGolden = "886b545045584143544954454d613167726571756573746e6974656d2d61646d697373696f6e6c63656c6c2d7265636569707402784033323839613431343939636133623538653566653036353137386231343138373535303363323639663330356430323630336464383133616563313863656638781974696465706f6f6c2d636865636b65642d7265636970652d32"
    activationGolden = "89775450455841435441435449564154494f4e494e50555432613267726571756573746e6974656d2d61646d697373696f6e6c63656c6c2d7265636569707402784033323839613431343939636133623538653566653036353137386231343138373535303363323639663330356430323630336464383133616563313863656638782074696465706f6f6c2d686f73742d61637469766174696f6e2d696e7075742d32477769746e657373"
    displayGolden = "886e54504558414354444953504c4159613167726571756573746c63656c6c2d726563656970740266707265666978784033323839613431343939636133623538653566653036353137386231343138373535303363323639663330356430323630336464383133616563313863656638781974696465706f6f6c2d646973706c61792d7265636970652d31"

inspectionScopeChecks :: FilePath -> IO ()
inspectionScopeChecks root = do
  let owner = "Tidepool.Session.Val.G2"
      path = root </> "inspection-scope.cbor"
      digest = replicate 64 '0'
      strings values = Cbor.encodeListLen (fromIntegral (length values))
        <> foldMap (Cbor.encodeString . T.pack) values
      envelope injected modules paths = Cbor.toStrictByteString $
        Cbor.encodeListLen 8 <> stringsHeader digest
          <> Cbor.encodeListLen 0 <> Cbor.encodeListLen 0 <> Cbor.encodeListLen 0
          <> Cbor.encodeListLen 4 <> Cbor.encodeString "inspection1"
          <> strings injected
          <> Cbor.encodeListLen (fromIntegral (length modules))
          <> foldMap (\name -> Cbor.encodeListLen 4 <> Cbor.encodeString "main"
            <> Cbor.encodeString (T.pack name) <> Cbor.encodeString (T.pack (root </> "value.hi"))
            <> Cbor.encodeString (T.pack digest)) modules
          <> strings paths
      stringsHeader sha = Cbor.encodeString "TPEXACTSCOPE" <> Cbor.encodeString "4"
        <> Cbor.encodeString (T.pack sha) <> Cbor.encodeString (T.pack sha)
      readScope injected modules paths = do
        BS.writeFile path (envelope injected modules paths)
        readExactScope path
  admitted <- readScope [owner] [owner] [root]
  case admitted of
    Right scope -> unless (isJust (scopeCheckedInspection scope)
        && isNothing (scopeCheckedCell scope) && isNothing (scopeCheckedItem scope)
        && isNothing (scopeCheckedDisplay scope) && scopeIncludePaths scope == Just [root]
        && map exactModule (scopeValueInterfaces scope) == [owner])
      (fail "inspection purpose gained publication authority or lost captured inputs")
    Left failure -> fail failure
  -- Authorization names may sort lexically while captured interfaces retain
  -- numeric generation order. Both inventories still name exactly the cap.
  let owner9 = "Tidepool.Session.Val.G9"
      owner10 = "Tidepool.Session.Val.G10"
  numeric <- readScope [owner10,owner9] [owner9,owner10] [root]
  case numeric of
    Right scope -> unless (map exactModule (scopeValueInterfaces scope) == [owner9,owner10])
      (fail "inspection generation inventory reordered captured interface inputs")
    Left failure -> fail failure
  forM_ [([owner],[],[root]), ([owner,owner],[owner,owner],[root])
        , (["Tidepool.Session.Lib.G2"],["Tidepool.Session.Lib.G2"],[root])
        , ([owner],[owner],["relative-root"])] $ \(injected,modules,paths) -> do
    rejected <- readScope injected modules paths
    unless (either (const True) (const False) rejected)
      (fail "inspection scope accepted mismatched, duplicate, non-value or relative inputs")
