{-# LANGUAGE OverloadedStrings #-}

module CheckedAdmissionTest (checkedAdmissionChecks) where

import qualified Codec.CBOR.Encoding as Cbor
import qualified Codec.CBOR.Write as Cbor
import Control.Exception (bracket)
import Control.Monad (unless, forM_)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Text as T
import System.Directory (getTemporaryDirectory, removeDirectoryRecursive, createDirectory, removeFile)
import System.FilePath ((</>))
import System.IO (openTempFile, hClose)
import Tidepool.CheckedAdmission
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExactScope
import Tidepool.ExtractRequest (RequestField(..), WorkerRequest(..), RequestOptions(..), AdmittedRequestOperation(..), admittedRequestOperation, admittedRequestOptions, admitWorkerRequest, workerArgv, workerRequestFromArgv)

checkedAdmissionChecks :: IO ()
checkedAdmissionChecks = withScratch $ \root -> do
  let retainedInput = SymbolIdentity "main" "Tidepool.Session.Val.G6" "value" "input" Nothing
  inspect <- rawRequest [Input "unused", InspectInfo "visible", InspectOut "inspection.cbor", InjectVal "Val7"]
  forM_ [inspect] $ \admitted -> do
    unless (matchesRawInspectionAdmission admitted ["Val7"]) (fail "inspection cap rejected matching complete inputs")
    forM_ [ admitted { requestTurn = True }
          , admitted { requestCheckSource = True }, admitted { requestCellPlan = True }
          , admitted { requestClassify = True }, admitted { requestCertifyHomeProducts = True }
          , admitted { requestActivationPreview = True }, admitted { requestDeclarationJoin = Just "join" }
          , admitted { requestBindGen = Just 7 }, admitted { requestTarget = Just "run" }
          , admitted { requestInjectVals = ["sibling"] }, admitted { requestInspectOut = Nothing }
          , admitted { requestRetainedGenerations = Map.singleton retainedInput 6 }
          ] $ \wrong -> unless (not (matchesRawInspectionAdmission wrong ["Val7"]))
            (fail "inspection authority admitted another operation or input inventory")
  inspectionScopeChecks root
  putStrLn "inspection admission: operation, inventory and legacy scope refusal checks passed"

rawRequest :: [RequestField] -> IO WorkerRequest
rawRequest fields = case workerRequestFromArgv (workerArgv fields) of
  Right (Just args) -> pure args
  other -> fail ("request fixture failed: " ++ show other)

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
  -- These legacy inspection envelopes have no typed interface evidence or
  -- producer certificate. Even matching inventories cannot authorize the current scope.
  let owner9 = "Tidepool.Session.Val.G9"
      owner10 = "Tidepool.Session.Val.G10"
  forM_ [([owner],[owner],[root])
        , ([owner10,owner9],[owner9,owner10],[root])
        , ([owner],[],[root]), ([owner,owner],[owner,owner],[root])
        , (["Tidepool.Session.Lib.G2"],["Tidepool.Session.Lib.G2"],[root])
        , ([owner],[owner],["relative-root"])] $ \(injected,modules,paths) -> do
    let expected = envelope injected modules paths
    rejected <- readScope injected modules paths
    case rejected of
      Left _ -> pure ()
      Right _ -> fail "legacy inspection envelope gained current scope authority"
    unchanged <- BS.readFile path
    unless (unchanged == expected) (fail "legacy inspection refusal changed its producer bytes")

matchesRawInspectionAdmission :: WorkerRequest -> [String] -> Bool
matchesRawInspectionAdmission raw values = case admitWorkerRequest raw of
  Right admitted | AdmittedInspection payload <- admittedRequestOperation admitted ->
    matchesInspectionAdmission (admittedRequestOptions admitted) payload values
  _ -> False
