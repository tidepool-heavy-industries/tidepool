-- P3 oracle fixture 2: a transitive package import competes with a direct
-- sibling target supplying the same defining module under a different body.
--
-- `viaText` reaches Data.Text.Internal (and its transitive closure) through
-- Data.Text; `directText` reaches Data.Text.Internal directly. The two
-- projections of the shared defining module must stay distinct, and the
-- transitive choice made inside Data.Text must not collapse onto the direct
-- target's projection.
module TransitiveCompetitor where

import qualified Data.Text as T
import Data.Text.Internal (Text(..))

{-# NOINLINE viaText #-}
viaText :: String -> Int
viaText = T.length . T.pack

{-# NOINLINE directText #-}
directText :: Text -> Int
directText = T.length
