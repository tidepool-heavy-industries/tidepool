-- | Immutable custody of the encoded originals admitted by one physical request.
-- Readers live only during admission; consumers receive a sealed value and never
-- fall back to a producer path when a captured input is missing.
module Tidepool.RequestInputs
  ( RequestOriginalInputs, RequestInputReader, captureRequestInputs, RequestInputTokenReader, captureRequestInputTokens
  , CapturedOriginalContent, emptyCapturedOriginalContent, capturedOriginalContentBytes
  , OriginalInputReference(..), continueRequestInputs, selectedOriginalContent
  , mergeCapturedOriginalContent, selectCapturedOriginalContent, capturedOriginalContentKeys
  , requestCaptureByteLimit
  , CapturedRequestInput, capturedInputBytes, capturedInputSha256, capturedRequestInputToken
  , capturedRequestInput, mergeRequestInputs, aliasRequestInputs, requestInputRetained, retainRequestEncodedBytes, requestInputBytes, revalidateRequestInputs, revalidateRequestInputsWith ) where

import Control.Exception (IOException, try, finally)
import Control.Monad (unless, when, forM_, foldM)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import Control.Concurrent.MVar (newMVar, modifyMVar, modifyMVar_, readMVar)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Numeric (showHex)
import System.Environment (lookupEnv)
import Text.Read (readMaybe)
import Tidepool.BoundedRead (readFileAtMost, FileObservations, FileObservation(..), observeFile, withFileObservations)
import Tidepool.Timing (readTimingEnabled, emitCount)

type RequestInputReader = FilePath -> Int -> IO BS.ByteString
type RequestInputTokenReader = FilePath -> Int -> IO CapturedRequestInput

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
data RequestOriginalInputs = RequestOriginalInputs Integer Integer (Map.Map FilePath CapturedInput) (Map.Map String CapturedInput) (Map.Map FilePath FilePath)
  deriving Eq

instance Show RequestOriginalInputs where
  show (RequestOriginalInputs limit used inputs encoded aliases) = "RequestOriginalInputs "
    ++ show (limit,used,Map.size inputs,Map.size encoded,Map.size aliases)

requestInputBytes :: RequestOriginalInputs -> Integer
requestInputBytes (RequestOriginalInputs _ used _ _ _) = used

-- Content has no path, request allowance or publication authority. A receiving
-- invocation selects its own paths and observations before it can consume it.
newtype CapturedOriginalContent = CapturedOriginalContent (Map.Map (String,Int) CapturedInput)

instance Show CapturedOriginalContent where
  show content = "CapturedOriginalContent " ++ show (capturedOriginalContentBytes content)

emptyCapturedOriginalContent :: CapturedOriginalContent
emptyCapturedOriginalContent = CapturedOriginalContent Map.empty

capturedOriginalContentBytes :: CapturedOriginalContent -> Integer
capturedOriginalContentBytes (CapturedOriginalContent content) =
  sum [toInteger (BS.length bytes) | CapturedInput bytes _ <- Map.elems content]

capturedOriginalContentKeys :: CapturedOriginalContent -> Set.Set (String,Int)
capturedOriginalContentKeys (CapturedOriginalContent content) = Map.keysSet content

selectCapturedOriginalContent :: Set.Set (String,Int) -> CapturedOriginalContent -> CapturedOriginalContent
selectCapturedOriginalContent selected (CapturedOriginalContent content) =
  CapturedOriginalContent (Map.restrictKeys content selected)

mergeCapturedOriginalContent :: CapturedOriginalContent -> CapturedOriginalContent -> CapturedOriginalContent
mergeCapturedOriginalContent (CapturedOriginalContent selected) (CapturedOriginalContent previous) =
  CapturedOriginalContent (Map.union selected previous)

data OriginalInputReference = OriginalInputReference
  { originalInputPath :: FilePath
  , originalInputSha256 :: String
  , originalInputLength :: Int
  , originalInputOrigins :: [FilePath]
  } deriving (Eq, Show)

