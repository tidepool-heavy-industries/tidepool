-- P3 ordinary shared-symbol fixture. `viaText` reaches Data.Text through
-- Data.Text.Internal while `directText` reaches the internal module directly.
-- This fixture establishes order invariance only; it does not establish two
-- competing transitive candidates.
module TransitiveCompetitor where

import qualified Data.Text as T
import Data.Text.Internal (Text(..))

{-# NOINLINE viaText #-}
viaText :: String -> Int
viaText = T.length . T.pack

{-# NOINLINE directText #-}
directText :: Text -> Int
directText = T.length
