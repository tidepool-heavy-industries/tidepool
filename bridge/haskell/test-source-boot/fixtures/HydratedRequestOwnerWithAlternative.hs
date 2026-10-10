{-# LANGUAGE DataKinds #-}
{-# LANGUAGE ExplicitForAll #-}
module Tidepool.Actors.Internal.Agent where
import Tidepool.Internal.RequestSite (RequestSite)
import Tidepool.Agent.Reply.Internal (ResponseResult)

keepResponseResultAuthority :: Maybe (ResponseResult Bool)
keepResponseResultAuthority = Nothing

{-# OPAQUE request #-}
request :: forall result input. input -> Maybe result
request _ = Nothing

{-# OPAQUE requestSited #-}
requestSited :: forall result input. RequestSite '[input, ResponseResult result] result -> input -> Maybe result
requestSited _ _ = Nothing

{-# OPAQUE requestAlternativeSited #-}
requestAlternativeSited :: forall result input.
  RequestSite '[input, ResponseResult result] result -> input -> Maybe result
requestAlternativeSited _ _ = Nothing