requestCaptureByteLimit :: IO Integer
requestCaptureByteLimit = do
  configured <- lookupEnv "TIDEPOOL_REQUEST_CAPTURE_BYTES"
  case configured of
    Nothing -> pure (4 * 1024 * 1024 * 1024)
    Just value -> case readMaybe value of
      Just amount | amount > 0 -> pure amount
      _ -> fail "TIDEPOOL_REQUEST_CAPTURE_BYTES must be a positive integer"

-- Continuation imports selected content into a new allowance and observation
-- set. A missing worker copy is reconstructed only from the offered immutable
-- materialization; historical request paths never become fallback inputs.
continueRequestInputs :: CapturedOriginalContent -> [OriginalInputReference]
  -> IO RequestOriginalInputs
continueRequestInputs (CapturedOriginalContent content) references = do
  limit <- requestCaptureByteLimit
  fst <$> foldM select (RequestOriginalInputs limit 0 Map.empty Map.empty Map.empty,content) references
  where
    select (receiving@(RequestOriginalInputs limit used files encoded aliases),available) reference = do
      timing <- readTimingEnabled
      let path = originalInputPath reference
          sha = originalInputSha256 reference
          count = originalInputLength reference
      when (count < 0 || count == maxBound) (fail "invalid original input length")
      input <- case Map.lookup (sha,count) available of
        Just retained -> do
          emitCount timing "original_inputs.content_hits" 1
          emitCount timing "original_inputs.content_hit_bytes" (toInteger count)
          pure retained
        Nothing -> do
          when (used + toInteger count > limit) (fail "continued original inputs exceed receiving byte budget")
          bytes <- readFileAtMost path (count + 1)
          unless (BS.length bytes == count && digest bytes == sha)
            (fail "owned original materialization differs from its admitted bytes")
          emitCount timing "original_inputs.content_misses" 1
          emitCount timing "original_inputs.content_miss_bytes" (toInteger count)
          pure (CapturedInput bytes sha)
      selected <- case Map.lookup (Map.findWithDefault path path aliases) files of
        Just old | old == input -> pure receiving
        Just _ -> fail "continued original inputs conflict at one receiving path"
        Nothing -> do
          when (used + toInteger count > limit) (fail "continued original inputs exceed receiving byte budget")
          pure (RequestOriginalInputs limit (used + toInteger count)
            (Map.insert path input files) encoded aliases)
      withOrigins <- either fail pure (aliasRequestInputs
        [(origin,path,sha) | origin <- originalInputOrigins reference] selected)
      -- Equal image parts share one backing store even on a cold receiving
      -- request. Path obligations and the receiving charge remain independent.
      pure (withOrigins,Map.insert (sha,count) input available)

selectedOriginalContent :: [OriginalInputReference] -> RequestOriginalInputs
  -> Either String CapturedOriginalContent
selectedOriginalContent references (RequestOriginalInputs _ _ files _ aliases) =
  CapturedOriginalContent . Map.fromList <$> mapM select references
  where
    select reference = case Map.lookup
        (Map.findWithDefault (originalInputPath reference) (originalInputPath reference) aliases) files of
      Just input@(CapturedInput bytes sha)
        | sha == originalInputSha256 reference && BS.length bytes == originalInputLength reference ->
          Right ((sha,BS.length bytes),input)
      _ -> Left "selected original content lacks its exact receiving capture"

-- Each extension shares the preceding immutable map. The budget charges every
-- retained encoded byte, including graphs, Core and home package sidecars;
-- GHC's separately retained decoded structures are not charged as encoded bytes.
captureRequestInputs :: Maybe RequestOriginalInputs -> (RequestInputReader -> IO a)
  -> IO (a, RequestOriginalInputs)
captureRequestInputs previous action = captureRequestInputTokens previous $ \readToken ->
  action (\path bound -> capturedInputBytes <$> readToken path bound)

