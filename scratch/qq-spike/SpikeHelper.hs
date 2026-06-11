-- | Home-module helper referenced from inside a TH quote.
module SpikeHelper (shout) where

import Data.Text (Text)
import qualified Data.Text as T

shout :: Text -> Text
shout t = T.toUpper t <> T.pack "!"
