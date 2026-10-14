{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module SyncCallAsNotify where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

newtype Tools mode = Tools { probe :: mode :- Sync (Notify Text) } deriving Generic

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (presentWith (const "done") (syncTool "Unit endpoint" (const (pure ()))))
