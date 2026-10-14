{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module CallAsNotify where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

newtype Tools mode = Tools { probe :: mode :- Notify Text } deriving Generic

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (presentWith (const "done") (tool "Unit endpoint" (const (pure ()))))
