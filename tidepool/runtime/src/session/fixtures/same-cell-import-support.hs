{-# LANGUAGE QuasiQuotes #-}
module SameCellImportSupport (answerValue) where

import Tidepool.Agent.Assignment (Label, labelText)
import qualified Tidepool.Data.Text as T
import Tidepool.QQ.Label (label)

answerValue :: Int
answerValue = T.length (labelText ([label|same-cell-quote-proof|] :: Label))
