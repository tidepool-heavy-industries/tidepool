{-# LANGUAGE DataKinds #-}
{-# LANGUAGE ExplicitForAll, KindSignatures #-}
module Tidepool.Actors.Unfold where
import Data.Kind (Type)
import Tidepool.Internal.RequestSite (RequestSite)
import Tidepool.Agent.Reply.Internal (ResponseResult)

keepResponseResultAuthority :: Maybe (ResponseResult Bool)
keepResponseResultAuthority = Nothing

{-# OPAQUE child #-}
child :: forall result (child :: Type) input (parent :: Type). input -> Maybe result
child _ = Nothing

{-# OPAQUE childSited #-}
childSited :: forall result (child :: Type) input (parent :: Type). RequestSite '[input] result -> input -> Maybe result
childSited _ _ = Nothing
