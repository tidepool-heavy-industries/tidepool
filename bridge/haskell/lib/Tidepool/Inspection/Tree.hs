{-# LANGUAGE OverloadedStrings #-}

-- | A lazy rendering and its unconsumed suffix. Traversal never asks for the
-- length of a container or evaluates a field beyond the current allowance.
module Tidepool.Inspection.Tree
  ( DisplayTree (..)
  , renderTree
  , treeParts
  , literalText
  , precedenceParens
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
  | Group [DisplayTree]
  | LineBreak
  | LegacyLeaf (Int -> (Text, Bool))

-- | Quote text using Haskell string-literal escapes.
literalText :: Text -> DisplayTree
literalText = StringLeaf . show . T.unpack

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
      Group children ->
        let layout = if flatLength (maxLineWidth + 1) children <= maxLineWidth
              then flatten children
              else children
        in go left pieces unavailable (layout ++ rest)
      LineBreak -> go (left - 1) ("\n" : pieces) unavailable rest
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

maxLineWidth :: Int
maxLineWidth = 80

flatLength :: Int -> [DisplayTree] -> Int
flatLength limit = go 0
  where
    go count _ | count >= limit = count
    go count [] = count
    go count (tree : rest) = case tree of
      TextLeaf value -> go (count + min (limit - count) (T.length value)) rest
      StringLeaf value -> go (count + length (take (limit - count) value)) rest
      Concat children -> go count (children ++ rest)
      Group children -> go count (children ++ rest)
      LineBreak -> go (count + 1) rest
      LegacyLeaf _ -> limit

flatten :: [DisplayTree] -> [DisplayTree]
flatten = concatMap $ \tree -> case tree of
  LineBreak -> [TextLeaf " "]
  Group children -> flatten children
  Concat children -> [Concat (flatten children)]
  other -> [other]

-- | GHC's 'showParen' for a constructor application rendered at a
-- 'showsPrec' precedence: parenthesized only above application precedence.
precedenceParens :: Int -> DisplayTree -> DisplayTree
precedenceParens precedence tree
  | precedence > 10 = Concat [TextLeaf "(", tree, TextLeaf ")"]
  | otherwise = tree

treeParts :: Text -> Text -> [DisplayTree] -> DisplayTree
treeParts opening closing children = Group
  [ TextLeaf opening
  , Concat (separate children)
  , TextLeaf closing
  ]
  where
    separate [] = []
    separate (value : rest) = value : following rest
    following [] = []
    following (value : rest) = TextLeaf "," : LineBreak : value : following rest
