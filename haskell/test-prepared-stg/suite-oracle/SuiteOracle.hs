{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TemplateHaskell #-}

-- | Native GHC oracle for the Suite.hs prepared corpus. Built and driven only
-- by @scripts/prepared-corpus-oracle.sh@, with the prepared pipeline's
-- optimisation flags.
--
-- @suite-oracle domain@ prints one JSON line per manifest expectation key:
-- its scope (@source_top@ or @compiler_introduced@) and, for source tops, its
-- class (@value@, @not_closed@, @unrepresentable@).
--
-- @suite-oracle eval OCCURRENCE@ evaluates one closed source top and prints
-- its expectation. Exit status 3 means an unrepresentable leaf (for example a
-- NaN) and 4 means a bottom below weak head normal form, which a non-forcing
-- observation cannot match. The driver enforces nontermination with an OS
-- timeout, because a non-allocating loop never yields to an in-process one.
module Main (main) where

import Control.Exception
import Data.List (foldl')
import System.Environment (getArgs)
import System.Exit
import System.IO

import qualified Suite
import SuiteOracleRender
import SuiteOracleTH (oracleTable)

table :: [(String, OracleEntry)]
table = $(oracleTable)

main :: IO ()
main = getArgs >>= \case
  ["domain"] -> mapM_ (putStrLn . domainLine) table
  ["eval", occurrence] -> case lookup occurrence table of
    Just (SourceValue whnf rendered) -> evalOne whnf rendered
    _ -> refuse 2 (occurrence ++ " is not a closed source value top")
  _ -> refuse 2 "usage: suite-oracle domain | suite-oracle eval OCCURRENCE"

domainLine :: (String, OracleEntry) -> String
domainLine (occurrence, entry) =
  "{\"occurrence\":" ++ jsonString occurrence ++ "," ++ case entry of
    CompilerIntroduced -> "\"scope\":\"compiler_introduced\"}"
    SourceNotClosed reason -> source "not_closed" (Just reason)
    SourceUnrepresentable reason -> source "unrepresentable" (Just reason)
    SourceValue _ _ -> source "value" Nothing
  where
    source class_ reason =
      "\"scope\":\"source_top\",\"class\":" ++ jsonString class_
        ++ maybe "" (\r -> ",\"reason\":" ++ jsonString r) reason
        ++ "}"

-- | A failure while reaching weak head normal form is the whole top's
-- failure, matching the engine's reusable runtime failure. A failure after
-- that lives inside the value; a non-forcing observation cannot reproduce it,
-- so the top is refused rather than given an error oracle.
evalOne :: IO () -> String -> IO ()
evalOne whnf rendered = do
  top <- try whnf
  case top of
    Left exception
      | Just (_ :: SomeAsyncException) <- fromException exception -> throwIO exception
      | Just NonTermination <- fromException exception ->
          putStrLn (errorExpectation "blackhole")
      | otherwise -> putStrLn (errorExpectation "raised_exception")
    Right () -> do
      deep <- try (evaluate (forceString rendered))
      case deep of
        Right json -> putStrLn json
        Left exception
          | Just (_ :: SomeAsyncException) <- fromException exception -> throwIO exception
          | Just (Unrepresentable reason) <- fromException exception -> refuse 3 reason
          | otherwise ->
              refuse 4 ("bottom below weak head normal form: " ++ displayException exception)

forceString :: String -> String
forceString s = foldl' (\() c -> c `seq` ()) () s `seq` s

refuse :: Int -> String -> IO a
refuse code reason = hPutStrLn stderr reason >> exitWith (ExitFailure code)
