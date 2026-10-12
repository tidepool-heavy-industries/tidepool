{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | A notebook-sized assessment: inspect a plan, observe once, compare policies,
-- then edit the reflex. See docs/composition.md for publication semantics.
module Examples.JevComposedWorkflow where

import Control.Lens (Lens', lens, over, traverseOf, traversed, _2, view)
import Control.Selective (ifS)
import Data.Profunctor (Star (..), lmap)
import Data.Text (Text)
import qualified Jev.Core as C
import qualified Jev.Operators as J
import Tidepool.Aeson.Value (Value)

data Report = Report { claim :: Text, evidence :: Text }
data Helper = Helper { report :: Report }

helperReport :: Lens' Helper Report
helperReport = lens report (\h r -> h { report = r })

type ReportState = "claim" J.::= Text J.:& "evidence" J.::= Text

reportState :: Report -> J.State ReportState
reportState r = J.state (#claim J.:= claim r J.:& #evidence J.:= evidence r)

assessReport :: Star (C.Query Value) Report (Either J.Doubt (J.Settled J.Careful Bool))
assessReport = J.assessment reportState $ \world ->
  J.judge J.careful <$> J.question world #supported
    (J.noul ("Does " <> J.field #evidence world <> " support " <> J.field #claim world <> "?"))

reviewPending :: [(Text, Helper)] -> J.Query [(Text, Either J.Doubt (J.Settled J.Careful Bool))]
reviewPending = traverseOf (traversed . _2)
  (runStar (lmap (view helperReport) assessReport))

-- If evidence is incomplete, observe which expansion would help. Speculation
-- can ask both questions in one call without executing the carried IO actions.
inspectOrContinue :: J.State ReportState -> [(Text, Text, IO ())] -> J.Program (IO ())
inspectOrContinue world sources = ifS
  (J.liftQuery (J.holds J.careful <$> J.question world #complete (J.noul "Does the evidence suffice?")))
  (pure (pure ()))
  (J.liftQuery (J.withUniform repertoire $ \offers ->
    J.taken <$> J.question world #expand (J.choice "Which source would resolve what is missing?" offers)))
  where
    repertoire = J.uniform (J.alt #pause "No source resolves the gap" (pure ()) J..|
      over (J.payloads #source) (\(_,_,action) -> action)
        (J.many #source (\(k,_,_) -> k) (\(_,w,_) -> w) sources))
    -- Edit only one payload family: wire keys and source descriptions stay put.
    -- A separate human form can use the original sources and the same actions.


data Hypothesis = MissingEvidence | Contradiction deriving (Eq, Show)
data Next = ReadMore | Recheck deriving (Eq, Show)

-- One observation supports expected-loss choice and several hypotheses to inspect.
-- The caller defines the losses; the library does not select a universal policy.
comparePolicies :: J.Weights Hypothesis
  -> Either J.DistributionError (Next, [(Double, Hypothesis)])
comparePolicies raw = do
  distribution <- J.normalize raw
  explore <- J.covering 0.9 distribution
  let loss ReadMore MissingEvidence = 1
      loss ReadMore Contradiction = 8
      loss Recheck MissingEvidence = 4
      loss Recheck Contradiction = 1
      chosen = if J.expected (loss ReadMore) distribution <= J.expected (loss Recheck) distribution
        then ReadMore else Recheck
  pure (chosen, explore)
