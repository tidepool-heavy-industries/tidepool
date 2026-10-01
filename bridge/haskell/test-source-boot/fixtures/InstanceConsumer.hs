module InstanceConsumer where

import InstanceRelay

result :: Int
result = available (42 :: Int)
