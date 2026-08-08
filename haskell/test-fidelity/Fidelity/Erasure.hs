-- | Coercion evidence is erased in Haskell, symmetrically with type evidence:
-- a 'CoVar' binder emits no runtime lambda and consumes no join-point
-- parameter slot, matching the applications that already drop 'Coercion'
-- arguments.
module Fidelity.Erasure (checks) where

import Fidelity.Harness (Check, check, extractBinding, nlamCount)
import Tidepool.Translate (ClosedModule(..))

checks :: IO [Check]
checks = sequence
  [ checkCoercionPolyFunction
  , checkCoercionBinderJoinPoint
  ]

-- | A coercion-polymorphic worker: a GADT equality-evidence match ('Refl')
-- forces an -O2 worker/wrapper split whose worker abstracts directly over
-- the unboxed coercion. Confirmed via 'TIDEPOOL_DUMP_CLOSED=wcastG' against
-- this fixture through the real, rebuilt extract: Tidy Core shows
-- @\$wcastG_\<unique\> = \\ \@a \@b (ww :: b ~# a) (x :: a) -> x \`cast\` (Sub (Sym ww))@
-- — a genuine @Lam \<covar\>@, not merely a boxed dictionary parameter (the
-- wrapper 'castG' itself binds a boxed @Refl a b@ value, not a raw CoVar;
-- the worker is where the erasure bug actually lives).
--
-- The target passed to 'extractBinding' is @result@, not the worker itself:
-- the real pipeline renames a compiler-synthesized (non-external) worker's
-- OccName by appending a fresh disambiguating suffix (observed:
-- @\$wcastG_u8286623314361732527@), so an exact-string target lookup for
-- @\$wcastG@ never resolves. @result@ needs no such renaming (it is a plain
-- user-named exported binding) and calls the worker directly post-inlining
-- (GHC drops the now-dead 'castG' wrapper from @result@'s reachable
-- closure), so the worker's translated 'NLam' chain still ends up in the
-- closed module's node list without needing to name it exactly.
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
  r <- extractBinding "erasure-copoly" "CoPolyFixture" coPolySrc "result"
  pure $ check
    "coercion-polymorphic function (result -> $wcastG worker): nlamCount == 1 (the value binder x; the erased covar ww emits no NLam)"
    (case r of
       Right cm -> nlamCount cm == 1
       Left _   -> False)

-- | A self-recursive local function ('cast1', called in tail position from
-- both GADT alternatives of 'coJoinGo') whose Core binder list includes a
-- CoVar — GHC's canonical shape for a joinrec. A standalone @ghc
-- -ddump-simpl@ probe with this project's exact extraction flags
-- ('-O2 -fno-full-laziness -fno-cpr-anal') compiles this source to a
-- genuine recursive join (the size report tags it @joins: 0/1@): a local
-- @letrec { \$wcast1 = \\ \@b \@a1 (ww :: b ~# a1) (n :: Int#) (eta :: b) ->
-- case n of { 0# -> eta \`cast\` Sub ww; _ -> \$wcast1 ... (n -# 1#) eta } }@.
--
-- Through the REAL pipeline this exact source compiles to the SAME shape —
-- confirmed via 'TIDEPOOL_DUMP_CLOSED=coJoinGo' against the rebuilt
-- extract, which dumps the identical local @letrec@ — but 'TIDEPOOL_JOINREC_DEBUG=1'
-- against the same run emits no @[313-joinrec]@ trace line for it, and the
-- closed module carries zero @NJoin@\/@NJump@ nodes: this local binding does
-- NOT survive as an @isJoinId@-true Core join through this pipeline, unlike
-- the plain @ghc -ddump-simpl@ probe. It falls through 'translateHead's
-- ordinary @Let (Rec pairs) body@ -> @Nothing -> translate rhs@ arm
-- instead, as a plain self-recursive closure — so this fixture does not
-- reach 'collectValueBinders' itself, only the ordinary Lam-erasure path
-- (also fixed by this task, but not the join-arity-specific half of it).
--
-- The check below is still a meaningful, non-vacuous regression on the
-- SYMMETRIC erasure invariant regardless of which arm handles it: both the
-- ordinary-closure arm (@translate rhs@, recursing through
-- 'translateHead's @Lam@ case) and the join arm (@collectValueBinders@) are
-- built on the SAME 'isErasedBinder' predicate, so a CoVar formal parameter
-- either contributes zero 'NLam'\/zero value-binder slots in both arms, or —
-- pre-fix — an extra one in both. 'nlamCount' over the whole closed graph is
-- therefore a fix-fingerprint that doesn't depend on which arm actually
-- fires. No fixture in this module was found that reaches
-- 'collectValueBinders' through the real pipeline; see the commit message
-- for that gap.
coJoinSrc :: String
coJoinSrc = unlines
  [ "{-# LANGUAGE GADTs #-}"
  , "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module CoJoinFixture (coJoinGo) where"
  , ""
  , "data T a where"
  , "  TI :: T Int"
  , "  TB :: T Bool"
  , ""
  , "coJoinGo :: T a -> a -> a"
  , "coJoinGo t d = case t of"
  , "    TI -> cast1 (3 :: Int) (5 :: Int)"
  , "    TB -> cast1 (3 :: Int) d"
  , "  where"
  , "    cast1 :: forall b a1. (b ~ a1) => Int -> b -> a1"
  , "    cast1 0 x = x"
  , "    cast1 n x = cast1 (n - 1) x"
  ]

-- | @coJoinGo@ contributes 2 'NLam' (t, d — @\@a@ erased); @cast1@
-- contributes 2 more post-fix (n, eta — @\@b \@a1@ and the covar @ww@ all
-- erased) or 3 pre-fix (the covar wrongly kept as a value binder), giving a
-- total of 4 post-fix vs. 5 pre-fix. See 'coJoinSrc's haddock for why this
-- is checked via 'nlamCount' rather than 'NJoin'\/'NJump' shape.
checkCoercionBinderJoinPoint :: IO Check
checkCoercionBinderJoinPoint = do
  r <- extractBinding "erasure-cojoin" "CoJoinFixture" coJoinSrc "coJoinGo"
  pure $ check
    "coercion-binder recursive local function (cast1): nlamCount == 4 (n, eta, t, d; the erased @b @a1 ww all emit no NLam)"
    (case r of
       Left _   -> False
       Right cm -> null (cmUnresolved cm) && nlamCount cm == 4)
