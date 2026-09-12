{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

module ResearchCanUnfold where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Actors.Shoal

result
  :: ForkGroupPath
  -> Label
  -> Label
  -> Eff ResearchEffects (Response Text, Response Text)
result group coordinator leaf = unfold group $
  (,) <$> child (researching @Text boundHead (assignment coordinator ()))
      <*> child (researchingLeaf @Text boundHead (assignment leaf ()))
