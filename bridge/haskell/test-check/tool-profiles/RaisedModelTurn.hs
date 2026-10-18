{-# LANGUAGE DataKinds, OverloadedStrings #-}
module RaisedModelTurn where
import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ModelCall)
import qualified Tidepool.Model as Model

selectedModel :: Model.Model Model.OpenAI
selectedModel = Model.openAI "test-catalog" "primary" (Model.OpenAIOptions Model.OpenAILow)

retainedTurn :: Eff '[ModelCall Model.OpenAI] (Model.ModelResult Text)
retainedTurn = Model.invokeModel (Model.textTurn selectedModel defaultSpec "instructions") "input"

-- A synchronous cell can reuse an ordinary compiled model program without
-- lending its context capability to that program's callbacks or hooks.
synchronousTurn :: Eff (SyncEffects '[ModelCall Model.OpenAI]) (Model.ModelResult Text)
synchronousTurn = raiseSync retainedTurn
