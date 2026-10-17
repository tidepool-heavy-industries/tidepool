{-# LANGUAGE DataKinds, ExplicitForAll #-}
module Tidepool.Scope.Internal where

import Control.Monad.Freer (Eff)
import Tidepool.Internal.RequestSite (RequestSite)

{-# OPAQUE runScope #-}
runScope :: forall answer. answer -> Eff '[] answer
runScope = pure

{-# OPAQUE runScopeSited #-}
runScopeSited :: forall answer. RequestSite '[] answer -> answer -> Eff '[] answer
runScopeSited _ = pure
