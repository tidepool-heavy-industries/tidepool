{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
module SelectedEffectsProbe where

import Tidepool.Effects
import qualified Tidepool.Effects as Effects
import qualified Tidepool.Effects.Core as Core

result :: Effects.M ()
result = Core.getContext >> pure ()
