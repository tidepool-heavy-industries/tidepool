{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, RankNTypes #-}
-- | Optics over Text: van Laarhoven traversals for line-level surgery,
-- composable with the Control.Lens operators the Prelude already
-- re-exports (&, %~, .~, toListOf, ^..).
--
--   "a\nbb\nccc" ^.. linesOf . filteredT (\l -> len l > 1)   -- ["bb","ccc"]
--   t & linesOf . filteredT (isPrefixOf "--") %~ toUpper      -- shout comments
--
-- File-level appliers report what changed instead of editing blind:
--
--   overFileM "notes.md" (filteredT (isInfixOf "TODO")) (replace "TODO" "DONE")
module Optics where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

-- | Van Laarhoven traversal whose targets are Text within Text.
-- RankNTypes alias so traversals can be passed as arguments.
type TextTraversal = forall f. Applicative f => (Text -> f Text) -> Text -> f Text

-- | Traverse each line of a Text. Uses splitOn/intercalate (NOT
-- lines/unlines) so the roundtrip is exact — no phantom trailing newline.
linesOf :: TextTraversal
linesOf f t = intercalate "\n" <$> traverse f (splitOn "\n" t)

-- | Restrict a traversal to targets matching a predicate.
-- Composes on the right: @linesOf . filteredT p@.
-- (Named filteredT to avoid clashing if lens's filtered ever lands.)
filteredT :: (Text -> Bool) -> TextTraversal
filteredT p f x = if p x then f x else pure x

-- | Apply a pure rewrite through a traversal to a file on disk.
-- Returns {file, targets, changed, written}: how many targets the
-- traversal hit, how many the rewrite actually altered, and whether the
-- file was touched (no-op rewrites never write).
overFileM :: Text -> TextTraversal -> (Text -> Text) -> M Value
overFileM path trav f = do
  src <- readFile path
  let targets = toListOf trav src
      changed = length (filter (\x -> f x /= x) targets)
      out = src & trav %~ f
  when (changed > 0) (writeFile path out)
  pure (object
    [ "file" .= path
    , "targets" .= length targets
    , "changed" .= changed
    , "written" .= (changed > 0)
    ])

-- | Set every target of a traversal in a file to a constant value.
-- Same change-count report as overFileM.
setFileM :: Text -> TextTraversal -> Text -> M Value
setFileM path trav newVal = overFileM path trav (\_ -> newVal)
