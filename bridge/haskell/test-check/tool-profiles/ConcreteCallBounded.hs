{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module ConcreteCallBounded where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

-- A concrete field must not bypass the hosted presentation boundary.
data Tools mode = Tools { probe :: Tool (Eff '[]) Text Text } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (tool "Echo" pure)

invalid = compileTools tools
