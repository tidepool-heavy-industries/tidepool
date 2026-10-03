module QuasiQuoteOccurrencesBenchmark (quasiQuoteOccurrenceBenchmark) where

import Control.Exception (evaluate)
import Control.Monad (forM_, unless, when)
import Control.Monad.IO.Class (liftIO)
import Data.Generics (everything, mkQ)
import Data.IORef (newIORef, readIORef)
import GHC (GhcPs, HsUntypedSplice(..), ParsedSource, getSessionDynFlags, runGhc, unLoc)
import GHC.Clock (getMonotonicTimeNSec)
import GHC.Data.FastString (mkFastString)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Driver.Session (parseDynamicFilePragma, xopt_set)
import GHC.LanguageExtensions (Extension(..))
import GHC.Parser qualified as Parser
import GHC.Parser.Header (getOptions)
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP)
import GHC.Stats (RTSStats(..), getRTSStats, getRTSStatsEnabled)
import GHC.Types.Name.Reader (RdrName)
import GHC.Types.SrcLoc (mkRealSrcLoc)
import System.CPUTime (getCPUTime)
import System.Mem (performGC)
import Text.Read (readMaybe)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences)

-- These boundaries plus the IORef read in each iteration prevent a benchmark
-- loop from sharing one pure traversal result across all its iterations.
{-# NOINLINE oldCollector #-}
oldCollector :: ParsedSource -> [RdrName]
oldCollector = everything (++) (mkQ [] selected) . unLoc
  where
    selected :: HsUntypedSplice GhcPs -> [RdrName]
    selected (HsQuasiQuote _ name _) = [name]
    selected _ = []

{-# NOINLINE newCollector #-}
newCollector :: ParsedSource -> [RdrName]
newCollector = quasiQuoteOccurrences

-- Parsing, source IO, AST warming and exact inventory comparison are excluded
-- from measured intervals. Allocation means cumulative RTS allocation, not
-- retained heap; ordinary collections during the timed loop remain included.
quasiQuoteOccurrenceBenchmark :: String -> [FilePath] -> IO ()
quasiQuoteOccurrenceBenchmark size files = do
  iterations <- case readMaybe size :: Maybe Int of
    Just value | value > 0 -> pure value
    _ -> fail "quasiquote benchmark requires a positive iteration count"
  enabled <- getRTSStatsEnabled
  unless enabled (fail "quasiquote benchmark requires +RTS -T")
  real <- mapM (\path -> (path,) <$> readFile path) selectedFiles
  let inputs =
        [ ("small-no-quotes", "module Small where\nvalue = 1\n")
        , ("large-literal", "module Large where\nvalue = \"" ++ replicate 100000 'x' ++ "\"\n")
        ] ++ real
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    initial <- getSessionDynFlags
    let syntax = xopt_set (xopt_set initial QuasiQuotes) TemplateHaskell
    forM_ inputs $ \(label, source) -> do
      let (_, options) = getOptions (initParserOpts syntax)
            (stringToStringBuffer source) label
      (flags, _, _) <- parseDynamicFilePragma syntax options
      let state = initParserState (initParserOpts flags)
            (stringToStringBuffer source) (mkRealSrcLoc (mkFastString label) 1 1)
      parsed <- case unP Parser.parseModule state of
        PFailed _ -> fail (label ++ ": parser rejected benchmark input")
        POk _ ast -> pure ast
      liftIO $ do
        let reference = oldCollector parsed
            candidate = newCollector parsed
        unless (reference == candidate) (fail (label ++ ": occurrence inventories differ"))
        count <- evaluate (length reference)
        inputChars <- evaluate (length source)
        ast <- newIORef parsed
        -- Alternate order to expose run-order sensitivity rather than quietly
        -- giving one collector every first measurement.
        forM_ [1 :: Int .. 4] $ \repetition -> do
          let modes = [("old", oldCollector), ("pruned", newCollector)]
          forM_ (if odd repetition then modes else reverse modes) $ \(mode, collect) -> do
            performGC
            before <- getRTSStats
            startCpu <- getCPUTime
            startWall <- getMonotonicTimeNSec
            forM_ [1 .. iterations] $ \_ -> do
              current <- readIORef ast
              actual <- evaluate (length (collect current))
              when (actual /= count) (fail "quasiquote benchmark inventory count changed")
            stopWall <- getMonotonicTimeNSec
            stopCpu <- getCPUTime
            performGC
            after <- getRTSStats
            putStrLn ("{\"input\":" ++ show label
              ++ ",\"input_chars\":" ++ show inputChars
              ++ ",\"quotes\":" ++ show count
              ++ ",\"mode\":" ++ show mode
              ++ ",\"repetition\":" ++ show repetition
              ++ ",\"iterations\":" ++ show iterations
              ++ ",\"wall_ns\":" ++ show (stopWall - startWall)
              ++ ",\"cpu_ps\":" ++ show (stopCpu - startCpu)
              ++ ",\"allocated_bytes\":" ++ show (allocated_bytes after - allocated_bytes before)
              ++ "}")
  where
    selectedFiles = if null files
      then [ "test-cell-splitter/fixtures/quasiquote-occurrences/contexts.hs"
           , "lib/Tidepool/QQ/Fmt.hs"
           , "lib/Tidepool/Command.hs" ]
      else files
