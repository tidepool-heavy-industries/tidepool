{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE UndecidableInstances #-}

-- | Workbench presentation. Summaries describe observations, never acceptance.
module Tidepool.Inspection
  ( WorkbenchDisplay (..)
  , FullInspection
  , FullDisplay (inspectFull)
  ) where

import Data.Text (Text)
import qualified Data.Text as Text
import Prelude
import Tidepool.Agent.Reply.Internal
import Tidepool.Agent.Watch.Internal

-- | The Bool reports omitted detail. The character limit bounds the demanded
-- Show prefix, not the evaluation time of an arbitrary Show implementation.
class WorkbenchDisplay a where
  workbenchDisplay :: a -> (Text, Bool)
  -- | Assignments need readable instructions on arrival. Structured inputs
  -- retain their ordinary compact presentation; the host also caps UTF-8 bytes.
  workbenchActivationDisplay :: Int -> a -> (Text, Bool)
  workbenchActivationDisplay _ = workbenchDisplay

instance {-# OVERLAPPABLE #-} Show a => WorkbenchDisplay a where
  workbenchDisplay value =
    let prefix = take 513 (show value)
    in (Text.pack (take 512 prefix), length prefix > 512)

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

instance {-# OVERLAPPABLE #-} Show a => FullDisplay a where
  inspectFull value = FullInspection (Text.pack (show value))

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
