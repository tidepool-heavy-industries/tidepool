{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module UnitEndpoints where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

data Tools mode = Tools
  { unitCall :: mode :- Call Text ()
  , unitNotify :: mode :- Notify Text
  , unitSyncCall :: mode :- Sync (Call Text ())
  , unitSyncNotify :: mode :- Sync (Notify Text)
  } deriving Generic

tools :: Tools (AsServerT (Eff '[]))
tools = Tools
  { unitCall = presentWith (const "call") (tool "Call" (const (pure ())))
  , unitNotify = presentWith (const "notify") (notify "Notify" (const (pure ())))
  , unitSyncCall = presentWith (const "sync call") (syncTool "Sync Call" (const (pure ())))
  , unitSyncNotify = presentWith (const "sync notify") (syncNotify "Sync Notify" (const (pure ())))
  }

compiled = compileInstalledTools tools
