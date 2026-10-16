{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

module StartupSpec (agentSpec, emptySpec, absentSpec) where

import Control.Monad.Freer (Eff, Members)
import GHC.Generics (Generic)
import Jev.Host (jevTransport)
import Tidepool.Aeson.Value (Value (Bool), object, (.=))
import Tidepool.Agent.Contract hiding (haskell, haskellSync)
import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (FsRead, FsWrite, Jev)
import Tidepool.Records (WriteOutcome (..))
import qualified Tidepool.Effects as E

data StartupTools effects mode = StartupTools
  { haskell :: mode :- HaskellCell effects
  , haskellSync :: mode :- Sync (HaskellCell (SyncEffects effects))
  } deriving Generic

startupWorkbenchSpec
  :: forall effects.
     ( KnownToolEffects effects, AsyncEffects effects
     , Subset effects (SyncEffects effects)
     )
  => AgentSpec (StartupTools effects) effects
startupWorkbenchSpec = defaultSpec
  { specTools = StartupTools
      { haskell = A.haskell (A.haskellTools @effects)
      , haskellSync = haskellTool "Refresh this actor's context before its next inference."
      }
  }

agentSpec
  :: forall effects.
     ( KnownToolEffects effects, AsyncEffects effects
     , Subset effects (SyncEffects effects)
     , Members '[FsRead, FsWrite, Jev] effects
     )
  => AgentSpec (StartupTools effects) effects
agentSpec = (startupWorkbenchSpec @effects)
  { initialContext = Just prepare }

prepare
  :: Members '[FsRead, FsWrite, Jev] effects
  => StartupInput -> Eff effects ContextSeed
prepare input = do
  created <- E.writeCheckedIf Nothing ".exomonad/state/startup-witness.txt" (startupOperation input)
  case created of
    Right (Written _ _) -> pure ()
    Right (WriteConflict _ _ _) -> pure ()
    Left issue -> error (show issue)
    Right other -> error (show other)
  memory <- E.readFile ".exomonad/state/startup-memory.txt"
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
     (KnownToolEffects effects, AsyncEffects effects, Subset effects (SyncEffects effects))
  => AgentSpec (StartupTools effects) effects
emptySpec = (startupWorkbenchSpec @effects)
  { initialContext = Just (const (pure emptyContext)) }

absentSpec
  :: forall effects.
     (KnownToolEffects effects, AsyncEffects effects, Subset effects (SyncEffects effects))
  => AgentSpec (StartupTools effects) effects
absentSpec = startupWorkbenchSpec @effects
