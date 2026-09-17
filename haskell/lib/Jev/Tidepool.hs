{-# OPTIONS_GHC -Wno-orphans #-}
{-# LANGUAGE LambdaCase #-}

-- | 'JsonValue' for Tidepool's own 'Tidepool.Aeson.Value.Value' (the
-- vendored aeson-shaped JSON type used by the Tidepool stdlib — there is no
-- aeson package here). Mirrors "Jev.Aeson", the upstream instance for
-- aeson's 'Value'.
module Jev.Tidepool () where

import qualified Data.Map.Strict as Map
import qualified Tidepool.Aeson.Value as V
import Jev.Core.Json

instance JsonValue V.Value where
  jNull = V.Null
  jBool = V.Bool
  jNumber = V.Number . V.fromFloatDigits
  jString = V.String
  jArray = V.Array
  jObject = V.object
  jView = \case
    V.Null -> VNull
    V.Bool b -> VBool b
    V.Number n -> VNumber (V.toRealFloat n)
    V.String s -> VString s
    V.Array xs -> VArray xs
    V.Object kv -> VObject (map (\(k, v) -> (V.toText k, v)) (Map.toList kv))
  -- Tidepool's 'Data.Scientific'-alike 'Eq' is exact (numeric value, not a
  -- 'Double' round-trip), and 'Data.Map.Strict' Eq is already
  -- order-insensitive on members — so structural derived 'Eq' on 'V.Value'
  -- is exactly what 'jEqual' wants, same as upstream's aeson instance.
  jEqual = (==)
