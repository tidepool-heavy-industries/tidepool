{-# LANGUAGE DataKinds, DeriveGeneric, DeriveAnyClass, TypeOperators,
             FlexibleContexts, FlexibleInstances, UndecidableInstances,
             NoImplicitPrelude, OverloadedStrings #-}
module Tidepool.Session.Lib.G1 (Request(..), Response(..), Tools(..), policy) where
import Control.Monad.Freer (Eff)
import Tidepool.Prelude
import Tidepool.Agent.Contract
import Tidepool.Effects

data Request = Request { value :: Int } deriving (Generic, FromJSON, JsonSchema)
data Response = Response { success :: Bool } deriving (Generic, ToJSON, JsonSchema)
data Tools mode = Tools { perform :: mode :- Call Request Response } deriving (Generic)

policy :: Eff '[AgentTools] ()
policy = serveToolsWith () $ \_ ->
  Tools { perform = tool "Return a typed response." $ \_ -> pure (Response True) }
