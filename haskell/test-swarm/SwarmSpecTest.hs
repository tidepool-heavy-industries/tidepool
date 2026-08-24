-- | Runner for 'SwarmSpec''s property families. A plain @exitcode-stdio-1.0@
-- test-suite (the @varid-mechanism-test@/@extract-fidelity-test@ precedent),
-- not hspec/tasty — QuickCheck alone is already a new dependency for this
-- package, and reusing this codebase's existing "assert and exit non-zero"
-- idiom keeps it to one.
module Main (main) where

import System.Exit (exitFailure, exitSuccess)
import Test.QuickCheck

import SwarmSpec (properties)

main :: IO ()
main = do
  results <- mapM (uncurry run) properties
  if and results then exitSuccess else exitFailure
  where
    run name prop = do
      putStrLn ("--- " <> name <> " ---")
      res <- quickCheckWithResult stdArgs {maxSuccess = 200} prop
      pure (isSuccess res)
