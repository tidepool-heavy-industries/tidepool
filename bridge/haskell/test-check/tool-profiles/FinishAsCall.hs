{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module FinishAsCall where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract

newtype Tools mode = Tools { probe :: mode :- Call Text () } deriving Generic

tools :: Tools (AsActorT (Eff '[]) Text Text)
tools = Tools (finishTool "Finish cannot impersonate a call" (\input -> pure ((), input)))
