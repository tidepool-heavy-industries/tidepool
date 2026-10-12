{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE NoMonomorphismRestriction #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Members)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (FsRead, FsWrite, Jev)
import qualified Persona.Attention as Attention
import qualified Persona.Context as Context
import qualified Persona.Spec as Persona

agentSpec = withStartup Persona.agentSpec

withStartup
  :: Members '[FsRead, FsWrite, Jev] effects
  => AgentSpec tools effects -> AgentSpec tools effects
withStartup spec = spec { initialContext = Just prepare }

prepare
  :: Members '[FsRead, FsWrite, Jev] effects
  => StartupInput -> Eff effects ContextSeed
prepare _ = do
  Right (_, prepared) <- Context.prepare (Attention.Limits 1 16384)
    "startup" Attention.continuity
  _ <- if all (\outcome -> case outcome of
      Attention.Selected _ presentation -> Attention.presentationId presentation == "original"
      _ -> False) (Attention.outcomes (Attention.inspect prepared))
    then pure () else error "startup did not decode and select the controlled Jev answers"
  Right () <- Context.save "startup-preparation" prepared
  pure (contextSection "persona-attention" (Just "notes/startup-preparation.md")
    (Attention.rendered prepared))
