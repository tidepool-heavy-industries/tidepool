{-# LANGUAGE DataKinds, OverloadedStrings #-}
module ModelContext where
import Control.Monad.Freer (Eff, send)
import Data.Text (Text)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ContextReadWrite (..), ModelCall, Model (CustomModel), Backend (OpenAIPlan))
import qualified Tidepool.Model as Model

selectedModel :: Model.Model Model.OpenAI
selectedModel = Model.openAI "test-catalog" "primary" (Model.OpenAIOptions Model.OpenAILow)

-- A nested model's hook must not borrow its caller's sync context authority.
spec :: AgentSpec NoTools '[ContextReadWrite, ModelCall Model.OpenAI]
spec = defaultSpec {afterTool = Just (\_ _ -> send (SetNextModelWith (CustomModel OpenAIPlan "executor")) >> pure NoAnnotation)}

invalid :: Eff '[ContextReadWrite, ModelCall Model.OpenAI] (Model.ModelResult Text)
invalid = Model.invokeModel (Model.textTurn selectedModel spec "instructions") "input"
