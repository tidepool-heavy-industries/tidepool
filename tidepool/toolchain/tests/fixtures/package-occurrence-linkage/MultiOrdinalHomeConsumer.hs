-- P3 oracle fixture 5 consumer: exercises both original ordinals of
-- MultiOrdinalHome so each group's package roots must stay with its group.
module MultiOrdinalHomeConsumer where

import MultiOrdinalHome
import Data.Text (Text)

result :: ([Int] -> [Int], String -> Text)
result = (byList, byText)
