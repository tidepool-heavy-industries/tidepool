{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module ErrandNeedsLaunch where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Prelude
import Tidepool.Actors.Exomonad

-- An errand child is inspection-only and holds no launch authority, so it
-- cannot issue errands of its own. Neither can a research coordinator: only
-- the row that carries AgentLaunch may start a fresh agent.
result :: Eff ResearchLeafEffects (Watch (Settlement Text))
result = errand "nested" "ask something else"

fromResearch :: Eff ResearchEffects (Watch (Settlement Text))
fromResearch = errand "nested" "ask something else"
