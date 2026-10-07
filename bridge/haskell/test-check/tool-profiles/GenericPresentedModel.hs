{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, FlexibleContexts, TypeFamilies, TypeOperators #-}
module GenericPresentedModel where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

import Tidepool.Effects.Core (ModelCall)
import qualified Tidepool.Model as Model

data Inner mode = Inner { probe :: mode :- Call Text Text } deriving (Generic)
data Outer mode = Outer { inner :: Inner mode } deriving (Generic)

rendered :: PresentableTool handler => (ToolOutput handler -> Text) -> handler -> Presented handler
rendered = presentWith

tools :: Outer (AsServerT (Eff '[ModelCall]))
tools = Outer (Inner (rendered id (tool "Echo" pure)))

installed = compileInstalledTools tools
bounded = Model.invokeModel (Model.textTurn (defaultSpec { specTools = tools }) "instructions") "input"
