{-# LANGUAGE DataKinds #-}

module CodingCanUnfold where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Shoal

result :: ForkGroupPath -> Eff CodingEffects ()
result group = unfold group (pure ())
