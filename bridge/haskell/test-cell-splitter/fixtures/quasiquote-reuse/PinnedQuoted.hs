{-# LANGUAGE QuasiQuotes #-}
module PinnedQuoted (value) where

import Data.Text (Text)
import Tidepool.QQ.Validate (uri)

value :: Text
value = [uri|https://example.test/stable|]
