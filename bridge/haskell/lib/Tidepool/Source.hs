{-# LANGUAGE OverloadedStrings #-}
-- | Pure inspection of captured source. No operation here reloads source.
module Tidepool.Source (SourceWindow(..), sourceLines) where
import Prelude
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Inspection.Display
import Tidepool.Inspection.Tree

-- | Line numbers address the source returned with this exact revision identity.
data SourceWindow = SourceWindow
  { sourceRevision :: Text
  , sourceWindowLines :: [(Int, Text)]
  } deriving (Eq, Show)

-- | Select a 1-based window from a retained 'readSourceModule' result.
-- Starts below one begin at one; nonpositive counts select no lines.
-- A terminal newline terminates its preceding line, without adding a line.
sourceLines :: Int -> Int -> (Text, Text) -> SourceWindow
sourceLines first count (revision, source) =
  let start = max 1 first
  in SourceWindow revision (take (max 0 count) (drop (start - 1) (zip [1..] (T.lines source))))

instance Display SourceWindow where
  displayTree (SourceWindow revision rows) = Constructor "SourceWindow"
    [("revision", displayTree revision), ("lines", Sequence "[" "]" (map line rows))]
    where line (number, body) = Concat [TextLeaf (T.pack (show number) <> ": "), TextLeaf body]
