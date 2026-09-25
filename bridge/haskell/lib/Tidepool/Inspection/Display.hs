{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE UndecidableInstances #-}

-- | The 'Display' and 'WorkbenchDisplay' classes and their generic
-- instances, split out of "Tidepool.Inspection" so a module the workbench
-- boundary itself depends on (a reply's rendered preview, an unfold child's
-- activation preview) can use these classes without importing the request
-- and watch types "Tidepool.Inspection" also renders. Those richer
-- instances (for 'Tidepool.Agent.Reply.Internal.ResponseState' and kin)
-- remain in "Tidepool.Inspection", which re-exports everything here.
module Tidepool.Inspection.Display
  ( Display (..),
    WorkbenchDisplay (..),
    application,
  )
where

import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Inspection.Tree
import Prelude

-- | The Bool reports omitted detail. The character limit bounds the demanded
-- Show prefix, not the evaluation time of an arbitrary Show implementation.
class WorkbenchDisplay a where
  workbenchDisplay :: a -> (Text, Bool)

  -- | Omit payloads already presented by effects in the current expression.
  -- Identity keys are opaque; explicitly inspecting the value later shows it again.
  workbenchDisplayWithout :: [Text] -> a -> (Text, Bool)
  workbenchDisplayWithout _ = workbenchDisplay

  -- | Assignments arrive whole: render up to the given character budget
  -- (the host's activation cap, which also caps UTF-8 bytes) rather than the
  -- compact 'workbenchDisplay' preview.
  workbenchActivationDisplay :: Int -> a -> (Text, Bool)
  workbenchActivationDisplay _ = workbenchDisplay

-- | Budgeted text rendering. Containers pass their remaining budget to children.
class Display a where
  displayWith :: Int -> a -> (Text, Bool)
  displayWith budget value =
    let (rendered, remaining, unavailable) = renderTree budget (displayTree value)
    in (rendered, maybe False (const True) remaining || unavailable)

  -- | Structural renderers retain the unconsumed tree. Existing custom
  -- displayWith instances remain bounded, but must implement this method to
  -- offer resumable detail rather than an explicitly unavailable remainder.
  displayTree :: a -> DisplayTree
  displayTree value = LegacyLeaf (\budget -> displayWith budget value)

  {-# MINIMAL displayWith | displayTree #-}

  -- | The tree at a 'showsPrec' precedence, so a constructor application is
  -- parenthesized exactly where derived 'Show' would parenthesize it. Instances
  -- without applications (atoms, brackets, custom layouts) need not define it.
  displayTreePrec :: Int -> a -> DisplayTree
  displayTreePrec _ = displayTree

  displayWithout :: [Text] -> Int -> a -> (Text, Bool)
  displayWithout _ = displayWith

-- | One constructor applied to arguments, each rendered as an application argument.
application :: Int -> Text -> [DisplayTree] -> DisplayTree
application precedence constructor arguments =
  precedenceParens precedence (Concat (TextLeaf constructor : concatMap (\argument -> [TextLeaf " ", argument]) arguments))

instance {-# OVERLAPPABLE #-} (Show a) => Display a where
  displayTree = displayTreePrec 0
  displayTreePrec precedence value = StringLeaf (showsPrec precedence value "")
  displayWith budget value =
    let limit = max 0 (min (maxBound - 1) budget)
        prefix = take (limit + 1) (show value)
     in (Text.pack (take limit prefix), length prefix > limit)

-- | One text rule: nested (`displayTree`) text is a quoted, escaped literal;
-- a standalone (`displayWith`) text renders raw, as 'WorkbenchDisplay' and a
-- raw tool's output do.
instance Display Text where
  displayTree = literalText
  displayTreePrec _ = literalText
  displayWith = rawText

-- | A 'String' is text, and renders as 'Text' does. Without this the list
-- instance answers for @[Char]@ and the result of 'show' displays as a list of
-- characters, one to a line.
instance {-# OVERLAPPING #-} Display [Char] where
  displayTree = displayTree . Text.pack
  displayTreePrec precedence = displayTreePrec precedence . Text.pack
  displayWith budget = displayWith budget . Text.pack

instance Display (a -> b) where
  displayTree _ = TextLeaf "<function>"

instance (Display a) => Display (Maybe a) where
  displayTree = displayTreePrec 0
  displayTreePrec _ Nothing = TextLeaf "Nothing"
  displayTreePrec precedence (Just value) = application precedence "Just" [displayTreePrec 11 value]
  displayWithout _ budget Nothing = rawText budget "Nothing"
  displayWithout keys budget (Just value) = renderParts budget "Just " "" [\n -> displayWithout keys n value]
  displayWith budget Nothing = rawText budget "Nothing"
  displayWith budget (Just value) = renderParts budget "Just " "" [\n -> displayWith n value]

instance (Display a, Display b) => Display (Either a b) where
  displayTree = displayTreePrec 0
  displayTreePrec precedence (Left value) = application precedence "Left" [displayTreePrec 11 value]
  displayTreePrec precedence (Right value) = application precedence "Right" [displayTreePrec 11 value]
  displayWithout keys budget (Left value) = renderParts budget "Left " "" [\n -> displayWithout keys n value]
  displayWithout keys budget (Right value) = renderParts budget "Right " "" [\n -> displayWithout keys n value]
  displayWith budget (Left value) = renderParts budget "Left " "" [\n -> displayWith n value]
  displayWith budget (Right value) = renderParts budget "Right " "" [\n -> displayWith n value]

instance {-# OVERLAPPING #-} (Display a) => Display [a] where
  displayTree = treeParts "[" "]" . map displayTree
  displayWithout keys budget values = renderParts budget "[" "]" (map (\value n -> displayWithout keys n value) values)
  displayWith budget values = renderParts budget "[" "]" (map (\value n -> displayWith n value) values)

instance (Display a, Display b) => Display (a, b) where
  displayTree (a, b) = treeParts "(" ")" [displayTree a, displayTree b]
  displayWithout keys budget (a, b) = renderParts budget "(" ")" [\n -> displayWithout keys n a, \n -> displayWithout keys n b]
  displayWith budget (a, b) = renderParts budget "(" ")" [\n -> displayWith n a, \n -> displayWith n b]

instance (Display a, Display b, Display c) => Display (a, b, c) where
  displayTree (a, b, c) = treeParts "(" ")" [displayTree a, displayTree b, displayTree c]
  displayWithout keys budget (a, b, c) = renderParts budget "(" ")" [\n -> displayWithout keys n a, \n -> displayWithout keys n b, \n -> displayWithout keys n c]
  displayWith budget (a, b, c) = renderParts budget "(" ")" [\n -> displayWith n a, \n -> displayWith n b, \n -> displayWith n c]

renderParts :: Int -> Text -> Text -> [Int -> (Text, Bool)] -> (Text, Bool)
renderParts budget opening closing values =
  let (body, omitted) = go (max 0 (budget - Text.length opening - Text.length closing)) [] values
      (text, clipped) = rawText budget (opening <> body <> closing)
   in (text, omitted || clipped)
  where
    go _ accumulated [] = (Text.concat (reverse accumulated), False)
    go remaining accumulated _ | remaining <= 0 = (Text.concat (reverse accumulated), True)
    go remaining accumulated (value : rest) =
      let separator = if null accumulated then "" else ",\n"
          (text, omitted) = value (max 0 (remaining - Text.length separator))
          next = text : separator : accumulated
       in if omitted
            then (Text.concat (reverse next), True)
            else go (remaining - Text.length separator - Text.length text) next rest

instance {-# OVERLAPPABLE #-} (Display a) => WorkbenchDisplay a where
  workbenchDisplay = displayWith 512
  workbenchDisplayWithout keys = displayWithout keys 512
  workbenchActivationDisplay = displayWith

instance WorkbenchDisplay Text where
  workbenchDisplay = rawText 512
  workbenchActivationDisplay limit = rawText limit

-- | A top-level 'String' renders raw, mirroring 'Text'. Without this the
-- 'Display'-derived {-# OVERLAPPABLE #-} instance answers instead and quotes it.
instance WorkbenchDisplay [Char] where
  workbenchDisplay = workbenchDisplay . Text.pack
  workbenchActivationDisplay limit = workbenchActivationDisplay limit . Text.pack
