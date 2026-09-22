{-# LANGUAGE OverloadedStrings #-}
module JsonIntrinsicDemand where

import qualified Data.Map.Strict as Map
import qualified Data.Text
import JsonIntrinsicDemandPayload (importedBody, importedText)
import Tidepool.Aeson.Value

result :: Int
result = case (encodeDynamic True, decodeDynamic importedText, encodeValue importedBody) of
  ("{\"dynamic\":true}", Right (Object decoded), "{\"preserved\":true}")
    | Map.lookup "decoded" decoded == Just (Bool True) -> 1
  _ -> 0

{-# OPAQUE encodeDynamic #-}
encodeDynamic :: Bool -> Data.Text.Text
encodeDynamic flag = encodeValue (Object (Map.fromList [("dynamic", Bool flag)]))

{-# OPAQUE decodeDynamic #-}
decodeDynamic :: Data.Text.Text -> Either Data.Text.Text Value
decodeDynamic input = eitherDecodeValue input
