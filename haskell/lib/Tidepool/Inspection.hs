{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE UndecidableInstances #-}

-- | Workbench presentation. Summaries describe observations, never acceptance.
module Tidepool.Inspection
  ( Display (..),
    renderText,
    WorkbenchDisplay (..),
    FullInspection,
    FullDisplay (inspectFull),
  )
where

import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Agent.Reply.Internal
import Tidepool.Agent.Watch.Internal
import Prelude

-- | The Bool reports omitted detail. The character limit bounds the demanded
-- Show prefix, not the evaluation time of an arbitrary Show implementation.
class WorkbenchDisplay a where
  workbenchDisplay :: a -> (Text, Bool)

  -- | Assignments need readable instructions on arrival. Structured inputs
  -- retain their ordinary compact presentation; the host also caps UTF-8 bytes.
  workbenchActivationDisplay :: Int -> a -> (Text, Bool)
  workbenchActivationDisplay _ = workbenchDisplay

-- | Budgeted text rendering. Containers pass their remaining budget to children.
class Display a where
  displayWith :: Int -> a -> (Text, Bool)

renderText :: Int -> Text -> (Text, Bool)
renderText budget value = (Text.take (max 0 budget) value, Text.length value > max 0 budget)

instance {-# OVERLAPPABLE #-} (Show a) => Display a where
  displayWith budget value =
    let limit = max 0 (min (maxBound - 1) budget)
        prefix = take (limit + 1) (show value)
     in (Text.pack (take limit prefix), length prefix > limit)

instance Display Text where
  displayWith = renderText

instance (Display a) => Display (Maybe a) where
  displayWith budget Nothing = renderText budget "Nothing"
  displayWith budget (Just value) = renderParts budget "Just " "" [\n -> displayWith n value]

instance (Display a, Display b) => Display (Either a b) where
  displayWith budget (Left value) = renderParts budget "Left " "" [\n -> displayWith n value]
  displayWith budget (Right value) = renderParts budget "Right " "" [\n -> displayWith n value]

instance {-# OVERLAPPING #-} (Display a) => Display [a] where
  displayWith budget values = renderParts budget "[" "]" (map (\value n -> displayWith n value) values)

instance (Display a, Display b) => Display (a, b) where
  displayWith budget (a, b) = renderParts budget "(" ")" [\n -> displayWith n a, \n -> displayWith n b]

instance (Display a, Display b, Display c) => Display (a, b, c) where
  displayWith budget (a, b, c) = renderParts budget "(" ")" [\n -> displayWith n a, \n -> displayWith n b, \n -> displayWith n c]

renderParts :: Int -> Text -> Text -> [Int -> (Text, Bool)] -> (Text, Bool)
renderParts budget opening closing values =
  let (body, omitted) = go (max 0 (budget - Text.length opening - Text.length closing)) [] values
      (text, clipped) = renderText budget (opening <> body <> closing)
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

instance WorkbenchDisplay Text where
  workbenchDisplay value =
    let prefix = Text.take 513 value
     in (Text.take 512 prefix, Text.length prefix > 512)
  workbenchActivationDisplay limit value =
    let prefix = Text.take (limit + 1) value
     in (Text.take limit prefix, Text.length prefix > limit)

newtype FullInspection = FullInspection Text

-- | Render a saved value explicitly. Text is already a presentation; other
-- values use Show by default, which can be expensive or fail.
class FullDisplay a where
  inspectFull :: a -> FullInspection

instance {-# OVERLAPPABLE #-} (Display a) => FullDisplay a where
  inspectFull value = FullInspection (fst (displayWith (maxBound - 1) value))

instance FullDisplay Text where
  inspectFull = FullInspection

instance WorkbenchDisplay FullInspection where
  workbenchDisplay (FullInspection text) = (text, False)

instance WorkbenchDisplay (ResponseResult a) where
  workbenchDisplay value =
    ("ResponseReady · " <> Text.pack (show (responseExecution value)), True)

instance WorkbenchDisplay (ResponseState a) where
  workbenchDisplay ResponsePending = ("ResponsePending", False)
  workbenchDisplay (ResponseCancellationPending reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("ResponseCancellationPending · " <> text, omitted)
  workbenchDisplay (ResponseReady value) = workbenchDisplay value
  workbenchDisplay (ResponseUnavailable reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("ResponseUnavailable · " <> text, omitted)

instance WorkbenchDisplay (WatchState a) where
  workbenchDisplay WatchPending = ("WatchPending", False)
  workbenchDisplay (WatchReady _) = ("WatchReady", True)
  workbenchDisplay (WatchUnavailable reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("WatchUnavailable · " <> text, omitted)

instance WorkbenchDisplay (ProgressState a) where
  workbenchDisplay ProgressPending = ("ProgressPending", False)
  workbenchDisplay (ProgressUpdate cursor _) =
    ("ProgressUpdate · " <> Text.pack (show cursor), True)
  workbenchDisplay ProgressClosed = ("ProgressClosed", False)
  workbenchDisplay (ProgressRejected reason) =
    let (text, omitted) = workbenchDisplay reason
     in ("ProgressRejected · " <> text, omitted)
