{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Pure construction checks in the recipe driver. The two live cases are
-- 'Project.EvidencePatternExamples.runCommandCase' and 'runReviewCase'; a
-- resident cell with host Jev can run those and inspect the raw responses.
module Project.EvidencePatternChecks (construction) where

import Control.Monad.Freer (Eff, Member)
import Data.Either (isLeft, isRight)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=)))
import Project.EvidencePattern
import Project.EvidencePatternExamples
import Tidepool.Aeson.Value (object, (.=))
import Tidepool.Check (RecipeCheck, check)

construction :: Member RecipeCheck effects => Eff effects ()
construction = do
  let commandOffers = J.offered (evidenceOffers commandCriteria describeEvidence commandEvidence)
      reviewOffers = J.offered (evidenceOffers reviewCriteria describeEvidence reviewEvidence)
      keys = map fst commandOffers
      wording = map snd commandOffers
  check "the bounded set includes explicit none and insufficient choices"
    (take 2 keys == ["none", "insufficient"]
      && length commandOffers == 5 && length reviewOffers == 5)
  check "the caller's exact provenance and excerpt reach the candidate wording"
    ("eb73928 Project/JevChecks.hs lines 37-40" `Text.isInfixOf` (wording !! 2)
      && evidenceExcerpt (head commandEvidence) `Text.isInfixOf` (wording !! 2)
      && "eb73928 Project/RetainedEvidence.hs lines 19-22" `Text.isInfixOf` snd (reviewOffers !! 2))
  let sharedOffers = J.offered (evidenceOffers commandCriteria
        (\row -> "Entry " <> evidenceKey row <> " in `evidence`") commandEvidence)
  check "shared-state wording refers to entries without repeating excerpts"
    (all (not . Text.isInfixOf (evidenceExcerpt (head commandEvidence)) . snd) sharedOffers)
  check "two clients override criteria while retaining the same shape"
    (selectionQuestion commandCriteria /= selectionQuestion reviewCriteria
      && usefulWhen commandCriteria /= usefulWhen reviewCriteria)
  let commandRequest = J.request J.jevLatest
        commandState commandPacket
  check "shared useful criterion appears once in the packet, not once per candidate"
    (case commandRequest of
      Left _ -> False
      Right request ->
        Text.count (usefulWhen commandCriteria) (Text.pack (show request)) == 1
          && all (not . Text.isInfixOf (usefulWhen commandCriteria)) wording
          && Text.count (Text.takeWhile (/= '\n') (evidenceExcerpt (head commandEvidence)))
               (Text.pack (show request)) == 1)
  check "both authored packets prepare as a single request"
    (isRight commandRequest
      && isRight (J.request J.jevLatest reviewState reviewPacket))
  let duplicate = [head commandEvidence, head commandEvidence]
  check "the existing Jev request validator rejects duplicate candidate keys"
    (isLeft (J.request J.jevLatest
      (J.state (#intent := ("duplicate check" :: Text.Text)))
      (#best := evidenceQuestion commandCriteria (const "") duplicate)))
  let candidate = decodeSelection "E0061-site"
      none = decodeSelection "none"
      insufficient = decodeSelection "insufficient"
  check "a decoded choice returns the original typed payload with its provenance"
    (candidate == Right (Selected (head commandEvidence)))
  check "none and insufficient remain distinct settled semantic answers"
    (none == Right NoUsefulEvidence && insufficient == Right InsufficientEvidence)

decodeSelection :: Text.Text -> Either Text.Text (EvidenceSelection Diagnostic)
decodeSelection selected = do
  response <- either (Left . Text.pack . show) Right $ J.decode
    (#best := evidenceQuestion commandCriteria (const "") commandEvidence)
    (object
      [ "model" .= ("scripted" :: Text.Text)
      , "answers" .= object
          [ "best" .= object
              [ "type" .= ("choice" :: Text.Text)
              , "choice" .= selected
              , "confidence" .= (0.95 :: Double)
              , "probabilities" .= object
                  [ key .= (if key == selected then 0.8 else 0.05 :: Double)
                  | key <- ["none", "insufficient", "E0061-site", "E0061-definition", "E0004-site"]
                  ]
              ]
          ]
      ])
  case settleEvidence J.lenient response.best of
    Left doubt -> Left doubt.why
    Right (J.Settled selection) -> Right selection
