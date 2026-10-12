{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

module AgentSpec (agentSpec, emptySpec, absentSpec) where

import Control.Monad.Freer (Eff, Members)
import Jev.Host (jevTransport)
import Tidepool.Aeson.Value (Value (Bool), object, (.=))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (FsRead, FsWrite, Jev, WriteOutcome (..))
import qualified Tidepool.Effects as E

agentSpec
  :: forall effects.
     ( KnownToolEffects effects, AsyncEffects effects
     , Members '[FsRead, FsWrite, Jev] effects
     )
  => AgentSpec (HaskellTools effects) effects
agentSpec = (defaultWorkbenchSpec @effects)
  { initialContext = Just prepare }

prepare
  :: Members '[FsRead, FsWrite, Jev] effects
  => StartupInput -> Eff effects ContextSeed
prepare input = do
  created <- E.writeCheckedIf Nothing "startup-witness.txt" (startupOperation input)
  case created of
    Right (Written _ _) -> pure ()
    Right (WriteConflict _ _ _) -> pure ()
    Left issue -> error (show issue)
    Right other -> error (show other)
  memory <- E.readFile "startup-memory.txt"
  body <- case memory of
    Right text -> pure text
    Left issue -> error (show issue)
  choice <- jevTransport (object
    [ "memory" .= body
    , "caller_seed_present" .= maybe False (const True) (startupCallerSeed input)
    , "caller_seed" .= startupCallerSeed input
    , "operation" .= startupOperation input
    ])
  case choice of
    Right (Bool True) -> pure (ContextSeed
      [ ContextSection "memory" (Just "startup-memory.txt") body
      , ContextSection "selection" (Just "controlled-jev") "included"
      ])
    Right (Bool False) -> pure emptyContext
    Right unexpected -> error (show unexpected)
    Left issue -> error (show issue)

emptySpec
  :: forall effects.
     (KnownToolEffects effects, AsyncEffects effects)
  => AgentSpec (HaskellTools effects) effects
emptySpec = (defaultWorkbenchSpec @effects)
  { initialContext = Just (const (pure emptyContext)) }

absentSpec
  :: forall effects.
     (KnownToolEffects effects, AsyncEffects effects)
  => AgentSpec (HaskellTools effects) effects
absentSpec = defaultWorkbenchSpec @effects
