{-# LANGUAGE OverloadedStrings #-}
module JsonAuthorityContract where

import Data.Text (Text)
import Tidepool.Aeson.Value
import GHC.Magic (lazy)

unrelated :: Int
unrelated = 42

-- The host replaces this payload-independent carrier without entering it.
{-# NOINLINE hostValue #-}
hostValue :: Value
hostValue = lazy hostValue

{-# NOINLINE polymorphicValue #-}
polymorphicValue :: a -> Value
polymorphicValue _ = hostValue

encodeOnly :: Value -> Text
encodeOnly = encodeValue

decodeOnly :: Text -> Either Text Value
decodeOnly = eitherDecodeValue

nestedValue :: Maybe Value
nestedValue = Nothing

result :: Text
result = case eitherDecodeValue "{\"number\":42,\"array\":[true,false]}" of
  Left message -> message
  Right value -> encodeValue value
