{-# LANGUAGE OverloadedStrings #-}
module JsonIntrinsicDemand where

import qualified Data.Map.Strict as Map
import JsonIntrinsicDemandPayload (importedBody, importedText)
import Tidepool.Aeson.Value

result :: Int
result = case (encodeValue importedBody, eitherDecodeValue importedText) of
  ("{\"preserved\":true}", Right (Object decoded))
    | Map.lookup "decoded" decoded == Just (Bool True) -> 1
  _ -> 0