captureRequestInputTokens :: Maybe RequestOriginalInputs -> (RequestInputTokenReader -> IO a)
  -> IO (a, RequestOriginalInputs)
captureRequestInputTokens previous action = do
  initial <- case previous of
    Just inputs -> pure inputs
    Nothing -> do
      limit <- requestCaptureByteLimit
      pure (RequestOriginalInputs limit 0 Map.empty Map.empty Map.empty)
  state <- newMVar (True,initial)
  let readInput path bound = modifyMVar state $ \(open,originals@(RequestOriginalInputs limit used inputs encoded aliases)) -> do
        unless open (fail "request admission reader used after sealing")
        when (bound < 0) (fail "negative request input bound")
        case Map.lookup (Map.findWithDefault path path aliases) inputs of
          Just (CapturedInput bytes sha) -> do
            when (BS.length bytes > bound) (fail "captured request input exceeds its consumer byte bound")
            pure ((open,originals),CapturedRequestInput bytes sha)
          Nothing -> do
            let available = min (toInteger (min bound (maxBound - 1))) (limit - used)
            bytes <- readFileAtMost path (fromInteger available + 1)
            when (toInteger (BS.length bytes) > available)
              (fail "request original input capture exceeds its byte budget or artifact bound")
            timing <- readTimingEnabled
            emitCount timing "original_inputs.fresh_file_bytes" (toInteger (BS.length bytes))
            let sha = digest bytes
                captured = CapturedInput bytes sha
                next = RequestOriginalInputs limit (used + toInteger (BS.length bytes))
                  (Map.insert path captured inputs) encoded aliases
            pure ((open,next),CapturedRequestInput bytes sha)
  result <- action readInput `finally` modifyMVar_ state (\(_,inputs) -> pure (False,inputs))
  (_,sealed) <- readMVar state
  pure (result,sealed)

capturedRequestInput :: RequestOriginalInputs -> FilePath -> String -> IO BS.ByteString
capturedRequestInput inputs path expected = capturedInputBytes <$> capturedRequestInputToken inputs path expected

capturedRequestInputToken :: RequestOriginalInputs -> FilePath -> String -> IO CapturedRequestInput
capturedRequestInputToken (RequestOriginalInputs _ _ inputs _ aliases) path expected =
  case Map.lookup (Map.findWithDefault path path aliases) inputs of
    Just (CapturedInput bytes actual) | actual == expected -> pure (CapturedRequestInput bytes actual)
    _ -> fail "request input is absent or differs from its admitted seal"

-- Transfer existing captures into the receiving request's budget. Equal paths
-- share their retained payload; conflicting captures cannot replace originals.
-- The receiver's allowance governs the union, regardless of donor allowances.
mergeRequestInputs :: RequestOriginalInputs -> [RequestOriginalInputs]
  -> Either String RequestOriginalInputs
mergeRequestInputs = foldM merge
  where
    merge receiving (RequestOriginalInputs _ _ files encoded aliases) = do
      withFiles <- foldM addFile receiving (Map.toAscList files)
      withEncoded <- foldM addEncoded withFiles (Map.toAscList encoded)
      aliasRequestInputs [(alias,path,sha) | (alias,path) <- Map.toAscList aliases
        , Just (CapturedInput _ sha) <- [Map.lookup path files]] withEncoded
    addFile owner@(RequestOriginalInputs limit used files encoded aliases) (path,input@(CapturedInput bytes _)) =
      case Map.lookup (Map.findWithDefault path path aliases) files of
        Just old | old == input -> Right owner
        Just _ -> Left "request capture transfer conflicts with an admitted path"
        Nothing | used + toInteger (BS.length bytes) <= limit ->
          Right (RequestOriginalInputs limit (used + toInteger (BS.length bytes))
            (Map.insert path input files) encoded aliases)
        _ -> Left "request capture transfer exceeds the receiving byte budget"
    addEncoded owner@(RequestOriginalInputs limit used files encoded aliases) (sha,input@(CapturedInput bytes _)) =
      case Map.lookup sha encoded of
        Just old | old == input -> Right owner
        Just _ -> Left "request capture transfer conflicts with an encoded input"
        Nothing | used + toInteger (BS.length bytes) <= limit ->
          Right (RequestOriginalInputs limit (used + toInteger (BS.length bytes))
            files (Map.insert sha input encoded) aliases)
        _ -> Left "request capture transfer exceeds the receiving byte budget"

