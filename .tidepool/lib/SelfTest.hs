{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Parse cargo test output: extract pass/fail/ignore counts from raw text blocks.
module SelfTest where

import Tidepool.Prelude

-- | Parse "test result: ok. N passed; M failed; K ignored; ..."
-- Returns (passed, failed, ignored). Returns (0,0,0) on parse failure.
parseTestResult :: Text -> (Int, Int, Int)
parseTestResult line =
  let ws = words (replace ";" " " (replace "." " " line))
  in  ( numBefore "passed" ws
      , numBefore "failed" ws
      , numBefore "ignored" ws
      )

-- | Find the number immediately before a label word.
numBefore :: Text -> [Text] -> Int
numBefore _     [] = 0
numBefore _     [_] = 0
numBefore label (w:x:rest)
  | x == label = fromMaybe 0 (parseIntM w)
  | otherwise  = numBefore label (x:rest)

-- | Parse multi-crate output blocks:
--   "=== crate-name ===\ntest result: ok. 5 passed; 0 failed; 1 ignored"
parseBlocks :: Text -> [(Text, Int, Int, Int)]
parseBlocks = go . lines
  where
    go [] = []
    go (l:rest)
      | isSep l =
          let name = strip (replace "===" "" l)
          in  case rest of
                (resultLine:rest') ->
                  let (p, f, i) = parseTestResult resultLine
                  in  (name, p, f, i) : go rest'
                [] -> [(name, 0, 0, 0)]
      | otherwise = go rest
    isSep t = isInfixOf "===" t
