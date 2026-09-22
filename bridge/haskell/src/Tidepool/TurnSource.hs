-- | Source assembly for the fused classify-and-compile turn request.
--
-- This remains in the compiler worker because binder names come from GHC's
-- classification result and must be inserted before the same resident worker
-- compiles the selected wrapper. Moving it to the launcher would require a
-- second worker round trip. All template authorship and selection policy stay
-- on the Rust side; this module only performs literal placement.
module Tidepool.TurnSource
  ( extractModuleName
  , spliceTemplate
  ) where

import Data.Char (isAlphaNum, isSpace)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix)
import Data.Maybe (listToMaybe)

-- | Substitute the raw turn, statement-position turn, and binder list without
-- rescanning inserted text.
spliceTemplate :: String -> String -> String -> String
spliceTemplate template turnText binders = go template
  where
    go source
      | "{{TURN_STMT}}" `isPrefixOf` source = placeTurnStmt turnText ++ go (drop 13 source)
      | "{{TURN}}" `isPrefixOf` source = turnText ++ go (drop 8 source)
      | "{{BINDERS}}" `isPrefixOf` source = binders ++ go (drop 11 source)
    go (char : rest) = char : go rest
    go [] = []

-- | Place a turn in a @do@ block. A layout-sensitive @let@ statement gets
-- explicit braces; every result ends with a newline.
placeTurnStmt :: String -> String
placeTurnStmt turnText = case letRest of
  Just rest | not ("{" `isPrefixOf` dropWhile isSpace rest) ->
    "let {" ++ rest ++ trailingNewline rest ++ " }\n"
  _ -> turnText ++ trailingNewline turnText
  where
    trimmed = dropWhile isSpace turnText
    letRest = case stripPrefix "let" trimmed of
      Just rest@(char : _) | isSpace char -> Just rest
      _ -> Nothing
    trailingNewline text = if "\n" `isSuffixOf` text then "" else "\n"

-- | Read the module name from a conventional module header.
extractModuleName :: String -> Maybe String
extractModuleName source = listToMaybe
  [ name
  | line <- lines source
  , Just rest <- [stripPrefix "module " (dropWhile (== ' ') line)]
  , let name = takeWhile (\char -> isAlphaNum char || char == '.' || char == '_')
          (dropWhile (== ' ') rest)
  , not (null name)
  ]
