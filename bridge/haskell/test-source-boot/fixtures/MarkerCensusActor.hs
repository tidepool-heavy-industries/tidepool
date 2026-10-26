{-# LANGUAGE DataKinds #-}
{-# LANGUAGE ExplicitForAll #-}
module Tidepool.Actor where
import Tidepool.Internal.RequestSite (RequestSite)

{-# OPAQUE serve #-}
serve :: forall result. result -> result
serve value = value
{-# OPAQUE serveSited #-}
serveSited :: forall result. RequestSite '[] result -> result -> result
serveSited _ value = value

{-# OPAQUE receive #-}
receive :: forall result. result -> result
receive value = value
{-# OPAQUE receiveSited #-}
receiveSited :: forall result. RequestSite '[] result -> result -> result
receiveSited _ value = value

{-# OPAQUE ordinary #-}
ordinary :: Int -> Int
ordinary value = value + 1
