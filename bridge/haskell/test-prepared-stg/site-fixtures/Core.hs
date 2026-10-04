{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DataKinds, FlexibleContexts, KindSignatures #-}
{-# LANGUAGE ExplicitForAll, TypeApplications, TypeOperators #-}
-- | The production verb shape over freer-simple's 'Member'. Its instances
-- resolve `Member V (W ': effects)` from a given `Member V effects`, and GHC
-- wraps such a call in 'GHC.Magic.nospec'.
module Tidepool.Actor
  ( Replies, Other, Member, Eff, receive, receiveSited ) where

import Control.Monad.Freer (Eff, Member)
import Data.Kind (Type)
import Tidepool.Internal.RequestSite (RequestSite)

data Replies (a :: Type)
data Other (a :: Type)

{-# OPAQUE receive #-}
receive :: forall a effects. Member Replies effects => String -> Eff effects a
receive _ = error "surface verb is rewritten to its site-aware sibling"
{-# OPAQUE receiveSited #-}
receiveSited :: forall a effects. Member Replies effects => RequestSite '[] a -> String -> Eff effects a
receiveSited _ _ = error "site-aware sibling is not executed by this test"
