{-# LANGUAGE QuasiQuotes #-}
module QuotedTemplateSupport (taskValue) where

import Tidepool.Agent.Assignment (Label, labelText)
import qualified Tidepool.Data.Text as T
import Tidepool.QQ.Label (label)

{-# OPAQUE taskValue #-}
taskValue :: Int -> Int
taskValue value = value + T.length (labelText ([label|x|] :: Label))
