{-# LANGUAGE DataKinds #-}

module ResearchLeafCannotUnfold where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

result :: ForkGroupPath -> Eff ResearchLeafEffects ()
result group = unfold group (pure ())
