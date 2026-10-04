{-# LANGUAGE DataKinds #-}
{-# LANGUAGE ExplicitForAll #-}

module Tidepool.Actor where

import Tidepool.Internal.RequestSite (RequestSite)

{-# OPAQUE receive #-}
receive :: forall answer. String -> Maybe answer
receive _ = Nothing

receiveSited :: forall answer. RequestSite '[] answer -> String -> Maybe answer
receiveSited _ _ = Nothing
