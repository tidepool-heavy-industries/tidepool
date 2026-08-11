{-# LANGUAGE ScopedTypeVariables #-}

-- | Per-item error attribution for @--turn-batch@
-- (plans/post-restart/batch-turns-feasibility.md §8, batch-extract lane
-- DONE criteria: "a Haskell-side test proving per-item error attribution").
--
-- Drives the ACTUAL @tidepool-extract-bin@ PRODUCTION BINARY as a
-- subprocess, mirroring 'Fidelity.D1Defense' exactly and for the same
-- reason: the checked mechanism (@runTurnBatchMode@, plan.json parsing, the
-- §8 stdout document) lives in @app/Main.hs@, not
-- @tidepool-extract-internal@ (the library 'Fidelity.Harness' calls into
-- directly) — a library-level call cannot reach it.
--
-- A 3-item plan whose MIDDLE item (index 1) fails to typecheck must: (a)
-- leave item 0's output directory COMPLETE (its own @result.cbor@ /
-- @meta.cbor@ / @asks.json@ / @turn.cbor@, byte-identical-single-turn
-- shape); (b) leave item 2's directory entirely WITHOUT compile output (it
-- never ran); (c) attribute the failure's real diagnostics to index 1 in
-- the §8 stdout document's @items@ array, with the flat top-level
-- @diagnostics@ array carrying the same message (the un-upgraded-reader
-- contract).
--
-- Deliberately avoids @Tidepool.Prelude@/@--include lib@ (unlike the batch
-- mode's own manual smoke test) so this check has no dependency on the
-- with-packages GHC's extra package set (@lens@/@safe@/...) — a bare `Int`
-- binding under GHC's ordinary Prelude is enough to exercise the session
-- compile path (an empty home-package dependency closure is a degenerate
-- but valid case of it).
module Fidelity.TurnBatch (checks) where

import Fidelity.Harness (Check, check)

import Data.List (isInfixOf, isPrefixOf)
import System.Directory
  ( createDirectoryIfMissing, doesFileExist, doesDirectoryExist, removeDirectoryRecursive )
import System.Environment (lookupEnv, getEnvironment)
import System.Exit (ExitCode(..))
import System.Process (proc, readProcess, readCreateProcessWithExitCode, CreateProcess(..))

workDir :: FilePath
workDir = "test-fidelity/work/turn-batch"

declTemplateSrc :: String
declTemplateSrc = "module Input where\n{{TURN}}\n"

bindTemplateSrc :: String
bindTemplateSrc = "module Input where\n__result :: Int\n__result = {{TURN_STMT}} in {{BINDERS}}\n"

-- | The 3-item plan: item 0 and item 2 are ordinary binds that would each
-- succeed on their own; item 1 references an out-of-scope identifier and
-- must fail to typecheck. Escaped by hand (fixed, controlled content — no
-- need for a JSON encoder here).
planJson :: FilePath -> String
planJson root = unlines
  [ "{\"version\":1,\"items\":["
  , "  {\"index\":0,\"turn_text\":\"let a = 111\",\"verdict\":{\"kind\":\"bind\",\"binders\":[\"a\"]},\"template\":\"bind\",\"session_root\":" ++ show root ++ ",\"inject_vals\":[],\"bind_gen\":1},"
  , "  {\"index\":1,\"turn_text\":\"let b = undefinedIdentifierXYZ 5\",\"verdict\":{\"kind\":\"bind\",\"binders\":[\"b\"]},\"template\":\"bind\",\"session_root\":" ++ show root ++ ",\"inject_vals\":[],\"bind_gen\":2},"
  , "  {\"index\":2,\"turn_text\":\"let c = 333\",\"verdict\":{\"kind\":\"bind\",\"binders\":[\"c\"]},\"template\":\"bind\",\"session_root\":" ++ show root ++ ",\"inject_vals\":[],\"bind_gen\":3}"
  , "]}"
  ]

-- | Resolve the built @tidepool-extract-bin@ (same recipe as
-- 'Fidelity.D1Defense.resolveExtractBin' — duplicated rather than shared, per
-- that module's own "each check owns a work-dir tag" isolation, and since
-- neither is exported from the other).
resolveExtractBin :: IO FilePath
resolveExtractBin = do
  mEnv <- lookupEnv "TIDEPOOL_EXTRACT"
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

checks :: IO [Check]
checks = do
  binPath <- resolveExtractBin
  removeDirIfExists workDir
  createDirectoryIfMissing True workDir
  let sessionRoot = workDir ++ "/session-root"
      batchOut    = workDir ++ "/out"
      declTmpl    = workDir ++ "/tmpl-decl.hs"
      bindTmpl    = workDir ++ "/tmpl-bind.hs"
      planPath    = workDir ++ "/plan.json"
  createDirectoryIfMissing True sessionRoot
  writeFile declTmpl declTemplateSrc
  writeFile bindTmpl bindTemplateSrc
  writeFile planPath (planJson sessionRoot)

  env0 <- getEnvironment
  let args =
        [ "--turn-batch", planPath, "--batch-out", batchOut
        , "--turn-template", "decl=" ++ declTmpl
        , "--turn-template", "bind=" ++ bindTmpl
        ]
      cp = (proc binPath args) { env = Just env0 }
  (exitCode, out, _err) <- readCreateProcessWithExitCode cp ""

  i0Result <- doesFileExist (batchOut ++ "/i0/result.cbor")
  i0Meta   <- doesFileExist (batchOut ++ "/i0/meta.cbor")
  i0Asks   <- doesFileExist (batchOut ++ "/i0/asks.json")
  i0Turn   <- doesFileExist (batchOut ++ "/i0/turn.cbor")
  i1Turn   <- doesFileExist (batchOut ++ "/i1/turn.cbor")
  i2Dir    <- doesDirectoryExist (batchOut ++ "/i2")
  i2AnyOut <- (||) <$> doesFileExist (batchOut ++ "/i2/turn.cbor")
                    <*> doesFileExist (batchOut ++ "/i2/result.cbor")

  pure
    [ check ("turn-batch: process exits non-zero on a mid-batch failure — exit=" ++ show exitCode)
        (exitCode /= ExitSuccess)
    , check "turn-batch: item 0 (before the failure) has a COMPLETE single-turn output set"
        (i0Result && i0Meta && i0Asks && i0Turn)
    , check "turn-batch: item 1 (the failing item) wrote NO turn.cbor (never finished compiling)"
        (not i1Turn)
    , check "turn-batch: item 2 (after the failure) produced no compile output"
        (not i2AnyOut)
    , check ("turn-batch: stdout reports item 0 as \"ok\" — stdout: " ++ oneLine out)
        ("\"index\":0,\"status\":\"ok\"" `isInfixOf` filter (/= ' ') out)
    , check ("turn-batch: stdout reports item 1 as \"failed\" — stdout: " ++ oneLine out)
        ("\"index\":1,\"status\":\"failed\"" `isInfixOf` filter (/= ' ') out)
    , check "turn-batch: stdout does NOT mention item 2 at all (absent, not \"ok\" or \"failed\")"
        (not ("\"index\":2" `isInfixOf` filter (/= ' ') out))
    , check ("turn-batch: item 1's own diagnostics name the real out-of-scope identifier — stdout: " ++ oneLine out)
        ("undefinedIdentifierXYZ" `isInfixOf` out)
    , check "turn-batch: the flat top-level diagnostics array (everything before \"items\":) ALSO carries the failing item's message (un-upgraded-reader contract)"
        ("undefinedIdentifierXYZ" `isInfixOf` takeWhileNotItems out)
    , check "turn-batch: an OLD version-1 reader still sees \"version\":1 at the front"
        ("{\"version\":1," `isPrefixOf` out)
    ]
  where
    oneLine = unwords . words
    -- The prefix of the stdout document up to (not including) the "items"
    -- key — i.e. just the flat top-level "diagnostics" array's own text.
    takeWhileNotItems s = case breakOnItems s of
      (pre, _) -> pre
    breakOnItems s@(c : cs)
      | "\"items\":" `isPrefixOf` s = ([], s)
      | otherwise = let (pre, rest) = breakOnItems cs in (c : pre, rest)
    breakOnItems [] = ([], [])
