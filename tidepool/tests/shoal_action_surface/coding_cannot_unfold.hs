{-# LANGUAGE DataKinds #-}

module CodingCannotUnfold where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Shoal

result :: ForkGroupPath -> Eff CodingActorEffects ()
result group = unfold group (pure ())
