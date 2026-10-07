{-# LANGUAGE QuasiQuotes #-}
module SameCellImportSupport (answerValue) where

import qualified Tidepool.Data.Text as T
import Tidepool.QQ.Fmt (fmt)

answerValue :: Int
answerValue = T.length ([fmt|same-cell-quote-proof|])
