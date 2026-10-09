-- | Immutable custody of the encoded originals admitted by one physical request.
-- Readers live only during admission; consumers receive a sealed value and never
-- fall back to a producer path when a captured input is missing.
module Tidepool.RequestInputs
  ( RequestOriginalInputs, RequestInputReader, captureRequestInputs
  , CapturedRequestInput, capturedInputBytes, capturedInputSha256, capturedRequestInputToken
  , capturedRequestInput, mergeRequestInputs, requestInputRetained, retainRequestEncodedBytes, requestInputBytes, revalidateRequestInputs, revalidateRequestInputsWith ) where

import Control.Exception (IOException, try, finally)
import Control.Monad (unless, when, forM_, foldM)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import Control.Concurrent.MVar (newMVar, modifyMVar, modifyMVar_, readMVar)
import qualified Data.Map.Strict as Map
import Numeric (showHex)
import System.Environment (lookupEnv)
import Text.Read (readMaybe)
import Tidepool.BoundedRead (readFileAtMost, FileObservations, FileObservation(..), observeFile, withFileObservations)

type RequestInputReader = FilePath -> Int -> IO BS.ByteString

-- A sealed token can be decoded repeatedly without reconstructing its digest.
-- Only successful lookup in an admitted owner issues one.
data CapturedRequestInput = CapturedRequestInput BS.ByteString String
capturedInputBytes :: CapturedRequestInput -> BS.ByteString
capturedInputBytes (CapturedRequestInput bytes _) = bytes
capturedInputSha256 :: CapturedRequestInput -> String
capturedInputSha256 (CapturedRequestInput _ sha) = sha

data CapturedInput = CapturedInput BS.ByteString String
instance Eq CapturedInput where
  CapturedInput a shaA == CapturedInput b shaB = BS.length a == BS.length b && shaA == shaB
data RequestOriginalInputs = RequestOriginalInputs Integer Integer (Map.Map FilePath CapturedInput) (Map.Map String CapturedInput)
  deriving Eq

instance Show RequestOriginalInputs where
  show (RequestOriginalInputs limit used inputs encoded) = "RequestOriginalInputs "
    ++ show (limit,used,Map.size inputs,Map.size encoded)

requestInputBytes :: RequestOriginalInputs -> Integer
requestInputBytes (RequestOriginalInputs _ used _ _) = used

-- Each extension shares the preceding immutable map. The budget charges every
-- retained encoded byte, including graphs, Core and home package sidecars;
-- GHC's separately retained decoded structures are not charged as encoded bytes.
captureRequestInputs :: Maybe RequestOriginalInputs -> (RequestInputReader -> IO a)
  -> IO (a, RequestOriginalInputs)
captureRequestInputs previous action = do
  initial <- case previous of
    Just inputs -> pure inputs
    Nothing -> do
      configured <- lookupEnv "TIDEPOOL_REQUEST_CAPTURE_BYTES"
      limit <- case configured of
        Nothing -> pure (4 * 1024 * 1024 * 1024)
        Just value -> case readMaybe value of
          Just amount | amount > 0 -> pure amount
          _ -> fail "TIDEPOOL_REQUEST_CAPTURE_BYTES must be a positive integer"
      pure (RequestOriginalInputs limit 0 Map.empty Map.empty)
  state <- newMVar (True,initial)
  let readInput path bound = modifyMVar state $ \(open,originals@(RequestOriginalInputs limit used inputs encoded)) -> do
        unless open (fail "request admission reader used after sealing")
        when (bound < 0) (fail "negative request input bound")
        case Map.lookup path inputs of
          Just (CapturedInput bytes _) -> do
            when (BS.length bytes > bound) (fail "captured request input exceeds its consumer byte bound")
            pure ((open,originals),bytes)
          Nothing -> do
            let available = min (toInteger (min bound (maxBound - 1))) (limit - used)
            bytes <- readFileAtMost path (fromInteger available + 1)
            when (toInteger (BS.length bytes) > available)
              (fail "request original input capture exceeds its byte budget or artifact bound")
            let captured = CapturedInput bytes (digest bytes)
                next = RequestOriginalInputs limit (used + toInteger (BS.length bytes))
                  (Map.insert path captured inputs) encoded
            pure ((open,next),bytes)
  result <- action readInput `finally` modifyMVar_ state (\(_,inputs) -> pure (False,inputs))
  (_,sealed) <- readMVar state
  pure (result,sealed)

