{-# LANGUAGE PackageImports #-}
{-# LANGUAGE DataKinds, TypeApplications, OverloadedStrings #-}
module JsonReply where

import Control.Monad.Freer (Eff)
import Tidepool.Aeson.Value (Value(..), object, (.=), scientific)
import Tidepool.Actor (receive)
import Tidepool.Effects.Core (ActorLocal)
import "tidepool-resume" Tidepool.Internal.Resume (settle, resumeLifted)

-- The request carries a mailbox handler. This machine-level control resumes
-- the receiver's typed next value directly without invoking that handler.
result :: Eff '[ActorLocal Maybe] Value
result = receive @Value @Maybe (\_ -> error "mailbox handler is not invoked")

__prepared = settle result
__resume q x = settle (resumeLifted q x)

-- These values are produced by Haskell and cross through the same live parcel
-- boundary as an actor reply. The host never invents their type authority.
payloads :: Eff '[] (Value, Value)
payloads = pure (payload 0, payload 1)
  where
    payload index = object ["index" .= (index :: Int), "mixed" .= mixed]
    mixed = Array [Bool True, Bool False, Null, Number (scientific 42 0),
      String "text", object ["nested" .= Array []]]

__payload = settle payloads
