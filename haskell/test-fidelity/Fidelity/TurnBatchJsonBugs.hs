{-# LANGUAGE ScopedTypeVariables #-}

-- | Targeted regressions for the three plan.json JSON-parser bugs fixed in
-- @app/Main.hs@ (~1108-1240): (1) numbers parsed as integers only, with no
-- fraction/exponent support; (2) raw (unescaped) control characters were
-- accepted inside string literals, which JSON requires to be escaped; (3)
-- UTF-16 surrogate-pair @\\u@ escapes decoded as two lone, wrong code
-- points instead of the single astral character they encode.
--
-- Drives the ACTUAL @tidepool-extract-bin@ PRODUCTION BINARY as a
-- subprocess, exactly like 'Fidelity.TurnBatch' and for the same reason:
-- the plan.json parser lives in @app/Main.hs@, not
-- @tidepool-extract-internal@ — a library-level call cannot reach it.
module Fidelity.TurnBatchJsonBugs (checks) where

import Fidelity.Harness (Check, check)
import Tidepool.ExtractRequest (RequestField(..), workerArgv)

import qualified Data.ByteString as BS
import Data.List (isInfixOf)
import System.Directory
  ( createDirectoryIfMissing, doesDirectoryExist, removeDirectoryRecursive )
import System.Environment (lookupEnv, getEnvironment)
import System.Exit (ExitCode(..))
import System.Process (proc, readProcess, readCreateProcessWithExitCode, CreateProcess(..))

workDir :: FilePath
workDir = "test-fidelity/work/turn-batch-json-bugs"

declTemplateSrc :: String
declTemplateSrc = "module Input where\n{{TURN}}\n"

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

-- | Run @--turn-batch@ on a hand-written @plan.json@ (bytes given raw, so a
-- test can embed a literal control byte or a \\u escape verbatim) against a
-- single "decl" template, and return (exit code, stdout, path to the
-- written i0/TurnItem0.hs module source, if any).
runBatch :: String -> BS.ByteString -> IO (ExitCode, String, FilePath)
runBatch tag planBytes = do
  let dir      = workDir ++ "/" ++ tag
      sessionRoot = dir ++ "/session-root"
      batchOut = dir ++ "/out"
      declTmpl = dir ++ "/tmpl-decl.hs"
      planPath = dir ++ "/plan.json"
      modPath  = batchOut ++ "/i0/TurnItem0.hs"
  removeDirIfExists dir
  createDirectoryIfMissing True sessionRoot
  writeFile declTmpl declTemplateSrc
  BS.writeFile planPath planBytes
  binPath <- resolveExtractBin
  env0 <- getEnvironment
  let args = workerArgv
        [ TurnBatch planPath, BatchOut batchOut, TurnTemplate "decl" declTmpl ]
      cp = (proc binPath args) { env = Just env0 }
  (exitCode, out, _err) <- readCreateProcessWithExitCode cp ""
  pure (exitCode, out, modPath)

-- | A plan.json's items entry with @extra@ raw JSON text spliced in after
-- the well-formed fields, and @turnText@ substituted verbatim (already
-- JSON-string-escaped by the caller) as the item's @turn_text@.
itemJson :: FilePath -> String -> String -> String
itemJson sessionRoot turnText extra =
  "{\"index\":0,\"turn_text\":\"" ++ turnText ++ "\",\"verdict\":{\"kind\":\"decl\",\"binders\":[]}"
  ++ ",\"template\":\"decl\",\"session_root\":" ++ show sessionRoot
  ++ ",\"inject_vals\":[],\"bind_gen\":1" ++ extra ++ "}"

checks :: IO [Check]
checks = do
  fracChecks <- checkFractionExponent
  ctrlChecks <- checkRawControlChar
  surrChecks <- checkSurrogatePair
  pure (fracChecks ++ ctrlChecks ++ surrChecks)

-- | Bug 1: numbers parsed as integers only. An item carrying extra
-- fractional/exponent-shaped fields that no accessor reads ("score":1.5e2,
-- "neg":-0.25) must not blow up the WHOLE parse — before the fix, any
-- fraction/exponent anywhere in plan.json (even in an unused field) hard-
-- failed with "trailing content"/"expected ',' or '}'".
checkFractionExponent :: IO [Check]
checkFractionExponent = do
  let dir = workDir ++ "/frac-exp"
      sessionRoot = dir ++ "/session-root"
      plan = "{\"version\":1,\"items\":[" ++
             itemJson sessionRoot "x = 1" ",\"score\":1.5e2,\"neg\":-0.25" ++
             "]}"
  (exitCode, out, _) <- runBatch "frac-exp" (utf8 plan)
  pure
    [ check ("fraction/exponent field alongside real fields: batch succeeds — exit=" ++ show exitCode ++ " out=" ++ oneLine out)
        (exitCode == ExitSuccess)
    , check ("fraction/exponent field alongside real fields: item 0 reported ok — out=" ++ oneLine out)
        ("\"index\":0,\"status\":\"ok\"" `isInfixOf` filter (/= ' ') out)
    ]

-- | Bug 2: raw control characters were accepted where JSON requires them
-- escaped. A literal (unescaped) TAB byte inside turn_text's JSON string
-- must be rejected as malformed plan.json, not silently threaded through.
checkRawControlChar :: IO [Check]
checkRawControlChar = do
  let dir = workDir ++ "/raw-ctrl"
      sessionRoot = dir ++ "/session-root"
      plan = "{\"version\":1,\"items\":[" ++
             itemJson sessionRoot "x\t = 1" "" ++
             "]}"
  (exitCode, out, _) <- runBatch "raw-ctrl" (utf8 plan)
  pure
    [ check ("raw control char in a string literal: batch fails loudly — exit=" ++ show exitCode ++ " out=" ++ oneLine out)
        (exitCode /= ExitSuccess)
    , check ("raw control char in a string literal: error names the control character — out=" ++ oneLine out)
        ("invalid control character" `isInfixOf` out)
    ]

-- | Bug 3: UTF-16 surrogate pairs decoded incorrectly. turn_text carries a
-- \\uD83D\\uDE00 escape (U+1F600 GRINNING FACE) inside a comment; the
-- spliced-out module source file must contain the correct 4-byte UTF-8
-- encoding of U+1F600 (F0 9F 98 80), not two mis-decoded lone surrogates.
checkSurrogatePair :: IO [Check]
checkSurrogatePair = do
  let dir = workDir ++ "/surrogate"
      sessionRoot = dir ++ "/session-root"
      plan = "{\"version\":1,\"items\":[" ++
             itemJson sessionRoot "-- marker \\uD83D\\uDE00" "" ++
             "]}"
  (exitCode, out, modPath) <- runBatch "surrogate" (utf8 plan)
  modBytes <- BS.readFile modPath
  let expectedEmoji = BS.pack [0xF0, 0x9F, 0x98, 0x80]  -- UTF-8 for U+1F600
  pure
    [ check ("surrogate-pair escape: batch succeeds — exit=" ++ show exitCode ++ " out=" ++ oneLine out)
        (exitCode == ExitSuccess)
    , check "surrogate-pair escape: spliced module source contains the correctly-decoded astral character (UTF-8 F0 9F 98 80)"
        (expectedEmoji `bsIsInfixOf` modBytes)
    ]
  where
    bsIsInfixOf needle hay = any (needle `BS.isPrefixOf`) (BS.tails hay)

oneLine :: String -> String
oneLine = unwords . words

-- | UTF-8 encode a plain-'Char'-only 'String' (every 'Char' used in this
-- module's plan.json literals is ASCII) into 'BS.ByteString' for
-- 'BS.writeFile', without pulling in @text@/@bytestring@'s own UTF-8
-- encoders for a one-off ASCII case.
utf8 :: String -> BS.ByteString
utf8 = BS.pack . map (fromIntegral . fromEnum)
