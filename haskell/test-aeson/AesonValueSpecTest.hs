{-# LANGUAGE OverloadedStrings #-}

-- | Unit coverage for 'Tidepool.Aeson.Value.eitherDecodeValue''s hand-written
-- RFC 8259 parser — the body that actually runs on the prepared-STG route
-- (the Core route intercepts the same name and lowers it to the Rust
-- @serde_json@ primop instead; see that function's Haddock). A plain
-- @exitcode-stdio-1.0@ test-suite in the @constructor-arity-test@ /
-- @swarm-spec-test@ style: assert-and-exit-non-zero, no test framework.
module Main (main) where

import Data.IORef (newIORef, readIORef, modifyIORef')
import qualified Data.Map.Strict as Map
import Data.Text (Text)
import System.Exit (exitFailure, exitSuccess)

import Tidepool.Aeson.Value

main :: IO ()
main = do
  failures <- newIORef (0 :: Int)
  let record ok label
        | ok = putStrLn ("ok   - " ++ label)
        | otherwise = do
            putStrLn ("FAIL - " ++ label)
            modifyIORef' failures (+ 1)

      -- Successful decode, compared against an expected 'Value'.
      checkRight :: String -> Text -> Value -> IO ()
      checkRight label input expected = case eitherDecodeValue input of
        Right v | v == expected -> record True label
        Right v -> record False (label ++ ": got " ++ show v ++ ", expected " ++ show expected)
        Left err -> record False (label ++ ": decode failed: " ++ show err)

      -- Rejected input: any 'Left' counts as a pass.
      checkLeft :: String -> Text -> IO ()
      checkLeft label input = case eitherDecodeValue input of
        Left _ -> record True label
        Right v -> record False (label ++ ": expected a decode failure, got " ++ show v)

      -- Two inputs must decode to the SAME 'Value' (by 'Eq', which for a
      -- 'Number' compares 'Scientific' BY VALUE via 'toRational' — see
      -- "Tidepool.Aeson.Scientific"). Used for normal-form agreement (e.g.
      -- @1.50e2@ and @150@).
      checkSameAs :: String -> Text -> Text -> IO ()
      checkSameAs label lhs rhs = case (eitherDecodeValue lhs, eitherDecodeValue rhs) of
        (Right a, Right b) | a == b -> record True label
        (Right a, Right b) -> record False (label ++ ": " ++ show a ++ " /= " ++ show b)
        (l, r) -> record False (label ++ ": one side failed to decode: " ++ show (l, r))

      -- 'show' of the two sides must ALSO agree (Show re-normalizes via
      -- 'stripZeros' independent of the stored coefficient/exponent pair).
      checkSameShow :: String -> Text -> Text -> IO ()
      checkSameShow label lhs rhs = case (eitherDecodeValue lhs, eitherDecodeValue rhs) of
        (Right a, Right b) | show a == show b -> record True label
        (Right a, Right b) -> record False (label ++ ": show " ++ show a ++ " /= show " ++ show b)
        (l, r) -> record False (label ++ ": one side failed to decode: " ++ show (l, r))

  -- --- literals ---------------------------------------------------------
  checkRight "true" "true" (Bool True)
  checkRight "false" "false" (Bool False)
  checkRight "null" "null" Null
  checkRight "whitespace-padded literal" "  \t\n true \r\n " (Bool True)

  -- --- numbers ------------------------------------------------------------
  checkRight "zero" "0" (Number (scientific 0 0))
  checkRight "negative zero" "-0" (Number (scientific 0 0))
  checkRight "small int" "7" (Number (scientific 7 0))
  checkRight "negative int" "-42" (Number (scientific (-42) 0))
  checkRight "fraction" "1.5" (Number (scientific 15 (-1)))
  checkRight "exponent" "1e3" (Number (scientific 1 3))
  checkRight "fraction and exponent" "1.50e2" (Number (scientific 150 0))
  checkRight "negative exponent with sign" "-2.5E-3" (Number (scientific (-25) (-4)))
  checkRight
    "large integer beyond Int64"
    "265252859812191058636308480000000"
    (Number (scientific 265252859812191058636308480000000 0))
  checkRight
    "large negative integer beyond Int64"
    "-265252859812191058636308480000000"
    (Number (scientific (-265252859812191058636308480000000) 0))

  -- Scientific normal form: 'Eq' is by numeric value, 'Show' re-normalizes
  -- at render time, so any faithful (coefficient, exponent) pair the parser
  -- picks must agree with any other on both counts.
  checkSameAs "1.50e2 and 150 compare equal" "1.50e2" "150"
  checkSameShow "1.50e2 and 150 render identically" "1.50e2" "150"
  checkSameAs "1e2 and 100 compare equal" "1e2" "100"
  checkSameShow "1e2 and 100 render identically" "1e2" "100"
  checkRight "1e2 renders as 100" "1e2" (Number (scientific 100 0))
  case eitherDecodeValue "1e2" of
    Right (Number s) -> record (show s == "100") ("show 1e2 == \"100\": got " ++ show s)
    other -> record False ("1e2 did not decode to a Number: " ++ show other)
  case eitherDecodeValue "1.50e2" of
    Right (Number s) -> record (show s == "150") ("show 1.50e2 == \"150\": got " ++ show s)
    other -> record False ("1.50e2 did not decode to a Number: " ++ show other)

  -- --- strings and escapes -------------------------------------------------
  checkRight "plain string" "\"hello\"" (String "hello")
  checkRight "empty string" "\"\"" (String "")
  checkRight
    "all simple escapes"
    "\"\\\"\\\\\\/\\b\\f\\n\\r\\t\""
    (String "\"\\/\b\f\n\r\t")
  checkRight "unicode escape" "\"\\u00e9\"" (String "\233")
  checkRight
    "surrogate pair (musical G clef, U+1D11E)"
    "\"\\uD834\\uDD1E\""
    (String "\119070")
  checkLeft "lone high surrogate" "\"\\uD800\""
  checkLeft "lone low surrogate" "\"\\uDC00\""
  checkLeft "control character in string" "\"a\tb\""
  checkLeft "unterminated string" "\"abc"

  -- --- arrays and objects ---------------------------------------------------
  checkRight "empty array" "[]" (Array [])
  checkRight "empty object" "{}" (Object Map.empty)
  checkRight
    "nested object/array"
    "{\"a\":[1,2.5,\"x\"],\"b\":{\"c\":null,\"d\":[true,false]}}"
    (Object (Map.fromList
      [ ("a", Array [Number (scientific 1 0), Number (scientific 25 (-1)), String "x"])
      , ("b", Object (Map.fromList
          [ ("c", Null)
          , ("d", Array [Bool True, Bool False])
          ]))
      ]))
  checkRight
    "duplicate object keys: last one wins"
    "{\"a\":1,\"a\":2}"
    (Object (Map.fromList [("a", Number (scientific 2 0))]))

  -- --- rejections -----------------------------------------------------------
  checkLeft "leading zero" "01"
  checkLeft "leading plus" "+1"
  checkLeft "bare leading-dot fraction" ".5"
  checkLeft "bare trailing-dot fraction" "5."
  checkLeft "trailing comma in array" "[1,]"
  checkLeft "trailing comma in object" "{\"a\":1,}"
  checkLeft "trailing garbage after value" "1 2"
  checkLeft "trailing garbage after object" "{}garbage"
  checkLeft "empty input" ""
  checkLeft "only whitespace" "   "
  checkLeft "unquoted object key" "{a:1}"
  checkLeft "single quotes are not JSON" "'a'"
  checkLeft "NaN is not JSON" "NaN"

  n <- readIORef failures
  if n == 0
    then putStrLn "all checks passed" >> exitSuccess
    else putStrLn (show n ++ " check(s) failed") >> exitFailure
