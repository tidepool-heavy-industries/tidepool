-- | Small utilities shared across the extractor's GHC-session plumbing and
-- the harness entry point. No dependency on the rest of the internal
-- library, so anything pulling in 'GHC.Pipeline'/'Binders' for one of these
-- can depend on this instead.
module Tidepool.ExtractUtil
  ( getLibdir
  , shaHex
  , capitalize
  , trySynchronous
  ) where

import Control.Exception (SomeAsyncException, SomeException, fromException, throwIO, try)
import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import Numeric (showHex)
import Data.Char (toUpper)
import System.Environment (lookupEnv)
import System.Process (readProcess)

-- | The GHC lib directory: @$TIDEPOOL_GHC_LIBDIR@ if set, else
-- @ghc --print-libdir@.
getLibdir :: IO FilePath
getLibdir = do
  envDir <- lookupEnv "TIDEPOOL_GHC_LIBDIR"
  case envDir of
    Just dir -> pure dir
    Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse

-- | Upper-case the first character. Used to derive a module name from a
-- file basename.
capitalize :: String -> String
capitalize [] = []
capitalize (c:cs) = toUpper c : cs

shaHex :: BS.ByteString -> String
shaHex = concatMap (\byte -> let text = showHex byte "" in
  replicate (2 - length text) '0' ++ text) . BS.unpack . SHA256.hash

-- | Catch ordinary failures while allowing cancellation and other asynchronous
-- exceptions to escape their caller's failure policy.
trySynchronous :: IO a -> IO (Either SomeException a)
trySynchronous action = do
  result <- try action
  case result of
    Left exception -> case fromException exception :: Maybe SomeAsyncException of
      Just async -> throwIO async
      Nothing -> pure (Left exception)
    Right value -> pure (Right value)
