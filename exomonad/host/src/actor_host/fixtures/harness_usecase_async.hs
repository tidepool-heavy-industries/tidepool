{-# LANGUAGE FlexibleContexts #-}

import Control.Monad.Freer (Eff, Member)
import Tidepool.Duration (milliseconds)
import Tidepool.Effects.Core (Sleep, sleep)

let perfDelayed :: Member Sleep effects => Eff effects Int
    perfDelayed = do
      sleep (milliseconds 100)
      value <- perfAction
      pure (value + 36)
value <- perfDelayed
_ <- display value
