{-# LANGUAGE DataKinds #-}
module ConstructorAdmissionConsumer where

import ConstructorAdmissionSupport
import Control.Monad.Freer (Eff)
import Tidepool.Internal.Resume (Settled, settle)

result :: Ticket
result = ticket

__prepared :: Settled '[] Ticket
__prepared = settle (pure result :: Eff '[] Ticket)
