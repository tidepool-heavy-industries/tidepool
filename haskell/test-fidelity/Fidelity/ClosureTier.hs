-- | PRD 21 lane C1's 'GhcPipeline.isClosureType' walk missed a HIGHER-KINDED
-- instantiation: for @data Box f = Box (f Int)@ at @f = (->) Bool@, the
-- datacon's own field is declared as @f Int@ — a tyvar application, opaque
-- regardless of what @f@ is instantiated to, so the only place function-ness
-- can be caught is @Box@'s own OUTER type argument, @(->) Bool@ itself. That
-- argument is a genuinely PARTIAL application of the arrow TyCon — matched
-- neither by @GHC.Core.Type.splitFunTy_maybe@ (which only ever sees a
-- SATURATED arrow; GHC itself normalizes every saturated @(->)@ application
-- to its @FunTy@ sugar) nor by @GHC.Core.TyCon.tyConDataCons_maybe@ (the
-- arrow TyCon is primitive — it has no DataCons). Deep-forcing a value of
-- this shape at bind time (the Tier0 path) crashes trying to force through
-- the function-shaped field. Fixed by special-casing the arrow TyCon
-- directly in 'GhcPipeline.isClosureType''s internal @goTc@.
module Fidelity.ClosureTier (checks) where

import Fidelity.Harness (Check, check, extractResultTier)

checks :: IO [Check]
checks = sequence
  [ checkPartialArrowFieldIsTier1
  , checkOrdinaryFunctorFieldIsTier0
  ]

-- | @Box@ instantiated at a partially-applied arrow: the field's own
-- declared type (@f Int@) never resolves to a function regardless of @f@,
-- so this fixture pins that function-ness is caught from @Box@'s outer
-- instantiation argument instead.
partialArrowSrc :: String
partialArrowSrc = unlines
  [ "module ClosureTierPartialArrow (result) where"
  , ""
  , "data Box f = Box (f Int)"
  , ""
  , "result :: Box ((->) Bool)"
  , "result = Box (\\_ -> 5)"
  ]

checkPartialArrowFieldIsTier1 :: IO Check
checkPartialArrowFieldIsTier1 = do
  r <- extractResultTier "closuretier-partial-arrow" "ClosureTierPartialArrow" partialArrowSrc
  pure $ check
    "Box ((->) Bool) -- a functor field instantiated at a partially-applied \
    \arrow -- classifies Tier1 and survives a bind (extracts + translates cleanly)"
    (case r of
       Right tier -> tier
       Left _     -> False)

-- | The control: the SAME @Box@ shape instantiated at an ordinary
-- non-function functor stays Tier0 — pins that the arrow-TyCon special case
-- fires only for a genuine arrow, not for every higher-kinded field.
ordinaryFunctorSrc :: String
ordinaryFunctorSrc = unlines
  [ "module ClosureTierOrdinaryFunctor (result) where"
  , ""
  , "data Box f = Box (f Int)"
  , ""
  , "result :: Box Maybe"
  , "result = Box Nothing"
  ]

checkOrdinaryFunctorFieldIsTier0 :: IO Check
checkOrdinaryFunctorFieldIsTier0 = do
  r <- extractResultTier "closuretier-ordinary-functor" "ClosureTierOrdinaryFunctor" ordinaryFunctorSrc
  pure $ check
    "Box Maybe -- the control -- stays Tier0 (isClosureType False)"
    (case r of
       Right tier -> not tier
       Left _     -> False)
