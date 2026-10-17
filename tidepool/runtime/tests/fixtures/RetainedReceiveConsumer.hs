{-# LANGUAGE PackageImports, DataKinds, OverloadedStrings #-}
module RetainedReceiveConsumer where

import qualified RetainedReceiveOwner as Original
import Control.Monad.Freer (Eff)
import Tidepool.Aeson.Value (Value(..), object, (.=), scientific)
import "tidepool-resume" Tidepool.Internal.Resume (settle, resumeLifted)

__prepared = settle Original.result
__resume q x = settle (resumeLifted q x)

-- A separate machine produces answers against the same retained canonical context.
payloads :: Eff '[] (Value, Value)
payloads = pure (payload 0, payload 1)
  where
    payload index = object ["index" .= (index :: Int), "mixed" .= mixed]
    mixed = Array [Bool True, Bool False, Null, Number (scientific 42 0),
      String "text", object ["nested" .= Array []]]

__payload = settle payloads
