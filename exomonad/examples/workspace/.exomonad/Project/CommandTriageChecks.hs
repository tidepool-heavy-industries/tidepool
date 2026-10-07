{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.CommandTriageChecks (construction) where

import Control.Monad.Freer (Eff, Member)
import Tidepool.Check
import Data.Text (Text)
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))
import Tidepool.Aeson.Value (Value)
import Project.CommandTriagePattern
import Project.CommandTriageExamples ()

construction :: Member RecipeCheck effects => Eff effects ()
construction = do
  let criteria = testFailureCriteria
      evidence = J.state (#diagnostic := ("test failed" :: Text))
      questions = #fault := faultQuestion criteria
        :& #transient := J.optional (Nothing :: Maybe (J.Q Value J.Noul))
      sent = J.request <$> J.prepare J.jevLatest evidence questions
      single = J.request <$> J.prepare J.jevLatest evidence (#fault := faultQuestion criteria)
  check "disabled speculative question sends exactly the single-question request"
    (case (sent, single) of { (Right a, Right b) -> a == b; _ -> False })
