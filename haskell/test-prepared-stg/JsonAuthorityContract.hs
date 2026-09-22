{-# LANGUAGE OverloadedStrings #-}
module JsonAuthorityContract where

import Data.Text (Text)
import Tidepool.Aeson.Value

result :: Text
result = case eitherDecodeValue "{\"number\":42,\"array\":[true,false]}" of
  Left message -> message
  Right value -> encodeValue value
