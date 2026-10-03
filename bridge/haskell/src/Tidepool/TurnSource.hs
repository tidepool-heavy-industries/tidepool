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
  , renderImportBinder
  , replaceTemplateMarker
  ) where

import GHC.Types.Name.Occurrence (isSymOcc, mkVarOcc)
import Data.Char (isAlphaNum, isSpace)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix)
import Data.Maybe (listToMaybe)
import qualified Data.Text as T

-- | Replace one protected marker before authored text is inserted. Duplicate
-- or missing markers cannot silently alter the selected compiler recipe.
replaceTemplateMarker :: String -> String -> String -> Either String String
replaceTemplateMarker marker replacement template =
  let (before,after) = T.breakOn (T.pack marker) (T.pack template)
      remaining = T.drop (length marker) after
  in if null marker || T.null after || T.pack marker `T.isInfixOf` remaining
    then Left "checked recipe marker is missing or duplicated"
    else Right (T.unpack before ++ replacement ++ T.unpack remaining)

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
    let body = explicitBraceLetBody rest
     in "let {" ++ body ++ trailingNewline body ++ " }\n"
  _ -> turnText ++ trailingNewline turnText
  where
    trimmed = dropWhile isSpace turnText
    letRest = case stripPrefix "let" trimmed of
      Just rest@(char : _) | isSpace char -> Just rest
      _ -> Nothing
    trailingNewline text = if "\n" `isSuffixOf` text then "" else "\n"

-- | Mirror Rust's @explicit_brace_let_body@: preserve the separators that
-- layout inserts between declarations when a statement-position @let@ is
-- placed inside explicit braces. A deeper line continues the preceding
-- declaration's expression.
explicitBraceLetBody :: String -> String
explicitBraceLetBody rest = firstLine ++ go following
  where
    (firstLine, following) = break (== '\n') rest
    firstColumn = 4 + length (takeWhile isSpace firstLine)

    go [] = []
    go ('\n' : remaining) =
      let (line, followingLines) = break (== '\n') remaining
          (indent, content) = span isSpace line
          placed = if not (null content) && length indent + 1 == firstColumn
            then indent ++ ";" ++ content
            else line
       in '\n' : placed ++ go followingLines
    go other = other

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

renderImportBinder :: String -> String
renderImportBinder name
  | isSymOcc (mkVarOcc name) = "(" ++ name ++ ")"
  | otherwise = name
