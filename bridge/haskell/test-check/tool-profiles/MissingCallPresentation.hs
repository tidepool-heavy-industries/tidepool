{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module MissingCallPresentation where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

data Tools mode = Tools { probe :: mode :- Call Text Text } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (tool "Echo" pure)
