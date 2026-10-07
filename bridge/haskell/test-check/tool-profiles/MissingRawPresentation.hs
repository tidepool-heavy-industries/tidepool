{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module MissingRawPresentation where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

data Tools mode = Tools { probe :: mode :- RawCall Text } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (rawTool "Echo" pure)
