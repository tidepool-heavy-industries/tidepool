{-# LANGUAGE DataKinds, GADTs, OverloadedStrings, TypeApplications #-}
module Main where

import Control.Exception (ErrorCall (..), Exception, evaluate, throwIO, try)
import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, run, runM, sendM)
import Control.Monad.Freer.State (State, get, modify, runState)
import Data.IORef (IORef, modifyIORef', newIORef, readIORef)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Data.Text.IO as Text
import System.Directory (createDirectoryIfMissing)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import qualified Tidepool.Check as Check
import Tidepool.Effects.Core (RecipeCheck (..))
import Tidepool.Test.Runner

require :: String -> Bool -> IO ()
require label observed = do
  unless observed (error label)
  putStrLn ("passed: " ++ label)

throws :: String -> IO a -> IO ()
throws label action = do
  result <- try @ErrorCall action
  require label (case result of Left _ -> True; Right _ -> False)

-- Native GHC compiles and executes the exact cell emitted by the exported
-- helper. The actor fixture's scoped binding deliberately has a typed value.
data CellOutcome = CellAccepted | CellRejected | CellFailed deriving (Eq, Show)
instance Exception CellOutcome

runCell :: FilePath -> FilePath -> Text -> IO CellOutcome
runCell support scratch source = do
  createDirectoryIfMissing True scratch
  let (imports, expression) = span (Text.isPrefixOf "import ") (Text.lines source)
      moduleSource = Text.unlines
        ([ "{-# LANGUAGE DataKinds, OverloadedStrings #-}"
         , "module Main where"
         , "import Control.Exception (evaluate)"
         , "import Control.Monad.Freer (Eff, run)"
         ] ++ imports ++
         [ "scopedValue :: Int"
         , "scopedValue = 42"
         , "main :: IO ()"
         , "main = evaluate (run (cell :: Eff '[] ()))"
         , "cell ="
         ] ++ map ("  " <>) expression)
      path = scratch ++ "/Cell.hs"
      binary = scratch ++ "/cell"
  Text.writeFile path moduleSource
  (compiled, buildOut, buildErr) <- readProcessWithExitCode "ghc"
    [ "-O0", "-fforce-recomp", "-i" ++ support
    , "-ilib", "-iactors"
    , "-outputdir", scratch, "-o", binary, path
    ] ""
  writeFile (scratch ++ "/build.log") (buildOut ++ buildErr)
  case compiled of
    ExitFailure _ -> pure CellRejected
    ExitSuccess -> do
      (completed, out, err) <- readProcessWithExitCode binary [] ""
      writeFile (scratch ++ "/execution.log") (out ++ err)
      pure $ case completed of
        ExitSuccess -> CellAccepted
        ExitFailure _ -> CellFailed

host :: IORef [Text] -> (Text -> IO Text) -> Eff '[RecipeCheck, IO] a -> IO a
host successes execute = runM . interpret handler
  where
    handler :: RecipeCheck a -> Eff '[IO] a
    handler request = case request of
      RecipeRoot -> pure ("fixture", 1, 1)
      RecipeTurn actor source -> do
        unless (actor == ("fixture", 1, 1)) (error "actor identity changed")
        sendM (execute source)
      RecipeAssert name observed -> sendM $
        if observed then modifyIORef' successes (++ [name])
        else throwIO (ErrorCall (Text.unpack name))
      _ -> error "unexpected recipe effect"

boundedObservations :: IO (Either ErrorCall (), Int)
boundedObservations = do
  observations <- newIORef (0 :: Int)
  result <- try @ErrorCall $ runM $ Check.assertEventually "bounded" $
    sendM (modifyIORef' observations (+ 1) >> pure False)
  count <- readIORef observations
  pure (result, count)

nativeDecoder :: IO (Either ErrorCall Int, [Text])
nativeDecoder = do
  successes <- newIORef []
  result <- try @ErrorCall $
    host successes (const (pure "{\"status\":\"failed\",\"items\":[{\"output\":\"True\"}]}")) $ do
      actor <- Check.root
      Check.assertCell actor "failed turn" "True"
      pure (42 :: Int)
  counted <- readIORef successes
  pure (result, counted)

cellCheck :: String -> Bool -> Text -> CellOutcome -> IO [Text]
cellCheck label awaited source expected = do
  support <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  successes <- newIORef []
  let executeSource emitted = runCell support ("native-contract-cells/" ++ label) emitted >>= throwIO
      action = host successes executeSource $ do
        actor <- Check.root
        if awaited then Check.awaitCell actor (Text.pack label) source else Check.assertCell actor (Text.pack label) source
  result <- try @CellOutcome action
  require label (case result of Left outcome -> outcome == expected; Right _ -> False)
  readIORef successes

tests :: TestTree
tests = testGroup "native helper contract"
  [ testCase "true assertion completes" $
      require "true" (run (Check.assertThat "true" True) == ())
  , testCase "false assertion throws" $
      throws "false" (evaluate (run (Check.assertThat "false" False)))
  , testCase "discarded false blocks continuation" $
      throws "discarded" (evaluate (run (Check.assertThat "discarded" False >> pure (42 :: Int))))
  , testCase "eventual success observes exactly three times" $ do
      let observe :: Eff '[State Int] Bool
          observe = modify @Int (+ 1) >> ((>= 3) <$> get @Int)
          (_, observations) = run (runState (0 :: Int) (Check.assertEventually "eventual" observe))
      require "three observations" (observations == 3)
  , testCase "120th observation may succeed" $ do
      let observe :: Eff '[State Int] Bool
          observe = do
            modify @Int (+ 1)
            count <- get @Int
            if count > 120 then error "observation limit exceeded" else pure (count == 120)
          (_, observations) = run (runState (0 :: Int) (Check.assertEventually "last" observe))
      require "last observation" (observations == 120)
  , testCase "120 false observations fail" $ do
      (result, _) <- boundedObservations
      require "bounded failure" (case result of Left _ -> True; Right _ -> False)
  , testCase "failed await performs exactly 120 observations" $ do
      (_, count) <- boundedObservations
      require "bounded count" (count == 120)
  , testCase "pure assertion executes in actor scope" $
      cellCheck "scoped-true" False "scopedValue == 42" CellAccepted >> pure ()
  , testCase "case-expression assertion preserves layout" $
      cellCheck "case-predicate" False "case Just scopedValue of\n  Just value -> value == 42\n  Nothing -> False" CellAccepted >> pure ()
  , testCase "false actor cell fails" $
      cellCheck "false-cell" False "scopedValue == 0" CellFailed >> pure ()
  , testCase "non-Bool actor cell is rejected" $
      cellCheck "wrong-type" False "scopedValue" CellRejected >> pure ()
  , testCase "typed await executes in actor scope" $
      cellCheck "typed-await" True "pure (scopedValue == 42)" CellAccepted >> pure ()
  , testCase "do-action await preserves layout" $
      cellCheck "do-predicate" True "do\n  let value = scopedValue\n  pure (value == 42)" CellAccepted >> pure ()
  , testCase "non-Bool action is rejected" $
      cellCheck "wrong-action" True "pure scopedValue" CellRejected >> pure ()
  , testCase "cell boundary never counts success before accepting turn status" $
      cellCheck "uncounted-boundary" False "scopedValue == 42" CellAccepted >>= require "no premature success" . null
  , testCase "native turn decoder rejects before continuation" $ do
      (result, _) <- nativeDecoder
      require "decoder refused" (case result of Left _ -> True; Right _ -> False)
  , testCase "native decoder rejection records zero successes" $ do
      (_, successes) <- nativeDecoder
      require "no successes" (null successes)
  ]

main :: IO ()
main = runTests tests
