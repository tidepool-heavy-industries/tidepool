{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.FieldNotesChecks (policy) where

import Control.Monad.Freer (Eff, Member)
import Tidepool.Check (RecipeCheck, check)
import Project.FieldNotes

policy :: Member RecipeCheck effects => Eff effects ()
policy = do
  let floorPolicy = AtLeast 0.6
      scorePolicy = AtLevel "strong"
      choicePolicy = WhenIn ["stuck", "waiting"]
  check "Noul floor excludes a below-floor answer and includes an answer at the floor"
    (not (noulTrips floorPolicy 0.59) && noulTrips floorPolicy 0.6)
  check "field-note cadence uses only positive runtime result ordinals"
    (shouldObserveOrdinal 3 3 && shouldObserveOrdinal 3 6
      && not (shouldObserveOrdinal 3 2) && not (shouldObserveOrdinal 3 0)
      && not (shouldObserveOrdinal 0 3))
  check "Score uses the four configured rubric levels and the selected level mass"
    ( scoreLevelNames == ["none", "slight", "clear", "strong"]
        && not (scorePolicyTrips scorePolicy 0.8 [("strong", 0.49)])
        && scorePolicyTrips scorePolicy 0.8 [("strong", 0.5)]
    )
  check "Choice policy gates only the selected closed alternative"
    ( choicePolicyTrips choicePolicy (Just "waiting") 0.2
        && not (choicePolicyTrips choicePolicy (Just "exploring") 0.9)
    )
