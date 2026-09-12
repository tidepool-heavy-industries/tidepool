{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}

module IntrospectionCase where

import Control.Monad.Freer (Eff, Member)
import Data.Proxy qualified

data Allowed result where
  Allowed :: Allowed ()

data Forbidden result where
  Forbidden :: Forbidden ()

allowed :: Member Allowed effects => Eff effects ()
allowed = allowed

forbidden :: Member Forbidden effects => Eff effects ()
forbidden = forbidden

fixedWrongRow :: Eff '[Forbidden] ()
fixedWrongRow = fixedWrongRow

polymorphic :: Member Allowed input => Eff input () -> Eff output ()
polymorphic = polymorphic

requiresShow :: Show value => value -> Int
requiresShow = requiresShow

pureValue :: Int
pureValue = 42

__tidepool_lookup_row :: Data.Proxy.Proxy '[Allowed]
__tidepool_lookup_row = Data.Proxy.Proxy

__tidepool_lookup_query :: Eff effects ()
__tidepool_lookup_query = __tidepool_lookup_query

__tidepool_inspect_0 :: Eff '[Allowed] ()
__tidepool_inspect_0 = allowed
