{-# LANGUAGE OverloadedStrings #-}

-- | A lazy rendering and its unconsumed suffix. Traversal never asks for the
-- length of a container or evaluates a field beyond the current allowance.
module Tidepool.Inspection.Tree
  ( DisplayTree (..)
  , renderTree
  , DisplayState
  , ExpansionKey
  , expansionKeyNumber
  , expansionKeyFromNumber
  , displayStateText
  , displayStateKeys
  , displayStateUnavailable
  , newDisplayState
  , expandDisplayState
  , treeParts
  , literalText
  , rawText
  , precedenceParens
  ) where

import Data.Text (Text)
import qualified Data.Text as T
import Numeric (showHex)
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
  | Constructor Text [(Text, DisplayTree)]
  | Sequence Text Text [DisplayTree]

-- | Quote text with minimal escaping: only quotes, backslashes, and control
-- characters are escaped. Printable non-ASCII (e.g. \955) passes through
-- unescaped, unlike GHC's 'show', which numerically escapes it.
literalText :: Text -> DisplayTree
literalText value = TextLeaf (T.concat ["\"", T.concatMap escapeChar value, "\""])
  where
    escapeChar '"' = "\\\""
    escapeChar '\\' = "\\\\"
    escapeChar '\n' = "\\n"
    escapeChar '\t' = "\\t"
    escapeChar '\r' = "\\r"
    escapeChar c
      | c < ' ' || c == '\DEL' = T.pack ("\\x" <> showHex (fromEnum c) ";")
      | otherwise = T.singleton c

-- | A standalone text result is presented raw, bounded by the single tree
-- renderer rather than a separate truncation algorithm.
rawText :: Int -> Text -> (Text, Bool)
rawText budget value =
  let (rendered, remaining, unavailable) = renderTree budget (TextLeaf value)
  in (rendered, maybe False (const True) remaining || unavailable)

-- | Render at most the requested characters, retaining the actual remaining
-- tree. The last flag reports detail omitted by a legacy custom renderer.
renderTree :: Int -> DisplayTree -> (Text, Maybe DisplayTree, Bool)
renderTree allowance tree = go (max 0 allowance) [] False [tree]
  where
    go _ pieces unavailable [] = (T.concat (reverse pieces), Nothing, unavailable)
    go left pieces unavailable pending | left <= 0 =
      (T.concat (reverse pieces), Just (Concat pending), unavailable)
    go left pieces unavailable (node : rest) = case node of
      Constructor name fields -> go left pieces unavailable (constructorTree name fields : rest)
      Sequence opening closing children -> go left pieces unavailable (treeParts opening closing children : rest)
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
      Constructor name fields -> go count (constructorTree name fields : rest)
      Sequence opening closing children -> go count (treeParts opening closing children : rest)
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

-- | A semantic constructor retains field boundaries for independent expansion.
constructorTree :: Text -> [(Text, DisplayTree)] -> DisplayTree
constructorTree name [] = TextLeaf name
constructorTree name fields = treeParts (name <> " {") "}"
  [Concat [TextLeaf (label <> " = "), value] | (label, value) <- fields]

-- | Keys are issued only by the traversal and remain local to one display.
newtype ExpansionKey = ExpansionKey Int deriving (Eq, Ord, Show)

expansionKeyNumber :: ExpansionKey -> Int
expansionKeyNumber (ExpansionKey key) = key

expansionKeyFromNumber :: Int -> ExpansionKey
expansionKeyFromNumber = ExpansionKey

-- | The visible page and independently retained omitted subtrees. Keeping an
-- old state is harmless: it has no shared read position or authored effects.
data DisplayState = DisplayState Text [(ExpansionKey, Text, DisplayTree)] Int Bool

displayStateText :: DisplayState -> Text
displayStateText (DisplayState value _ _ _) = value

displayStateKeys :: DisplayState -> [(ExpansionKey, Text)]
displayStateKeys (DisplayState _ branches _ _) = [(key, label) | (key, label, _) <- branches]

displayStateUnavailable :: DisplayState -> Bool
displayStateUnavailable (DisplayState _ _ _ unavailable) = unavailable

newDisplayState :: Int -> DisplayTree -> DisplayState
newDisplayState budget tree =
  let (value, branches, next, unavailable) = preview (max 0 budget) 0 1 "value" tree
  in DisplayState value branches next unavailable

-- | Expand exactly one retained subtree, preserving the keys of its siblings.
-- Invalid or already-consumed keys do not force any retained value.
expandDisplayState :: Int -> ExpansionKey -> DisplayState -> Maybe DisplayState
expandDisplayState budget selected (DisplayState _ branches next _) = choose [] branches
  where
    choose _ [] = Nothing
    choose prefix ((key, label, tree) : rest)
      | key == selected =
          let (value, children, nextKey, unavailable) = preview (max 0 budget) 0 next label tree
          in Just (DisplayState value (reverse prefix ++ children ++ rest) nextKey unavailable)
      | otherwise = choose ((key, label, tree) : prefix) rest

-- The per-field allowance prevents a large first field from hiding a sibling.
-- The depth and item caps stop before inspecting collapsed values. Sequence
-- traversal looks at a bounded prefix, never its length or complete spine.
preview :: Int -> Int -> Int -> Text -> DisplayTree -> (Text, [(ExpansionKey, Text, DisplayTree)], Int, Bool)
preview budget depth next label tree
  | budget <= 0 || depth >= 2 = ("", [(ExpansionKey next, label, tree)], next + 1, False)
  | otherwise = case tree of
      Constructor name [] -> (T.take budget name, [], next, False)
      Constructor name fields -> children (name <> " {") "}" fields
      Sequence opening closing items -> sequenceChildren opening closing 0 items
      _ ->
        let (value, remaining, unavailable) = renderTree budget tree
        in case remaining of
          Nothing -> (value, [], next, unavailable)
          Just suffix -> (value, [(ExpansionKey next, label, suffix)], next + 1, unavailable)
  where
    children opening closing fields = walk opening [] next False fields
      where
        walk value keys key unavailable [] = (bounded value closing, keys, key, unavailable)
        walk value keys key unavailable pending@((field, child) : rest)
          | room value <= 0 =
              (bounded value closing, keys ++ [(ExpansionKey key, label <> ".fields", Constructor "" pending)], key + 1, unavailable)
          | otherwise =
              let prefix = (if value == opening then "" else ", ") <> field <> " = "
                  allowance = max 0 (min 128 (room (value <> prefix)))
                  (shown, omitted, following, childUnavailable) = preview allowance (depth + 1) key (label <> "." <> field) child
                  marker = if null omitted then "" else "…"
                  piece = value <> prefix <> shown <> marker
              in walk piece (keys ++ omitted) following (unavailable || childUnavailable) rest
        room value = budget - T.length value - T.length closing - 1
        bounded value suffix = T.take budget (value <> suffix)
    sequenceChildren opening closing count items
      | count >= 8 = (opening <> "…" <> closing, [(ExpansionKey next, label <> ".remainder", Sequence opening closing items)], next + 1, False)
      | otherwise = walk opening [] next False count items
      where
        walk value keys key unavailable _ [] = (T.take budget (value <> closing), keys, key, unavailable)
        walk value keys key unavailable index pending@(child : rest)
          | index >= 8 || budget - T.length value - T.length closing <= 2 =
              (T.take budget (value <> "…" <> closing), keys ++ [(ExpansionKey key, label <> ".remainder", Sequence opening closing pending)], key + 1, unavailable)
          | otherwise =
              let separator = if index == count then "" else ", "
                  allowance = max 0 (min 128 (budget - T.length value - T.length separator - T.length closing - 1))
                  (shown, omitted, following, childUnavailable) = preview allowance (depth + 1) key (label <> "[" <> T.pack (show index) <> "]") child
                  marker = if null omitted then "" else "…"
              in walk (value <> separator <> shown <> marker) (keys ++ omitted) following (unavailable || childUnavailable) (index + 1) rest
