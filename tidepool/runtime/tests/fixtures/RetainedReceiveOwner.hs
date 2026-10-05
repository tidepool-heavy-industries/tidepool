{-# LANGUAGE DataKinds, TypeApplications #-}
module RetainedReceiveOwner where

import Control.Monad.Freer (Eff)
import Tidepool.Aeson.Value (Value)
import Tidepool.Actor (receive)
import Tidepool.Effects.Core (ActorLocal)
import qualified RetainedReceiveSupport as Support

{-# OPAQUE result #-}
result :: Eff '[ActorLocal Maybe] Value
result = if Support.routeEven 6
  then receive @Value @Maybe (\_ -> error "mailbox handler is not invoked")
  else error "original recursive support selected the wrong branch"
