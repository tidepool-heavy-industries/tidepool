module ModuleProductB (consume) where

import ModuleProductA (Box(..), produce)

consume :: Box
consume = Box (produce + 1)
