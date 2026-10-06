{-# LANGUAGE QuasiQuotes #-}
module ShadowQuoted (value) where

import Tidepool.QQ.Validate (uri)

value :: Int
value = [uri|QUOTE_INPUT_PATH|]
