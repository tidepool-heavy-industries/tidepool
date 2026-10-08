{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module MissingSyncPresentation where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

data Tools mode = Tools { probe :: mode :- Sync (Call Text Text) } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (syncTool "Echo" pure)