capturedRequestInput :: RequestOriginalInputs -> FilePath -> String -> IO BS.ByteString
capturedRequestInput inputs path expected = capturedInputBytes <$> capturedRequestInputToken inputs path expected

capturedRequestInputToken :: RequestOriginalInputs -> FilePath -> String -> IO CapturedRequestInput
capturedRequestInputToken (RequestOriginalInputs _ _ inputs _) path expected =
  case Map.lookup path inputs of
    Just (CapturedInput bytes actual) | actual == expected -> pure (CapturedRequestInput bytes actual)
    _ -> fail "request input is absent or differs from its admitted seal"

-- Transfer existing captures into the receiving request's budget. Equal paths
-- share their retained payload; conflicting captures cannot replace originals.
-- The receiver's allowance governs the union, regardless of donor allowances.
mergeRequestInputs :: RequestOriginalInputs -> [RequestOriginalInputs]
  -> Either String RequestOriginalInputs
mergeRequestInputs = foldM merge
  where
    merge receiving (RequestOriginalInputs _ _ files encoded) = do
      withFiles <- foldM addFile receiving (Map.toAscList files)
      foldM addEncoded withFiles (Map.toAscList encoded)
    addFile owner@(RequestOriginalInputs limit used files encoded) (path,input@(CapturedInput bytes _)) =
      case Map.lookup path files of
        Just old | old == input -> Right owner
        Just _ -> Left "request capture transfer conflicts with an admitted path"
        Nothing | used + toInteger (BS.length bytes) <= limit ->
          Right (RequestOriginalInputs limit (used + toInteger (BS.length bytes))
            (Map.insert path input files) encoded)
        _ -> Left "request capture transfer exceeds the receiving byte budget"
    addEncoded owner@(RequestOriginalInputs limit used files encoded) (sha,input@(CapturedInput bytes _)) =
      case Map.lookup sha encoded of
        Just old | old == input -> Right owner
        Just _ -> Left "request capture transfer conflicts with an encoded input"
        Nothing | used + toInteger (BS.length bytes) <= limit ->
          Right (RequestOriginalInputs limit (used + toInteger (BS.length bytes))
            files (Map.insert sha input encoded))
        _ -> Left "request capture transfer exceeds the receiving byte budget"

-- An encoded graph issued inside the request has no mutable producer path.
-- Its owning scope retains the bytes and facts together; charge those bytes in
-- the same request budget without introducing a path or a second file store.
retainRequestEncodedBytes :: [BS.ByteString] -> RequestOriginalInputs -> Maybe RequestOriginalInputs
retainRequestEncodedBytes [] owner = Just owner
retainRequestEncodedBytes (bytes:rest) owner@(RequestOriginalInputs limit used files encoded) =
  let sha = digest bytes
  in case Map.lookup sha encoded of
    Just _ -> retainRequestEncodedBytes rest owner
    Nothing | used + toInteger (BS.length bytes) <= limit ->
      retainRequestEncodedBytes rest (RequestOriginalInputs limit
        (used + toInteger (BS.length bytes)) files
        (Map.insert sha (CapturedInput bytes sha) encoded))
    _ -> Nothing

requestInputRetained :: RequestOriginalInputs -> FilePath -> String -> Bool
requestInputRetained (RequestOriginalInputs _ _ inputs _) path expected =
  case Map.lookup path inputs of
    Just (CapturedInput _ actual) -> actual == expected
    Nothing -> False

-- Publication observes current producer paths independently of custody. A
-- transient mutation cannot change consumption, and persistent drift refuses
-- terminal publication. Async exceptions are not converted into validation.
revalidateRequestInputs :: RequestOriginalInputs -> IO (Either String ())
revalidateRequestInputs inputs = withFileObservations (\observations -> revalidateRequestInputsWith observations inputs)

revalidateRequestInputsWith :: FileObservations -> RequestOriginalInputs -> IO (Either String ())
revalidateRequestInputsWith observations (RequestOriginalInputs _ _ inputs _) = do
  result <- try $ forM_ (Map.toAscList inputs) $ \(path,CapturedInput bytes sha) -> do
    current <- observeFile observations path (Just (BS.length bytes))
    unless (observedByteCount current == BS.length bytes && observedSha256 current == sha)
      (fail "request original input changed before publication")
  pure (either (Left . show) Right (result :: Either IOException ()))

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let rendered = showHex byte ""
  in replicate (2 - length rendered) '0' ++ rendered) . BS.unpack . SHA.hash
