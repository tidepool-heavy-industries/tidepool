-- | Small utilities shared across the extractor's GHC-session plumbing and
-- the harness entry point. No dependency on the rest of the internal
-- library, so anything pulling in 'GHC.Pipeline'/'Binders' for one of these
-- can depend on this instead.
module Tidepool.ExtractUtil
  ( getLibdir
  , capitalize
  ) where

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
