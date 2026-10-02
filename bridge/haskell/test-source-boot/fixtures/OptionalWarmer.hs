module OptionalWarmer where

import OptionalSupport
import OptionalAnchor

result :: Int
result = if optional then anchor else 0
