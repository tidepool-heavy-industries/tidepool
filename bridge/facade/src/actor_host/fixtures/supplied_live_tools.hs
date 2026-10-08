{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.Supplied (Probe (..), SuppliedTools (..), actualSpec) where

import Control.Monad.Freer (send)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev (JevAskWith))
import Tidepool.Agent.Reply (Replies)

data Probe = Probe { number :: Int, hold :: Bool }
  deriving (Generic, FromJSON, JsonSchema)

data SuppliedTools effects mode = SuppliedTools
  { distinctive :: mode :- Sync (Call Probe Int)
  , notebook :: HaskellTools effects mode
  } deriving Generic

actualSpec :: Int -> Text
           -> AgentSpec (SuppliedTools '[Replies, Jev]) '[Replies, Jev]
actualSpec offset description = defaultSpec
  { specTools = SuppliedTools
      { distinctive = presentWith presentJson $ syncTool description $ \(Probe value held) -> do
          if held then do
            _ <- send (JevAskWith "supplied-admitted-handler")
            pure ()
          else pure ()
          pure (value + offset)
      , notebook = haskellTools
      }
  }
