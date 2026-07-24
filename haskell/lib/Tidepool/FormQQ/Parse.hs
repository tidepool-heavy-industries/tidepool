-- | Pure line parser for the @[form|...|]@ DSL. No Template Haskell, no
-- 'Tidepool.Ui' dependency — this is unit-testable directly (see
-- @haskell\/test-formqq\/FormQQParserTest.hs@), independent of the
-- splice-time codegen in "Tidepool.FormQQ".
--
-- == Grammar (one widget per line)
--
-- @
-- choice \<prompt\>: \<key\> \<key\> ...   -- each key doubles as its own label
-- text \<prompt\>
-- multiline \<prompt\>
-- \<anything else\>                      -- prose, verbatim
-- @
--
-- A blank (all-whitespace) line parses to @Right Nothing@ (skipped, not a
-- widget). Leading whitespace before a @choice@\/@text@\/@multiline@ keyword
-- is tolerated (so the DSL can be written indented to match surrounding
-- source); a plain prose line is passed through byte-for-byte, indentation
-- included.
module Tidepool.FormQQ.Parse
  ( ParsedLine (..)
  , parseFormLine
  ) where

import Prelude
import Data.Char (isSpace)
import Data.List (stripPrefix)

-- | One parsed DSL line, pre-codegen.
data ParsedLine
  = PChoice String [String]  -- ^ prompt, keys (each key used as both key and label)
  | PText String             -- ^ prompt (single-line text input)
  | PMultiline String        -- ^ prompt (multiline text input)
  | PProse String             -- ^ verbatim prose line
  deriving (Eq, Show)

-- | Parse one DSL line. @Right Nothing@ = blank line (skipped). @Left msg@ =
-- malformed line; @msg@ carries no line number — the caller (the quoter's
-- per-line loop in "Tidepool.FormQQ") attaches that.
parseFormLine :: String -> Either String (Maybe ParsedLine)
parseFormLine raw
  | all isSpace raw = Right Nothing
  | Just rest <- stripPrefix "choice " stripped    = Just <$> parseChoice rest
  | Just rest <- stripPrefix "text " stripped       = Just <$> parseText rest
  | Just rest <- stripPrefix "multiline " stripped  = Just <$> parseMultiline rest
  | otherwise                                       = Right (Just (PProse raw))
  where
    stripped = dropWhile isSpace raw

parseChoice :: String -> Either String ParsedLine
parseChoice rest = case break (== ':') rest of
  (_, []) -> Left "choice widget requires ': key key ...' after the prompt"
  (promptRaw, _ : afterColon)
    | null promptT -> Left "choice prompt is empty"
    | null keys    -> Left "choice widget needs at least one key after ':'"
    | otherwise    -> Right (PChoice promptT keys)
    where
      promptT = trim promptRaw
      keys    = words afterColon

parseText :: String -> Either String ParsedLine
parseText rest
  | null promptT = Left "text widget needs a prompt"
  | otherwise    = Right (PText promptT)
  where promptT = trim rest

parseMultiline :: String -> Either String ParsedLine
parseMultiline rest
  | null promptT = Left "multiline widget needs a prompt"
  | otherwise    = Right (PMultiline promptT)
  where promptT = trim rest

trim :: String -> String
trim = f . f where f = reverse . dropWhile isSpace
