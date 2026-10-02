{-# LANGUAGE DataKinds, OverloadedStrings #-}
module RaisedModelTurn where
import Control.Monad.Freer (Eff, raise)
import Data.Text (Text)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ModelCall)
import qualified Tidepool.Model as Model

retainedTurn :: Eff '[ModelCall] (Model.ModelResult Text)
retainedTurn = Model.invokeModel (Model.textTurn defaultSpec "instructions") "input"

-- A synchronous cell can reuse an ordinary compiled model program without
-- lending its context capability to that program's callbacks or hooks.
synchronousTurn :: Eff (SyncEffects '[ModelCall]) (Model.ModelResult Text)
synchronousTurn = raise retainedTurn
