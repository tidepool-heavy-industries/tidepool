{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings #-}
module ConcreteNativeProfile where
import Control.Monad.Freer (Eff)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Commands)

-- A concrete notebook descriptor cannot borrow a different actor's base row.
data Tools mode = Tools
  { disguised :: HaskellTool 'BeforeNextInference '[Commands] '[Commands]
  } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (haskellTool "Invalid concrete notebook profile")

invalid = compileInstalledTools tools
