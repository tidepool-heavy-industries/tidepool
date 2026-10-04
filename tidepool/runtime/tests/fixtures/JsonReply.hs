{-# LANGUAGE DataKinds, TypeApplications #-}
module JsonReply where

import Control.Monad.Freer (Eff)
import Tidepool.Aeson.Value (Value)
import Tidepool.Actor (receive)
import Tidepool.Effects.Core (ActorLocal)
import Tidepool.Internal.Resume (settle, resumeLifted)

-- The request carries a mailbox handler. This machine-level control resumes
-- the receiver's typed next value directly without invoking that handler.
result :: Eff '[ActorLocal Maybe] Value
result = receive @Value @Maybe (\_ -> error "mailbox handler is not invoked")

__prepared = settle result
__resume q x = settle (resumeLifted q x)
