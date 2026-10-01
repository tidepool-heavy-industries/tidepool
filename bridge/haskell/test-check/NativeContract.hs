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
import System.Environment (getArgs)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import qualified Tidepool.Check as Check
import Tidepool.Effects.Core (RecipeCheck (..))

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
    , "-ibridge/haskell/lib", "-ibridge/haskell/actors"
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

main :: IO ()
main = do
  [support, scratch] <- getArgs
  require "true assertion completes" (run (Check.assertThat "true" True) == ())
  throws "false assertion throws" (evaluate (run (Check.assertThat "false" False)))
  throws "discarded false blocks continuation"
    (evaluate (run (Check.assertThat "discarded" False >> pure (42 :: Int))))
  let observe :: Eff '[State Int] Bool
      observe = modify @Int (+ 1) >> ((>= 3) <$> get @Int)
      (_, observations) = run (runState (0 :: Int) (Check.assertEventually "eventual" observe))
  require "eventual success observes exactly three times" (observations == 3)
  let finalObservation :: Eff '[State Int] Bool
      finalObservation = do
        modify @Int (+ 1)
        count <- get @Int
        if count > 120 then error "observation limit exceeded" else pure (count == 120)
      (_, bounded) = run (runState (0 :: Int) (Check.assertEventually "last" finalObservation))
  require "120th observation may succeed" (bounded == 120)
  falseObservations <- newIORef (0 :: Int)
  throws "120 false observations fail" $
    runM $ Check.assertEventually "bounded" $
      sendM (modifyIORef' falseObservations (+ 1) >> pure False)
  require "failed await performs exactly 120 observations" . (== 120) =<< readIORef falseObservations
  successes <- newIORef []
  cellNumber <- newIORef (0 :: Int)
  let executeSource source = do
        number <- readIORef cellNumber
        modifyIORef' cellNumber (+ 1)
        outcome <- runCell support (scratch ++ "/cell-" ++ show number) source
        -- Native GHC cannot execute the turn-status JSON intrinsic. Stop at
        -- this effect boundary after running the exact generated actor cell.
        throwIO outcome
      execute = host successes executeSource
      checked label source = execute $ do
        actor <- Check.root
        Check.assertCell actor label source
      awaited label source = execute $ do
        actor <- Check.root
        Check.awaitCell actor label source
      cellResult label expected action = do
        result <- try @CellOutcome action
        require label (case result of Left outcome -> outcome == expected; Right _ -> False)
  cellResult "pure assertion executes in actor scope" CellAccepted (checked "scoped true" "scopedValue == 42")
  cellResult "case-expression assertion preserves layout" CellAccepted
    (checked "case predicate" "case Just scopedValue of\n  Just value -> value == 42\n  Nothing -> False")
  cellResult "false actor cell fails" CellFailed (checked "false cell" "scopedValue == 0")
  cellResult "non-Bool actor cell is rejected" CellRejected (checked "wrong type" "scopedValue")
  cellResult "typed await executes in actor scope" CellAccepted (awaited "typed await" "pure (scopedValue == 42)")
  cellResult "do-action await preserves layout" CellAccepted
    (awaited "do predicate" "do\n  let value = scopedValue\n  pure (value == 42)")
  cellResult "non-Bool action is rejected" CellRejected (awaited "wrong action type" "pure scopedValue")
  require "cell boundary never counts success before accepting turn status" . null =<< readIORef successes
  failedCounts <- newIORef []
  throws "native turn decoder rejects before continuation" $
    host failedCounts (const (pure "{\"status\":\"failed\",\"items\":[{\"output\":\"True\"}]}")) $ do
      actor <- Check.root
      Check.assertCell actor "failed turn" "True"
      pure (42 :: Int)
  require "native decoder rejection records zero successes" . null =<< readIORef failedCounts
  putStrLn "executed: 17 native helper contract checks"
  putStrLn "not executed: successful host counting requires the prepared-runtime JSON intrinsic"
