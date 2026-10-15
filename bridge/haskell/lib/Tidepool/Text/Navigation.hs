{-# LANGUAGE OverloadedStrings #-}
-- | Pure addresses into exact retained Text. Offsets count Unicode scalar
-- values, not UTF-8 bytes. These functions make no claim about upstream content.
module Tidepool.Text.Navigation
  ( TextSlice(..), LiteralMatches(..), TextSelectionIssue(..)
  , retainedLines, retainedWindow, literalSearch
  ) where
import Prelude
import Tidepool.Inspection.Display
import Tidepool.Inspection.Tree
import Data.Text (Text)
import qualified Data.Text as T

data TextSlice = TextSlice
  { sliceIdentity :: Text, sliceStart :: Int, sliceEnd :: Int, sliceText :: Text
  , retainedBefore :: Bool, retainedAfter :: Bool
  } deriving (Eq, Show)
data LiteralMatches = LiteralMatches
  { occurrenceCount :: Int, occurrenceLocations :: [(Int, Int)]
  , occurrenceContexts :: [TextSlice]
  } deriving (Eq, Show)
data TextSelectionIssue = EmptyNeedle | InvalidLineWindow | LineOutsideCapture deriving (Eq, Show)

-- | LF stays in its preceding line, including a preceding CR. A terminal LF
-- adds no extra line. Empty captured text has zero lines.
retainedLines :: Text -> [Text]
retainedLines text | T.null text = []
                   | otherwise = let (body, rest) = T.breakOn "\n" text
                                 in if T.null rest then [body] else (body <> "\n") : retainedLines (T.drop 1 rest)

-- | Strict 1-based inclusive line selection. Never clamps an invalid address.
retainedWindow :: Text -> Int -> Int -> Text -> Either TextSelectionIssue TextSlice
retainedWindow identity first last' text
  | first < 1 || last' < first = Left InvalidLineWindow
  | last' > length rows = Left LineOutsideCapture
  | otherwise = Right (TextSlice identity start end body (start > 0) (end < T.length text))
  where rows = retainedLines text
        start = sum (map T.length (take (first - 1) rows))
        body = T.concat (take (last' - first + 1) (drop (first - 1) rows))
        end = start + T.length body

-- | All overlapping literal matches, with stable 0-based half-open offsets.
-- Context radius is nonnegative characters on each side, independent of line
-- length. Empty needles are rejected rather than issuing unbounded locations.
literalSearch :: Text -> Int -> Text -> Text -> Either TextSelectionIssue LiteralMatches
literalSearch identity radius needle text
  | T.null needle = Left EmptyNeedle
  | otherwise = Right (LiteralMatches (length locations) locations (map context locations))
  where
    width = T.length needle
    size = T.length text
    padding = max 0 radius
    locations = search 0 text
    search offset remaining = let (before, suffix) = T.breakOn needle remaining
      in if T.null suffix then [] else let start = offset + T.length before
        in (start, start + width) : search (start + 1) (T.drop 1 suffix)
    context (start, end) =
      let left = max 0 (start - padding)
          right = end + min padding (size - end)
      in TextSlice identity left right (T.take (right - left) (T.drop left text)) (left > 0) (right < size)


instance Display TextSlice where
  displayTree slice = Constructor "TextSlice"
    [("identity", displayTree (sliceIdentity slice)), ("start", displayTree (sliceStart slice)),
     ("end", displayTree (sliceEnd slice)), ("retainedBefore", displayTree (retainedBefore slice)),
     ("retainedAfter", displayTree (retainedAfter slice)), ("text", TextLeaf (sliceText slice))]
instance Display LiteralMatches where
  displayTree matches = Constructor "LiteralMatches"
    [("count", displayTree (occurrenceCount matches)), ("locations", displayTree (occurrenceLocations matches)),
     ("contexts", displayTree (occurrenceContexts matches))]
instance Display TextSelectionIssue where displayTree = TextLeaf . T.pack . show
