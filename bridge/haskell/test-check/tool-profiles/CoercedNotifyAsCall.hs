{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module CoercedNotifyAsCall where

import Control.Monad.Freer (Eff)
import Data.Coerce (coerce)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

newtype Tools mode = Tools { probe :: mode :- Call Text () } deriving Generic

notice :: Tool 'NotifyKind (Eff '[]) Text ()
notice = notify "Notice" (const (pure ()))

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (presentWith (const "done") (coerce notice))
