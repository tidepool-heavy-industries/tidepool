{-# LANGUAGE OverloadedStrings #-}
module Project.PositiveReport (PositiveReport(..), UnsupportedReport(..)) where

import Prelude
import Data.Proxy (Proxy(..))
import Project.Researcher (Report(..))
import Tidepool.Aeson (FromJSON(..), Result(..), object, (.=))
import Tidepool.Aeson.Schema (JsonSchema(..))
import Tidepool.Inspection.Display (Display(..), WorkbenchDisplay(..))

-- Same schema as Report, with a semantic check owned by FromJSON. This
-- distinguishes structural validation from the compiled Haskell decoder.
newtype PositiveReport = PositiveReport Report
instance JsonSchema PositiveReport where
  jsonSchema _ = jsonSchema (Proxy :: Proxy Report)
instance FromJSON PositiveReport where
  parseJSON value = do
    report <- parseJSON value
    if summary report == ""
      then Error "checked report requires a nonempty summary"
      else pure (PositiveReport report)
instance WorkbenchDisplay PositiveReport where
  workbenchDisplay (PositiveReport report) = workbenchDisplay report
instance Display PositiveReport where
  displayView (PositiveReport report) = displayView report

-- The ordinary result decoder is valid; only its authored schema is unsupported.
newtype UnsupportedReport = UnsupportedReport Report
instance JsonSchema UnsupportedReport where
  jsonSchema _ = object ["type" .= ("unsupported-report" :: String)]
instance FromJSON UnsupportedReport where
  parseJSON value = UnsupportedReport <$> parseJSON value
instance WorkbenchDisplay UnsupportedReport where
  workbenchDisplay (UnsupportedReport report) = workbenchDisplay report
