-- P3 oracle fixture 5 consumer: exercises both original ordinals of
-- MultiOrdinalHome so each group's package roots must stay with its group.
module MultiOrdinalHomeConsumer where

import MultiOrdinalHome
import Data.Text (Text)

result :: ([Int] -> [Int], String -> Text)
result = (byList, byText)

-- A second projection consumes the same two fresh HOME issuers. Completion of
-- the first target is carried into this one, so exact prior bindings must be
-- reused while the fresh per-ordinal evidence is still fully checked.
result2 :: ([Int] -> [Int], String -> Text)
result2 = (byList, byText)
