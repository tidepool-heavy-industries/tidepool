{-# OPTIONS_GHC -Werror=missing-fields #-}
module RejectMissingHandler where

import Sketch

-- Record construction otherwise permits missing fields as bottoms in Haskell.
-- Exhaustiveness here requires missing-fields to be an error, not just a warning.
bad :: Steps (Handlers String)
bad = Steps { follow = \(Edge edge) -> show edge }
