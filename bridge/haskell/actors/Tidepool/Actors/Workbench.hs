{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | Build-owned hosted policies for the standard ordered actor effects.
module Tidepool.Actors.Workbench
  ( BuiltinDispatcherEffects
  , installDefaultWorkbench
  , installDefaultAsyncWorkbench
  ) where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract
  ( SyncEffects, defaultWorkbenchSpec, defaultAsyncWorkbenchSpec, installSpec )
import Tidepool.Effects.Core (AgentTools)
import Tidepool.Internal.ActorProfiles (ActorEffects)
import Tidepool.Inspection ()

-- | Build-owned hosted installers for the standard ordered actor row. Their
-- dispatchers are immutable code; every installation still creates actor-local
-- policy state through 'AgentToolsInstallWith'.
type BuiltinDispatcherEffects = AgentTools ': SyncEffects ActorEffects

installDefaultWorkbench :: Eff BuiltinDispatcherEffects ()
installDefaultWorkbench = installSpec @ActorEffects defaultWorkbenchSpec

installDefaultAsyncWorkbench :: Eff BuiltinDispatcherEffects ()
installDefaultAsyncWorkbench = installSpec @ActorEffects defaultAsyncWorkbenchSpec
