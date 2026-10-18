{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE TypeApplications #-}
module ModelFixture where

import Prelude
import Control.Monad (replicateM)
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson
import Tidepool.Agent.Contract
import Tidepool.Model
import qualified Exomonad.Models as Models
import Tidepool.Effects (Sleep (..), Console (..))
import Tidepool.Effects.Core (ActorEffectKey)
import Tidepool.Effects.Row (knownEffects, effectKeys)
import Tidepool.Duration (milliseconds)

-- Object arguments are required by the admitted bounded invocation profile.
data EchoArgs = EchoArgs { message :: Text }
  deriving (Generic, FromJSON, ToJSON, JsonSchema)
data EchoTools mode = EchoTools { echo :: mode :- Call EchoArgs Text }
  deriving Generic

callbackTurn :: (Member Sleep effects, Member Console effects)
             => ModelTurn Models.PrimaryModelFamily EchoTools effects Text
callbackTurn = textTurn Models.primaryModel (AgentSpec
  { specTools = EchoTools (presentWith id $ tool "Echo with the caller's effects" (\args -> do
      send (SleepWith (milliseconds 1))
      send (Print "model callback")
      pure (message args)))
  , afterTool = Just (\_ _ -> do
      send (SleepWith (milliseconds 1))
      send (Print "model hook")
      pure (Annotated "checked"))
  }) "callback"

callbackCell :: (Member (ModelCall Models.PrimaryModelFamily) effects, AsyncEffects effects, Member Sleep effects, Member Console effects)
             => Eff effects (Either ModelFailure Text, Maybe ModelUsage)
callbackCell = do
  result <- invokeModel callbackTurn "hello"
  pure (modelOutcome result, fmap invocationUsage (modelReceipt result))

nestedTurn :: (Member (ModelCall Models.PrimaryModelFamily) effects, AsyncEffects effects) => ModelTurn Models.PrimaryModelFamily EchoTools effects Text
nestedTurn = textTurn Models.primaryModel (AgentSpec
  { specTools = EchoTools (presentWith id $ tool "Invoke a nested model in this cell" (\_ -> do
      result <- invokeModel (textTurn Models.primaryModel defaultSpec "nested") "inner"
      pure (either (const "nested failed") id (modelOutcome result))))
  , afterTool = Nothing
  }) "nested-outer"

nestedCell :: (Member (ModelCall Models.PrimaryModelFamily) effects, AsyncEffects effects) => Eff effects (Either ModelFailure Text)
nestedCell = modelOutcome <$> invokeModel nestedTurn "outer"

nestedBudgetCell :: (Member (ModelCall Models.PrimaryModelFamily) effects, AsyncEffects effects) => Eff effects (Either ModelFailure Text)
nestedBudgetCell = modelOutcome <$> invokeModel
  (withLimits (defaultLimits { requestLimit = Just 1 }) nestedTurn) "outer"

budgetTurn :: ModelTurn Models.PrimaryModelFamily NoTools effects Text
budgetTurn = withLimits (defaultLimits { requestLimit = Just 1 })
  (textTurn Models.primaryModel defaultSpec "budget")

budgetCell :: (Member (ModelCall Models.PrimaryModelFamily) effects, AsyncEffects effects)
           => Eff effects (Int, Either ModelFailure Text)
budgetCell = do
  results <- replicateM 17 (invokeModel budgetTurn "one")
  let successes = length [() | result <- results, Right _ <- [modelOutcome result]]
  pure (successes, modelOutcome (last results))

parkedCell :: (Member (ModelCall Models.PrimaryModelFamily) effects, AsyncEffects effects) => Eff effects (Either ModelFailure Text)
parkedCell = modelOutcome <$> invokeModel (textTurn Models.primaryModel defaultSpec "parked") "park"

modelCapabilityKeys :: [ActorEffectKey]
modelCapabilityKeys = effectKeys (knownEffects @'[ModelCall Models.PrimaryModelFamily])
