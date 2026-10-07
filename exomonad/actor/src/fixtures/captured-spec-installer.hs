{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module CapturedSpecInstaller (installer) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (AgentTools)

newtype Tools mode = Tools { probe :: mode :- RawCall Text }
  deriving Generic

installer :: Text -> Int -> Eff (AgentTools ': SyncEffects '[]) ()
installer captured _ = installSpec @'[] defaultSpec
  { specTools = Tools
      { probe = presentWith id $ rawTool "captured notebook value" $ \_ -> pure captured
      }
  }
