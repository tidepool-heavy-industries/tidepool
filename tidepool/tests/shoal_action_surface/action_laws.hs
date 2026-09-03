{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module ShoalActionLaws where

import Control.Monad.Freer (run)
import Prelude
import Tidepool.Agent.Action

liftedResult :: Either ActionFailure Int
liftedResult = run $ runAgentAction $ do
  value <- liftAction (pure 21)
  pure (value * 2)

failedAction :: AgentAction '[] Int
failedAction = AgentAction (pure (Left (AwaitedActorFailed "stop")))

shortCircuitedResult :: Either ActionFailure Int
shortCircuitedResult = run $ runAgentAction $ do
  _ <- failedAction
  liftAction $ error "monadic failure did not short-circuit"

applicativeShortCircuitedResult :: Either ActionFailure Int
applicativeShortCircuitedResult = run $ runAgentAction $
  (+) <$> failedAction <*> liftAction (pure (error "applicative failure did not short-circuit"))

result :: Int
result = case (liftedResult, shortCircuitedResult, applicativeShortCircuitedResult) of
  ( Right value
    , Left (AwaitedActorFailed "stop")
    , Left (AwaitedActorFailed "stop")
    ) -> value
  _ -> error "unexpected AgentAction result"
