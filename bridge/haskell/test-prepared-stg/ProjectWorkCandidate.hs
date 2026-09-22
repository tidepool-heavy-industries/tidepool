module ProjectWorkCandidate where

import Project.Types (Delivery (..))
import Project.Work qualified as Work

-- Preserve both the production constructor distinction and its payload in a
-- shape supported by the prepared-corpus comparator.
candidate :: Either Int Int
candidate = case Work.candidate of
  Preparation value -> Left value
  Complete value -> Right value
