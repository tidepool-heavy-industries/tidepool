{-# LANGUAGE NoImplicitPrelude #-}
module Tidepool.Data.Time (UTCTime(..), parseISO8601, epochMillis) where

import Prelude (Either(..), Int)
import Data.Text (Text)

newtype UTCTime = UTCTime Int

{-# OPAQUE parseISO8601 #-}
parseISO8601 :: Text -> Either Text UTCTime
parseISO8601 input = Left input

epochMillis :: UTCTime -> Int
epochMillis (UTCTime value) = value
