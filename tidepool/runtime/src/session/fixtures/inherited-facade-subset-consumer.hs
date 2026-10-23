{-# LANGUAGE DataKinds #-}
{-# LANGUAGE PackageImports #-}
{-# LANGUAGE TypeFamilies #-}
module InheritedFacadeConsumer where

import "freer-simple" Control.Monad.Freer (Eff)
import "tidepool-resume" Tidepool.Internal.Resume (Settled, settle)
import DECLARATION_FACADE_MODULE

__prepared :: Settled '[] Int
__prepared = settle (pure (keptNumber (Kept (41 :: KeptPayload Bool))) :: Eff '[] Int)
