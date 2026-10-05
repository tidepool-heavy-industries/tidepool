{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module SelectedEffectsProbe where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Tidepool.Effects
import qualified Tidepool.Effects.Core as Core

readContext :: Member ContextReadWrite effects => Eff effects ()
readContext = Core.getContext >> pure ()

-- Authors can still name an explicit row with an ordinary lexical alias.
type M = Eff '[ContextReadWrite]
explicitAlias :: M ()
explicitAlias = readContext
