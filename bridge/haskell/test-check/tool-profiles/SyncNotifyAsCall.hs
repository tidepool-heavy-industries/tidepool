{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module SyncNotifyAsCall where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

newtype Tools mode = Tools { probe :: mode :- Sync (Call Text ()) } deriving Generic

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (presentWith (const "done") (syncNotify "Unit endpoint" (const (pure ()))))
