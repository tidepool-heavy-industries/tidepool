{-# LANGUAGE NoMonomorphismRestriction #-}
module AgentSpec (agentSpec) where

import Tidepool.Agent.Contract (AgentSpec (..))
import qualified Persona.Spec as Persona
import qualified Persona.ReentryProbe as Probe

agentSpec = let original = Persona.runtimeSpec in original
  { initialContext = Just $ \input -> do
      Probe.arrival
      case initialContext original of
        Just startup -> startup input
        Nothing -> error "Persona startup initializer missing"
  }
