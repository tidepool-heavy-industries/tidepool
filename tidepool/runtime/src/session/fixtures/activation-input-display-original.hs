module ActivationDisplayOriginal (Task, make) where

import qualified Data.Text as Text
import Tidepool.Inspection.Display (Display (..), WorkbenchDisplay (..))

newtype Task = Task Int

make :: Int -> Task
make = Task

instance Display Task where
  displayWith budget (Task value) =
    let text = Text.pack (if value == 42 then "original task display 42" else "wrong original input")
    in (Text.take budget text, Text.length text > budget)

instance WorkbenchDisplay Task where
  workbenchDisplay = displayWith 65536
  workbenchActivationDisplay = displayWith
