module IntrospectionStandalone where

import Control.Monad.Freer (Eff, Member)

data Allowed result = Allowed

allowed :: Member Allowed effects => Eff effects ()
allowed = allowed

pureValue :: Int
pureValue = 42