-- Durable copies of an admitted original share its payload and budget. Aliases
-- are explicit custody transfers; both source and copied paths remain terminal
-- observations, and neither is reopened by intermediate consumers.
aliasRequestInputs :: [(FilePath,FilePath,String)] -> RequestOriginalInputs
  -> Either String RequestOriginalInputs
aliasRequestInputs aliasesToAdd receiving = foldM addAlias receiving aliasesToAdd
  where
    addAlias owner@(RequestOriginalInputs limit used files encoded aliases) (alias,path,sha) = do
      let primary = Map.findWithDefault path path aliases
      input <- case Map.lookup primary files of
        Just value@(CapturedInput _ seal) | seal == sha -> Right value
        _ -> Left "request alias lacks its admitted original seal"
      case Map.lookup (Map.findWithDefault alias alias aliases) files of
        Just old | old == input -> Right owner
        Just _ -> Left "request alias conflicts with an admitted path"
        Nothing -> Right (RequestOriginalInputs limit used files encoded (Map.insert alias primary aliases))

-- An encoded graph issued inside the request has no mutable producer path.
-- Its owning scope retains the bytes and facts together; charge those bytes in
-- the same request budget without introducing a path or a second file store.
retainRequestEncodedBytes :: [BS.ByteString] -> RequestOriginalInputs -> Maybe RequestOriginalInputs
retainRequestEncodedBytes [] owner = Just owner
retainRequestEncodedBytes (bytes:rest) owner@(RequestOriginalInputs limit used files encoded aliases) =
  let sha = digest bytes
  in case Map.lookup sha encoded of
    Just _ -> retainRequestEncodedBytes rest owner
    Nothing | used + toInteger (BS.length bytes) <= limit ->
      retainRequestEncodedBytes rest (RequestOriginalInputs limit
        (used + toInteger (BS.length bytes)) files
        (Map.insert sha (CapturedInput bytes sha) encoded) aliases)
    _ -> Nothing

requestInputRetained :: RequestOriginalInputs -> FilePath -> String -> Bool
requestInputRetained (RequestOriginalInputs _ _ inputs _ aliases) path expected =
  case Map.lookup (Map.findWithDefault path path aliases) inputs of
    Just (CapturedInput _ actual) -> actual == expected
    Nothing -> False

-- Publication observes current producer paths independently of custody. A
-- transient mutation cannot change consumption, and persistent drift refuses
-- terminal publication. Async exceptions are not converted into validation.
revalidateRequestInputs :: RequestOriginalInputs -> IO (Either String ())
revalidateRequestInputs inputs = withFileObservations (\observations -> revalidateRequestInputsWith observations inputs)

revalidateRequestInputsWith :: FileObservations -> RequestOriginalInputs -> IO (Either String ())
revalidateRequestInputsWith observations (RequestOriginalInputs _ _ inputs _ aliases) = do
  let paths = Map.union inputs (Map.mapMaybe (`Map.lookup` inputs) aliases)
  result <- try $ forM_ (Map.toAscList paths) $ \(path,CapturedInput bytes sha) -> do
    current <- observeFile observations path (Just (BS.length bytes))
    unless (observedByteCount current == BS.length bytes && observedSha256 current == sha)
      (fail "request original input changed before publication")
  pure (either (Left . ("request original input changed before publication: " ++) . show) Right (result :: Either IOException ()))

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let rendered = showHex byte ""
  in replicate (2 - length rendered) '0' ++ rendered) . BS.unpack . SHA.hash
