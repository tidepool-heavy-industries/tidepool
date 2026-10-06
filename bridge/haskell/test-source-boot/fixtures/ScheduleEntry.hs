module ScheduleEntry where

import qualified CacheEntry
import ScheduleLeft
import ScheduleRight
import ScheduleIndependent ()

result :: (Int, Bool)
result = (left 12 + right 12, CacheEntry.result)
