{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module MissingSyncNotifyPresentation where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

data Tools mode = Tools { probe :: mode :- Sync (Notify Text) } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (syncNotify "Notice" (const (pure ())))
