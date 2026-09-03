{-# LANGUAGE DataKinds #-}

module ShoalPublicAction where

import Control.Monad.Freer (Eff)
import Prelude
import Tidepool.Actors.Shoal

lifted :: Eff ActorEffects Int -> AgentAction ActorEffects Int
lifted = liftAction

result :: Int
result = 42
