{-# LANGUAGE MagicHash #-}

module Main where

import AwaitSettledDependencies qualified as Await
import Control.Exception (PatternMatchFail, displayException, evaluate, try)
import GHC.Internal.Control.Exception.Base (patError)
import ProjectWorkCandidate qualified as Project

main :: IO ()
main = do
  putStrLn ("project-work-candidate=" <> show Project.candidate)
  putStrLn ("await-settled-dependencies=" <> show Await.awaitSettledDependencies)
  failure <-
    try (evaluate (patError "Suite.hs:3|f"# :: Int)) ::
      IO (Either PatternMatchFail Int)
  case failure of
    Left exception ->
      putStrLn ("pattern-match-failure=" <> show (displayException exception))
    Right value ->
      error ("patError unexpectedly returned " <> show value)
