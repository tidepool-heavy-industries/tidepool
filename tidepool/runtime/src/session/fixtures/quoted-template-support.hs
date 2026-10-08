{-# LANGUAGE QuasiQuotes #-}
module QuotedTemplateSupport (taskValue) where

import qualified Tidepool.Data.Text as T
import Tidepool.QQ.Fmt (fmt)

{-# OPAQUE taskValue #-}
taskValue :: Int -> Int
taskValue value = value + T.length ([fmt|x|])
