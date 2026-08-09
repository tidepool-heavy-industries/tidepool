{-# LANGUAGE ScopedTypeVariables #-}

-- | D1 mutation test (plans/post-restart/extract-wave/spawn-latency/00-spec.md,
-- codex-review-2026-08-08.md item 7): proves the hard-fail metadata-subset
-- defense ('Main.assertMetaCoversEmitted' in @app/Main.hs@) is REAL, not
-- theatre. Drives the ACTUAL @tidepool-extract-bin@ PRODUCTION BINARY as a
-- subprocess — the defense lives in the executable component, not
-- @tidepool-extract-internal@ (the library every other check group in this
-- suite depends on), so a library-level 'Fidelity.Harness.extractBinding'
-- call cannot reach it.
--
-- The fixture's only supplier of the imported, non-wired-in constructor
-- @Data.List.NonEmpty.:|@ is 'Tidepool.Translate.recordDC' — see the fixture
-- source below for why. With @TIDEPOOL_TEST_DROP_DC@ unset, extraction of
-- that exact module succeeds (the anti-vacuity control: the SAME module,
-- undropped, works). With it set to the constructor's qualified name,
-- extraction must FAIL with CHECK A's message and write no @target.cbor@.
module Fidelity.D1Defense (checks) where

import Fidelity.Harness (Check, check)

import Data.List (isInfixOf)
import System.Directory (createDirectoryIfMissing, doesFileExist, doesDirectoryExist, removeDirectoryRecursive)
import System.Environment (lookupEnv, getEnvironment)
import System.Exit (ExitCode(..))
import System.Process (proc, readProcess, readCreateProcessWithExitCode, CreateProcess(..))

workDir :: FilePath
workDir = "test-fidelity/work/d1-defense"

-- | The constructor this test drops. Its module is the current base-split
-- home of 'Data.List.NonEmpty'\'s @(:|)@ on this toolchain (GHC 9.12,
-- confirmed against this fixture via TIDEPOOL_DUMP_CLOSED — NOT guessed).
dropTarget :: String
dropTarget = "GHC.Internal.Base.:|"

-- | A plain @data@ constructor (NOT a newtype — a newtype erases entirely
-- via coercion and never reaches Core as a real Con/Case, so dropping its
-- recordDC call would be vacuous in the OTHER direction: nothing to catch).
-- Built and matched with NO typeclass involved (no Semigroup/Foldable/Show),
-- so no instance dictionary — itself a reachable top-level bind with a
-- concretely @NonEmpty@-mentioning type — supplies it via
-- 'Tidepool.Translate.collectTransitiveDCons'. 'stash' is the load-bearing
-- trick: a locally defined, NOINLINE, fully polymorphic identity
-- (@[a] -> a@). Its own signature never mentions 'Data.List.NonEmpty.NonEmpty',
-- so 'collectTransitiveDCons' (which walks only TOP-LEVEL binders' declared
-- types) never sees it either. NOINLINE also keeps GHC's simplifier from
-- folding @case stash [5 :| []] of (n :| _) -> n@ down to @5@ via
-- case-of-known-constructor (which would erase the constructor from Core
-- entirely before extraction ever sees it, since the scrutinee would no
-- longer be an opaque call). Net effect: 'Tidepool.Translate.recordDC' is
-- this constructor's ONLY supplier across wiredInMeta / tyconMeta / usedMeta
-- / transitiveMeta.
fixtureSrc :: String
fixtureSrc = unlines
  [ "module D1Fixture (target) where"
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

-- | Resolve the built @tidepool-extract-bin@: prefer TIDEPOOL_EXTRACT (same
-- env var the Rust runtime and battery scripts use, so a caller that already
-- built one doesn't pay to rebuild); otherwise build + resolve it via cabal
-- (the same local-iteration recipe haskell/CLAUDE.md documents).
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

-- | Run the real binary in whole-module mode (@--target target@, the
-- 'Main.writeWholeModuleClosed' lane CHECK A/B guard), optionally with
-- @TIDEPOOL_TEST_DROP_DC@ set. Passed via the child's environment (not a
-- global 'System.Environment.setEnv'), so this never races the other check
-- groups sharing this test-suite process.
runExtract :: FilePath -> FilePath -> Maybe String -> IO (ExitCode, String, String)
runExtract binPath outDir mDropDC = do
  removeDirIfExists outDir
  createDirectoryIfMissing True outDir
  baseEnv <- getEnvironment
  let scrubbed = filter ((/= "TIDEPOOL_TEST_DROP_DC") . fst) baseEnv
      env' = maybe scrubbed (\dc -> ("TIDEPOOL_TEST_DROP_DC", dc) : scrubbed) mDropDC
      args = [workDir ++ "/D1Fixture.hs", "--target", "target", "--output-dir", outDir]
      cp = (proc binPath args) { env = Just env' }
  readCreateProcessWithExitCode cp ""

checks :: IO [Check]
checks = do
  binPath <- resolveExtractBin
  createDirectoryIfMissing True workDir
  writeFile (workDir ++ "/D1Fixture.hs") fixtureSrc

  -- Leg 1 (anti-vacuity control): knob UNSET — the SAME module extracts
  -- successfully, proving the drop (not something else) makes the difference.
  -- Whole-module @--target target@ mode writes @<targetName>.cbor@, i.e.
  -- @target.cbor@ (only the session/turn lanes use the fixed "result" base —
  -- see 'Main.processSessionFile'/'Main.runTurnMode').
  let outUnset = workDir ++ "/out-unset"
  (unsetCode, unsetOut, unsetErr) <- runExtract binPath outUnset Nothing
  unsetResultExists <- doesFileExist (outUnset ++ "/target.cbor")

  -- Leg 2: knob SET to this fixture's only recordDC-supplied constructor —
  -- extraction must FAIL with CHECK A's message, no target.cbor written.
  let outSet = workDir ++ "/out-set"
  (setCode, setOut, setErr) <- runExtract binPath outSet (Just dropTarget)
  setResultExists <- doesFileExist (outSet ++ "/target.cbor")

  pure
    [ check ("D1 mutation: knob UNSET extracts successfully (anti-vacuity control) — exit="
              ++ show unsetCode ++ " stderr=" ++ oneLine unsetErr)
        (unsetCode == ExitSuccess && unsetResultExists)
    , check ("D1 mutation: knob SET (" ++ dropTarget ++ ") fails extraction — exit=" ++ show setCode)
        (setCode /= ExitSuccess)
    , check "D1 mutation: knob SET writes no target.cbor"
        (not setResultExists)
    , check ("D1 mutation: knob SET fails with CHECK A's message — stdout+stderr: "
              ++ oneLine (setOut ++ setErr))
        ("D1 CHECK A" `isInfixOf` (setOut ++ setErr))
    ]
  where
    oneLine = unwords . words
