{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
module SelectedEffectsProbe where

import Tidepool.Effects
import qualified Tidepool.Effects as Effects

result :: Effects.M ()
result = getContext >> pure ()
