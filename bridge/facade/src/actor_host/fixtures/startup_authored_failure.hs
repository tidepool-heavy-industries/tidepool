{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeOperators #-}

module Tidepool.Actors.Internal.ExomonadDriver
  ( RootEffects
  , rootDriver
  , module Tidepool.Actors.Exomonad
  ) where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (AgentSession)

type RootEffects = AgentSession ': ActorEffects

rootDriver :: Eff RootEffects ()
rootDriver = error "startup authored failure sentinel"
