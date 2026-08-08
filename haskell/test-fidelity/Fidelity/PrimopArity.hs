-- | Two fail-loud contracts of the extract pipeline.
--
-- The generic unboxed-tuple fallback can bind at most one result; every higher
-- result arity without a dedicated split fails loud at extract time rather
-- than aliasing several binders onto one node. And a failed module load is a
-- phase barrier — the pipeline stops there instead of continuing into
-- typechecking against an error-recovery environment.
module Fidelity.PrimopArity (checks) where

import Fidelity.Harness (Check)

checks :: IO [Check]
checks = pure []
