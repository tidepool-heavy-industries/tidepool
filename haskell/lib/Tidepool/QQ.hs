-- | Quasi-quoters for the tidepool eval dialect.
--
-- @[fmt|...|]@ — Text interpolation with @{antiquote}@ holes.
-- @[j|...|]@   — JSON 'Tidepool.Aeson.Value.Value' literals (expression
--                position) and shape-matching (pattern position).
--
-- Both quoters do all parsing at COMPILE time (inside the splice
-- evaluator) and expand to plain Core over 'Data.Text.Text' and the
-- vendored 'Tidepool.Aeson.Value.Value' — no runtime parsing, no
-- Generic/Typeable machinery, nothing the Cranelift JIT doesn't already
-- run. See @plans/qq-spike.md@ for the architecture decision.
module Tidepool.QQ
  ( fmt
  , j
  ) where

import Tidepool.QQ.Fmt (fmt)
import Tidepool.QQ.Json (j)
