{-# LANGUAGE ExplicitForAll #-}
module Tidepool.Actor where

import Tidepool.Internal.RequestSite ()

{-# OPAQUE receive #-}
receive :: forall answer. String -> Maybe answer
receive _ = Nothing
