{-# LANGUAGE DataKinds #-}

module CodingCanUnfold where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

result :: ForkGroupPath -> Eff CodingEffects ()
result group = unfold group (pure ())
