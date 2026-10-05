{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract hiding (haskell)
import qualified Tidepool.Agent.Contract as A

newtype Probe = Probe { number :: Int }
  deriving (Generic, FromJSON, JsonSchema)

data BrowserTools effects mode = BrowserTools
  { haskell :: mode :- HaskellCell effects
  , probe :: mode :- Call Probe Int
  }
  deriving (Generic)

agentSpec
  :: forall effects.
     (KnownToolEffects effects, AsyncEffects effects)
  => AgentSpec (BrowserTools effects) effects
agentSpec = defaultSpec
  { specTools = BrowserTools
      { haskell = A.haskell (A.haskellTools @effects)
      , probe = presentWith presentJson $ tool "Add two to the supplied number." answer
      }
  }

answer :: Probe -> Eff effects Int
answer (Probe value) = pure (value + 2)
