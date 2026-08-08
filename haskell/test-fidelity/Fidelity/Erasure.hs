-- | Coercion evidence is erased in Haskell, symmetrically with type evidence:
-- a 'CoVar' binder emits no runtime lambda and consumes no join-point
-- parameter slot, matching the applications that already drop 'Coercion'
-- arguments.
module Fidelity.Erasure (checks) where

import Fidelity.Harness (Check, check, extractBinding, nodeList, nlamCount)
import Tidepool.Translate (ClosedModule(..), FlatNode(..))

checks :: IO [Check]
checks = sequence
  [ checkCoercionPolyFunction
  , checkCoercionBinderJoinPoint
  ]

-- | A coercion-polymorphic worker: a GADT equality-evidence match ('Refl')
-- forces an -O2 worker/wrapper split whose worker abstracts directly over
-- the unboxed coercion. Confirmed by running 'TIDEPOOL_DUMP_CLOSED=$wcastG'
-- against this fixture through the real pipeline (and independently via
-- plain @ghc -ddump-simpl@ with this project's exact extraction flags,
-- '-O2 -fno-full-laziness -fno-cpr-anal'): Tidy Core shows
-- @$wcastG = \\ \@a \@b (ww :: b ~# a) (x :: a) -> x \`cast\` (Sub (Sym ww))@
-- — a genuine @Lam \<covar\>@, not merely a boxed dictionary parameter (the
-- wrapper 'castG' itself binds a boxed @Refl a b@ value, not a raw CoVar;
-- the worker is where the erasure bug actually lives).
coPolySrc :: String
coPolySrc = unlines
  [ "{-# LANGUAGE GADTs #-}"
  , "module CoPolyFixture (castG, result) where"
  , ""
  , "data Refl a b where"
  , "  Refl :: Refl a a"
  , ""
  , "castG :: Refl a b -> a -> b"
  , "castG Refl x = x"
  , "{-# NOINLINE castG #-}"
  , ""
  , "result :: Int"
  , "result = castG Refl (42 :: Int)"
  ]

checkCoercionPolyFunction :: IO Check
checkCoercionPolyFunction = do
  r <- extractBinding "e2-copoly" "CoPolyFixture" coPolySrc "$wcastG"
  pure $ check
    "coercion-polymorphic function ($wcastG) extracts with nlamCount == 1 (the value binder x; the erased covar ww emits no NLam)"
    (case r of
       Right cm -> nlamCount cm == 1
       Left _   -> False)

-- | A non-recursive local join point ('cast1', shared tail-call target of
-- both GADT alternatives of 'go') whose Core binder list includes a CoVar.
-- Confirmed by running 'TIDEPOOL_JOINREC_DEBUG=1' + 'TIDEPOOL_DUMP_CLOSED=go'
-- against this fixture through the real pipeline (and independently via
-- plain @ghc -ddump-simpl@ with this project's exact extraction flags):
-- Tidy Core shows
-- @let { \$wcast1 :: forall b a1. (b ~# a1) => b -> a1
--        \$wcast1 = \\ \@b \@a1 (ww :: b ~# a1) (eta :: b) -> eta \`cast\` Sub ww }
--  in case t of { TI -> ... \$wcast1 ...; TB -> ... \$wcast1 ... }@
-- — the @-ddump-simpl@ size report tags 'go's RHS 'joins: 0/1' (one genuine
-- non-recursive join, not a case-alternative and not a lambda-lifted
-- top-level function): a real 'NJoin', not the case-alt CoVar filter this
-- task's boundary already covers.
coJoinSrc :: String
coJoinSrc = unlines
  [ "{-# LANGUAGE GADTs #-}"
  , "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module CoJoinFixture (go) where"
  , ""
  , "data T a where"
  , "  TI :: T Int"
  , "  TB :: T Bool"
  , ""
  , "go :: Bool -> T a -> a -> a"
  , "go flip1 t d = case t of"
  , "    TI -> if flip1 then cast1 (5 :: Int) else cast1 (6 :: Int)"
  , "    TB -> cast1 d"
  , "  where"
  , "    cast1 :: forall b a1. (b ~ a1) => b -> a1"
  , "    cast1 x = x"
  , "    {-# NOINLINE cast1 #-}"
  ]

checkCoercionBinderJoinPoint :: IO Check
checkCoercionBinderJoinPoint = do
  r <- extractBinding "e2-cojoin" "CoJoinFixture" coJoinSrc "go"
  pure $ check
    "coercion-binder join point (cast1): NJoin's param count matches every NJump's arg count against it, nothing unresolved"
    (case r of
       Left _   -> False
       Right cm ->
         null (cmUnresolved cm) &&
         case [ (b, ps) | NJoin b ps _ _ <- nodeList cm ] of
           [(joinB, params)] ->
             let jumpArgCounts = [ length args | NJump b args <- nodeList cm, b == joinB ]
             in not (null jumpArgCounts) && all (== length params) jumpArgCounts
           _ -> False)
