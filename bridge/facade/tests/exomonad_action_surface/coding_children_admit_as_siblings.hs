{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

module CodingChildrenAdmitAsSiblings where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Actors.Exomonad

-- | Two children admitted in the same 'unfold' each carry a roster of the
-- other branches admitted alongside them (label, allocated path, and a
-- truncated preview of the sibling's own assignment input), computed with
-- the same 'WorkbenchDisplay' the engine renders a child's own
-- `sessionInput` preview with.
result
  :: ForkGroupPath
  -> Label
  -> Label
  -> Eff CodingEffects (Response Text, Response Text)
result group workerA workerB = unfold group $
  (,) <$> child (coding @Text currentCheckout (assignment workerA ()))
      <*> child (scaffolding @Text currentCheckout (assignment workerB ()))
