{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

module ResearchCanUnfold where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Actors.Exomonad

result
  :: ForkGroupPath
  -> Label
  -> Label
  -> Eff ResearchEffects (Response Text, Response Text)
result group coordinator leaf = unfoldDeferred group $
  (,) <$> child (withLifetime ActorOwned (researching @Text currentCheckout (assignment coordinator ())))
      <*> child (withLifetime ActorOwned (researchingLeaf @Text currentCheckout (assignment leaf ())))
