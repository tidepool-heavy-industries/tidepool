{-# LANGUAGE DataKinds #-}

module CodingCanUnfold where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Shoal

result :: ForkGroupPath -> Eff CodingActorEffects ()
result group = unfold group (pure ())
