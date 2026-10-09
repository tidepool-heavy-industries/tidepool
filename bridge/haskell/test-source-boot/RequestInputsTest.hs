module RequestInputsTest (requestInputHistories, requestInputBoundaries) where

import Control.Exception (AsyncException(ThreadKilled), IOException, bracket, throwIO, try)
import Control.Monad (foldM, unless)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.FilePath ((</>))
import Test.QuickCheck
import Tidepool.RequestInputs
import SourceBootFixtureSupport (withScratch, digest)

-- The model owns values, independently of the implementation's custody map.
-- Every step writes a producer path, then either captures a new original or
-- consumes an earlier one. Mutations and restoration test both snapshot use and
-- the terminal current-path proof; independent requests see the current bytes.
requestInputHistories :: IO ()
requestInputHistories = do
  result <- quickCheckWithResult stdArgs {maxSuccess=80, maxSize=40} $ \steps ->
    let selected = take 40 (steps :: [(NonNegative Int, [Bool])])
        (extended,changed,restored) = historyFlags selected
    in classify (null selected) "empty history"
      $ classify extended "extend retained owner"
      $ classify changed "producer drift"
      $ classify restored "restore captured input"
      $ ioProperty (history selected)
  unless (isSuccess result) (fail "request input custody history property failed")
  where
    history steps = withScratch $ \directory -> do
      (_,empty) <- captureRequestInputs Nothing (const (pure ()))
      (_,model,owner) <- foldM (step directory) (Map.empty,Map.empty,empty) steps
      observed <- revalidateRequestInputs owner
      let current = fst3Result steps
      -- The final proof is independently determined by the disk model, rather
      -- than by the owner itself or by its readback result.
      pure (either (const False) (const True) observed == (current == model))
    fst3Result = foldl (\current (NonNegative key,bits) -> Map.insert (key `mod` 5) (payload bits) current) Map.empty
    step directory (current,model,owner) (NonNegative key,bits) = do
      let index = key `mod` 5
          path = directory </> show index
          bytes = payload bits
      BS.writeFile path bytes
      (captured,next) <- captureRequestInputs (Just owner) (\reader -> reader path 64)
      let expected = Map.findWithDefault bytes index model
          admitted = Map.insertWith (\_ old -> old) index bytes model
          updated = Map.insert index bytes current
      unless (captured == expected) (fail "scope extension consumed a changed admitted original")
      unless (requestInputBytes next == sum (map (toInteger . BS.length) (Map.elems admitted)))
        (fail "scope extension charged retained input bytes again or lost custody accounting")
      retained <- capturedRequestInput next path (digest expected)
      unless (retained == expected) (fail "captured request input lost its admitted byte identity")
      (_,independent) <- captureRequestInputs Nothing (\reader -> reader path 64)
      fresh <- capturedRequestInput independent path (digest bytes)
      unless (fresh == bytes) (fail "independent request inherited another request's snapshot")
      pure (updated,admitted,next)
    historyFlags steps = let (_,_,extended,changed,restored) = foldl observe
          (Map.empty,Map.empty,False,False,False) steps
        in (extended,changed,restored)
      where
        observe (first,current,extended,changed,restored) (NonNegative key,bits) =
          let index = key `mod` 5
              bytes = payload bits
              original = Map.lookup index first
          in (Map.insertWith (\_ old -> old) index bytes first,Map.insert index bytes current
            ,extended || maybe False (const True) original
            ,changed || maybe False (/= bytes) original
            ,restored || (original == Just bytes && Map.lookup index current /= original))
    payload bits = BS.pack [if bit then 1 else 0 | bit <- take 64 bits]

requestInputBoundaries :: IO ()
requestInputBoundaries = withScratch $ \directory -> do
  let path = directory </> "original"
      other = directory </> "other"
      bytes = BS.pack [1,2,3]
  BS.writeFile path bytes
  BS.writeFile other bytes
  (escaped,owner) <- captureRequestInputs Nothing $ \reader -> do
    _ <- reader path 3
    pure reader
  BS.writeFile path (BS.singleton 9)
  stable <- capturedRequestInput owner path (digest bytes)
  changed <- revalidateRequestInputs owner
  unless (stable == bytes && either (const True) (const False) changed)
    (fail "producer replacement changed consumption or passed terminal validation")
  BS.writeFile path bytes
  restored <- revalidateRequestInputs owner
  unless (restored == Right ()) (fail "restored producer bytes remained stale in terminal validation")
  afterSeal <- try (escaped path 3) :: IO (Either IOException BS.ByteString)
  unless (either (const True) (const False) afterSeal) (fail "admission reader escaped its lifetime")
  tooSmall <- try (captureRequestInputs (Just owner) (\reader -> reader path 2))
    :: IO (Either IOException (BS.ByteString,RequestOriginalInputs))
  unless (either (const True) (const False) tooSmall) (fail "retained bytes bypassed a stricter bound")
  wrongSeal <- try (capturedRequestInput owner path (replicate 64 '0')) :: IO (Either IOException BS.ByteString)
  missing <- try (capturedRequestInput owner other (digest bytes)) :: IO (Either IOException BS.ByteString)
  unless (all (either (const True) (const False)) [wrongSeal,missing])
    (fail "snapshot lookup reconstructed missing or conflicting authority from disk")
  let config = "TIDEPOOL_REQUEST_CAPTURE_BYTES"
      restore Nothing = unsetEnv config
      restore (Just value) = setEnv config value
  bracket (lookupEnv config) restore $ \_ -> do
    setEnv config "5"
    (_,bounded) <- captureRequestInputs Nothing (\reader -> reader path 3)
    exceeds <- try (captureRequestInputs (Just bounded) (\reader -> reader other 3))
      :: IO (Either IOException (BS.ByteString,RequestOriginalInputs))
    unless (either (const True) (const False) exceeds) (fail "aggregate capture budget was applied per file")
    unless (requestInputBytes bounded == 3) (fail "encoded input accounting differs from retained bytes")
    unless (case retainRequestEncodedBytes [bytes] bounded of Nothing -> True; _ -> False)
      (fail "new encoded graph bytes bypassed the request aggregate budget")
    let graphBytes = BS.pack [4,5]
    graphOwner <- maybe (fail "bounded encoded graph was refused") pure
      (retainRequestEncodedBytes [graphBytes] bounded)
    unless (requestInputBytes graphOwner == 5
        && fmap requestInputBytes (retainRequestEncodedBytes [graphBytes] graphOwner) == Just 5)
      (fail "encoded graph generations lost accounting or charged the same graph again")
    setEnv config "0"
    invalid <- try (captureRequestInputs Nothing (const (pure ())))
      :: IO (Either IOException ((),RequestOriginalInputs))
    unless (either (const True) (const False) invalid) (fail "invalid capture budget silently selected a default")
  cancelled <- try (captureRequestInputs Nothing (\_ -> throwIO ThreadKilled))
    :: IO (Either AsyncException ((),RequestOriginalInputs))
  unless (case cancelled of Left ThreadKilled -> True; _ -> False)
    (fail "capture admission converted asynchronous cancellation")
