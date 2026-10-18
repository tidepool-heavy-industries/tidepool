{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, FlexibleContexts, TypeFamilies, TypeOperators #-}
module GenericPresentedModel where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

import Tidepool.Effects.Core (ModelCall)
import qualified Tidepool.Model as Model

selectedModel :: Model.Model Model.OpenAI
selectedModel = Model.openAI "test-catalog" "primary" (Model.OpenAIOptions Model.OpenAILow)

data Inner mode = Inner { probe :: mode :- Call Text Text } deriving (Generic)
data Outer mode = Outer { inner :: Inner mode } deriving (Generic)

rendered :: PresentableTool handler => (ToolOutput handler -> Text) -> handler -> Presented handler
rendered = presentWith

tools :: Outer (AsServerT (Eff '[ModelCall Model.OpenAI]))
tools = Outer (Inner (rendered id (tool "Echo" pure)))

installed = compileInstalledTools tools
bounded = Model.invokeModel (Model.textTurn selectedModel (defaultSpec { specTools = tools }) "instructions") "input"
