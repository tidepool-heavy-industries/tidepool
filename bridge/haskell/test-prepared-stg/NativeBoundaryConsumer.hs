module NativeBoundaryConsumer (consumer) where

import NativeBoundaryProducer
import NativeBoundaryInstances ()
import NativeBoundarySource (sourceId, sourceRule)

consumer :: Int -> Int
consumer value = alias + inlineAlias value + poly value
  + fst (strictPair (value, value)) + single True + first [True]
  + unBox (Box value) + unWrap (Wrap value)
  + sourceId value + sourceRule value
