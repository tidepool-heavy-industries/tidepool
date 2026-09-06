{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

module ResearchCanUnfold where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Actors.Shoal

result
  :: ForkGroupPath
  -> BranchLabel
  -> BranchLabel
  -> Eff ResearchEffects (Forked Text, Forked Text)
result group coordinator leaf = unfold group $
  (,) <$> child (researching @Text coordinator boundHead ())
      <*> child (researchingLeaf @Text leaf boundHead ())
