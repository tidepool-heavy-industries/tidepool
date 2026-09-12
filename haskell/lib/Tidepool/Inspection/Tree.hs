{-# LANGUAGE OverloadedStrings #-}

-- | A lazy rendering and its unconsumed suffix. Traversal never asks for the
-- length of a container or evaluates a field beyond the current allowance.
module Tidepool.Inspection.Tree
  ( DisplayTree (..)
  , renderTree
  , treeParts
  ) where

import Data.Text (Text)
import qualified Data.Text as T
import Prelude

-- | Legacy renderers receive the remaining allowance. Their omitted detail
-- cannot be recovered without a new rendering contract; it is not a cursor.
data DisplayTree
  = TextLeaf Text
  | StringLeaf String
  | Concat [DisplayTree]
  | LegacyLeaf (Int -> (Text, Bool))

-- | Render at most the requested characters, retaining the actual remaining
-- tree. The last flag reports detail omitted by a legacy custom renderer.
renderTree :: Int -> DisplayTree -> (Text, Maybe DisplayTree, Bool)
renderTree allowance tree = go (max 0 allowance) [] False [tree]
  where
    go _ pieces unavailable [] = (T.concat (reverse pieces), Nothing, unavailable)
    go left pieces unavailable pending | left <= 0 =
      (T.concat (reverse pieces), Just (Concat pending), unavailable)
    go left pieces unavailable (node : rest) = case node of
      Concat children -> go left pieces unavailable (children ++ rest)
      TextLeaf value ->
        let (prefix, suffix) = T.splitAt left value
            remaining = if T.null suffix then rest else TextLeaf suffix : rest
        in go (left - T.length prefix) (prefix : pieces) unavailable remaining
      StringLeaf value ->
        let (prefix, suffix) = splitAt left value
            remaining = case suffix of [] -> rest; _ -> StringLeaf suffix : rest
        in go (left - length prefix) (T.pack prefix : pieces) unavailable remaining
      LegacyLeaf render ->
        let (value, omitted) = render left
            (prefix, excess) = T.splitAt left value
        in go (left - T.length prefix) (prefix : pieces)
             (unavailable || omitted || not (T.null excess)) rest

treeParts :: Text -> Text -> [DisplayTree] -> DisplayTree
treeParts opening closing children = Concat
  [ TextLeaf opening
  , Concat (separate children)
  , TextLeaf closing
  ]
  where
    separate [] = []
    separate (value : rest) = value : following rest
    following [] = []
    following (value : rest) = TextLeaf ",\n" : value : following rest
