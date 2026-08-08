{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE LambdaCase #-}

-- | Shared plumbing for the extract-fidelity checks: compile a synthetic
-- module through the REAL one-shot pipeline ('runPipeline') and translate one
-- of its top-level bindings to closed Core, exactly as the extract binary
-- does. A check asserts on the resulting 'FlatNode's, or on the extraction
-- error text when the contract is "fail loud".
--
-- Each check owns a work-dir tag; two checks sharing a tag would race on the
-- same .hs/.hi files.
module Fidelity.Harness
  ( Check
  , check
  , extractBinding
  , extractError
  , nodeList
  , nvarIds
  , nlamCount
  , getLibdir
  ) where

import Tidepool.GhcPipeline (runPipeline, PipelineResult(..))
import Tidepool.Translate (translateModuleClosed, ClosedModule(..), FlatNode(..))

import Control.Exception (try, SomeException, evaluate)
import Data.Foldable (toList)
import Data.List (isInfixOf)
import Data.Word (Word64)
import System.Directory (createDirectoryIfMissing)
import System.Environment (lookupEnv)
import System.Process (readProcess)

-- | A labelled boolean. 'Main' prints these and exits non-zero on any 'False'.
type Check = (String, Bool)

check :: String -> Bool -> Check
check = (,)

workRoot :: FilePath
workRoot = "test-fidelity/work"

getLibdir :: IO FilePath
getLibdir = lookupEnv "TIDEPOOL_GHC_LIBDIR" >>= \case
  Just d  -> pure d
  Nothing -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse

-- | Write @<modName>.hs@ into a per-check work dir, compile it through
-- 'runPipeline', and translate @target@ to closed Core. 'Left' carries the
-- extraction error text (the translator's hard failures are plain 'error's).
extractBinding
  :: String              -- ^ work-dir tag, unique per check
  -> String              -- ^ module name == file basename
  -> String              -- ^ module source
  -> String              -- ^ target top-level binder
  -> IO (Either String ClosedModule)
extractBinding tag modName src target = do
  let dir  = workRoot ++ "/" ++ tag
      path = dir ++ "/" ++ modName ++ ".hs"
  createDirectoryIfMissing True dir
  writeFile path src
  r <- try $ do
    res <- runPipeline path [dir, "lib"]
    cm  <- translateModuleClosed (prHscEnv res) (prBinds res) target
    _   <- evaluate (length (nodeList cm))
    pure cm
  pure $ case r of
    Left (e :: SomeException) -> Left (oneLine (show e))
    Right cm                  -> Right cm
  where
    oneLine = unwords . words

-- | 'extractBinding' specialised to the fail-loud contract: 'True' when
-- extraction failed AND the error text mentions @needle@.
extractError :: String -> String -> String -> String -> String -> IO (Bool, String)
extractError tag modName src target needle =
  extractBinding tag modName src target >>= \case
    Left err -> pure (needle `isInfixOf` err, err)
    Right _  -> pure (False, "<extraction SUCCEEDED — expected a loud failure>")

nodeList :: ClosedModule -> [FlatNode]
nodeList = toList . cmNodes

nvarIds :: ClosedModule -> [Word64]
nvarIds cm = [ v | NVar v <- nodeList cm ]

-- | How many runtime lambdas the binding actually emits — the erasure
-- contract's observable: an erased binder emits no 'NLam'.
nlamCount :: ClosedModule -> Int
nlamCount cm = length [ () | NLam _ _ <- nodeList cm ]
