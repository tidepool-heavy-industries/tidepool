{-# LANGUAGE ScopedTypeVariables #-}

-- | Mutation test for the pre-write metadata coverage contract.
--
-- The fixture emits @Data.List.NonEmpty.:|@. A child-local fault removes that
-- constructor from the final metadata table after all collectors contribute;
-- extraction must fail before writing the target artifact.
module Fidelity.MetadataCoverage (checks) where

import Fidelity.Harness (Check, check)
import Tidepool.ExtractRequest (RequestField(..), workerArgv)

import Data.List (isInfixOf)
import System.Directory (createDirectoryIfMissing, doesFileExist, doesDirectoryExist, removeDirectoryRecursive)
import System.Environment (lookupEnv, getEnvironment)
import System.Exit (ExitCode(..))
import System.Process (proc, readProcess, readCreateProcessWithExitCode, CreateProcess(..))

workDir :: FilePath
workDir = "test-fidelity/work/metadata-coverage"

-- | The GHC 9.12 home of 'Data.List.NonEmpty'\'s @(:|)@ constructor.
dropTarget :: String
dropTarget = "GHC.Internal.Base.:|"

-- | @stash@ keeps the constructor observable in optimized Core. The mutation
-- targets the completed table, including metadata from transitive types.
fixtureSrc :: String
fixtureSrc = unlines
  [ "module MetadataCoverageFixture (target) where"
  , ""
  , "import Data.List.NonEmpty (NonEmpty((:|)))"
  , ""
  , "{-# NOINLINE stash #-}"
  , "stash :: [a] -> a"
  , "stash (x:_) = x"
  , "stash []    = errorWithoutStackTrace \"empty\""
  , ""
  , "target :: Int"
  , "target = case stash [5 :| []] of"
  , "  (n :| _) -> n"
  ]

-- | Resolve the built compiler worker. The fidelity suite drives the private
-- protocol directly so failures exercise the Haskell executable boundary.
resolveExtractBin :: IO FilePath
resolveExtractBin = do
  mEnv <- lookupEnv "TIDEPOOL_EXTRACT_WORKER"
  case mEnv of
    Just p  -> pure p
    Nothing -> do
      _ <- readProcess "cabal" ["build", "tidepool-extract-bin"] ""
      trim <$> readProcess "cabal" ["list-bin", "tidepool-extract-bin"] ""
  where trim = reverse . dropWhile (`elem` ("\n " :: String)) . reverse

removeDirIfExists :: FilePath -> IO ()
removeDirIfExists dir = do
  exists <- doesDirectoryExist dir
  if exists then removeDirectoryRecursive dir else pure ()

-- | Run the real worker with an optional, child-local metadata fault.
runExtract :: FilePath -> FilePath -> Maybe String -> IO (ExitCode, String, String)
runExtract binPath outDir mDropDC = do
  removeDirIfExists outDir
  createDirectoryIfMissing True outDir
  baseEnv <- getEnvironment
  let scrubbed = filter ((/= "TIDEPOOL_TEST_DROP_DC") . fst) baseEnv
      env' = maybe scrubbed (\dc -> ("TIDEPOOL_TEST_DROP_DC", dc) : scrubbed) mDropDC
      args = workerArgv [Input (workDir ++ "/MetadataCoverageFixture.hs"), Target "target", OutputDir outDir]
      cp = (proc binPath args) { env = Just env' }
  readCreateProcessWithExitCode cp ""

checks :: IO [Check]
checks = do
  binPath <- resolveExtractBin
  createDirectoryIfMissing True workDir
  writeFile (workDir ++ "/MetadataCoverageFixture.hs") fixtureSrc

  -- First prove the unmodified metadata path succeeds.
  let outUnset = workDir ++ "/out-unset"
  (unsetCode, unsetOut, unsetErr) <- runExtract binPath outUnset Nothing
  unsetResultExists <- doesFileExist (outUnset ++ "/target.cbor")

  -- Then remove one required constructor and require a pre-write failure.
  let outSet = workDir ++ "/out-set"
  (setCode, setOut, setErr) <- runExtract binPath outSet (Just dropTarget)
  setResultExists <- doesFileExist (outSet ++ "/target.cbor")

  pure
    [ check ("metadata mutation: control extracts successfully — exit="
              ++ show unsetCode ++ " stderr=" ++ oneLine unsetErr)
        (unsetCode == ExitSuccess && unsetResultExists)
    , check ("metadata mutation: dropping " ++ dropTarget ++ " fails extraction — exit=" ++ show setCode)
        (setCode /= ExitSuccess)
    , check "metadata mutation writes no target.cbor"
        (not setResultExists)
    , check ("metadata mutation reports the artifact contract — stdout+stderr: "
              ++ oneLine (setOut ++ setErr))
        ("artifact metadata contract failed" `isInfixOf` (setOut ++ setErr))
    ]
  where
    oneLine = unwords . words
