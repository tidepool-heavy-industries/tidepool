-- | Unit tests for the pure @[form|...|]@ DSL line parser
-- ('Tidepool.FormQQ.Parse.parseFormLine'). No TH\/splice evaluation involved
-- here — this exercises the parser directly. The compile-time-fail path (a
-- malformed line becoming a GHC error naming the line number) is exercised
-- live through the JIT eval pipeline by @works_form_qq@ in
-- @tidepool-runtime\/tests\/jit_surface.rs@.
--
-- Run: @cabal run formqq-parser-test@ (needs the nix with-packages GHC on
-- PATH; same toolchain as @session-c-test@\/@varid-mechanism-test@).
module Main (main) where

import Tidepool.FormQQ.Parse (ParsedLine (..), parseFormLine)

import Control.Monad (forM_, unless)
import System.Exit (exitFailure, exitSuccess)

main :: IO ()
main = do
  let checks :: [(String, Bool)]
      checks =
        [ ("blank line -> skipped",
            parseFormLine "" == Right Nothing)
        , ("whitespace-only line -> skipped",
            parseFormLine "   \t  " == Right Nothing)
        , ("choice widget",
            parseFormLine "choice pick one: a b c"
              == Right (Just (PChoice "pick one" ["a", "b", "c"])))
        , ("choice trims prompt and tolerates extra spacing",
            parseFormLine "choice  spaced  : x"
              == Right (Just (PChoice "spaced" ["x"])))
        , ("choice tolerates leading indentation",
            parseFormLine "   choice env: dev prod"
              == Right (Just (PChoice "env" ["dev", "prod"])))
        , ("choice missing colon is an error",
            parseFormLine "choice pick one"
              == Left "choice widget requires ': key key ...' after the prompt")
        , ("choice empty prompt is an error",
            parseFormLine "choice : a b"
              == Left "choice prompt is empty")
        , ("choice no keys after colon is an error",
            parseFormLine "choice pick one:   "
              == Left "choice widget needs at least one key after ':'")
        , ("text widget",
            parseFormLine "text your name"
              == Right (Just (PText "your name")))
        , ("text empty prompt is an error",
            parseFormLine "text   "
              == Left "text widget needs a prompt")
        , ("multiline widget",
            parseFormLine "multiline describe the bug"
              == Right (Just (PMultiline "describe the bug")))
        , ("multiline empty prompt is an error",
            parseFormLine "multiline   "
              == Left "multiline widget needs a prompt")
        , ("prose passthrough, verbatim",
            parseFormLine "Just some notes."
              == Right (Just (PProse "Just some notes.")))
        , ("word starting with 'choice' but no following space is prose",
            parseFormLine "choices are great"
              == Right (Just (PProse "choices are great")))
        ]

  forM_ checks $ \(label, ok) ->
    putStrLn ((if ok then "ok   - " else "FAIL - ") ++ label)

  unless (all snd checks) exitFailure
  putStrLn "all form-qq parser checks passed"
  exitSuccess
