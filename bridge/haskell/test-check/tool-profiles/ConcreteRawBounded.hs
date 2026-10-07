{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module ConcreteRawBounded where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

-- A concrete field must not bypass the hosted presentation boundary.
data Tools mode = Tools { probe :: RawTool (Eff '[]) Text } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (rawTool "Echo" pure)

invalid = compileTools tools
