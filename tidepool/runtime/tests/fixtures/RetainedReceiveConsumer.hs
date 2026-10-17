{-# LANGUAGE PackageImports, DataKinds, OverloadedStrings #-}
module RetainedReceiveConsumer where

import qualified RetainedReceiveOwner as Original
import Control.Monad.Freer (Eff)
import Tidepool.Aeson.Value (Value(..), (.=), scientific)
import qualified Data.Map.Strict as Map
import "tidepool-resume" Tidepool.Internal.Resume (settle, resumeLifted)

__prepared = settle Original.result
__resume q x = settle (resumeLifted q x)

-- A separate machine produces answers against the same retained canonical context.
payloads :: Eff '[] (Value, Value)
payloads = pure (payload 0, payload 1)

{-# OPAQUE payload #-}
payload :: Int -> Value
payload index = Object (Map.fromList ["index" .= index, "mixed" .= mixed])
  where
    mixed = Array [Bool True, Bool False, Null, Number (scientific 42 0),
      String "text", objectValue]
    objectValue = Object (Map.fromList ["nested" .= Array []])

__payload = settle payloads
