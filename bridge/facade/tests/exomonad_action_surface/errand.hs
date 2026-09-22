{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module ExomonadErrand where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Prelude
import Tidepool.Actors.Exomonad

-- One call takes the task. No record to define, no client to construct, no
-- state to query, no retirement to write.
askText :: Eff ActorEffects (Watch (Settlement Text))
askText = errand "repo-layout" "which crate owns the compile cache?"

-- One call returns the reply: poll the watch, read the settlement.
reply :: WatchState (Settlement Text) -> Maybe (Either ResponseFailure Text)
reply state = case state of
  WatchReady settled -> Just (settledValue settled)
  _ -> Nothing

-- The errand is the only thing the caller writes; a coding child still takes
-- the ordinary typed path, which the errand does not replace.
typedStillWorks :: ForkGroupPath -> Label -> Eff ActorEffects (Response Int)
typedStillWorks group leaf =
  unfold group (child (coding @Int projectHead (assignment leaf ())))
