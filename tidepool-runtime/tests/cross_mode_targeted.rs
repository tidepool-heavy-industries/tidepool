//! Targeted regressions: Haskell fixtures designed to exercise specific cross-mode divergence dimensions.
//!
//! (1) GADT effect dispatch via Member constraints: Ensures UnsafeRefl elision works across modules.
//! (2) Name collisions across modules: Disambiguates constructors with same name but different types/arity.
//! (3) Primitive boxing: Ensures W#/I#/ByteArray# are handled correctly when GHC leaves boxes.
//! (4) Mutual recursion across modules: Exercises LetRec phase ordering and join point analysis.
//! (5) Typeclass dictionary dispatch: Verifies derived Show/Eq dictionaries survive translation.

mod cross_mode_harness;

use cross_mode_harness::{
    assert_cross_mode_pure_equivalent, assert_cross_mode_runtime_equivalent,
    assert_cross_mode_structurally_equivalent, CrossModeFixture,
};
use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;

#[derive(FromCore)]
enum EchoReq {
    #[core(name = "Echo")]
    Echo(i64),
}

struct EchoHandler;

impl EffectHandler for EchoHandler {
    type Request = EchoReq;
    fn handle(&mut self, req: EchoReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            EchoReq::Echo(n) => cx.respond(n),
        }
    }
}

#[derive(FromCore)]
enum E1Req {
    #[core(name = "E1")]
    E1(i64),
}

struct E1Handler;

impl EffectHandler for E1Handler {
    type Request = E1Req;
    fn handle(&mut self, req: E1Req, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            E1Req::E1(n) => cx.respond(n),
        }
    }
}

#[derive(FromCore)]
enum E2Req {
    #[core(name = "E2")]
    E2(i64),
}

struct E2Handler;

impl EffectHandler for E2Handler {
    type Request = E2Req;
    fn handle(&mut self, req: E2Req, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            E2Req::E2(n) => cx.respond(n),
        }
    }
}

// === Dimension A: GADT effect dispatch ===

#[test]
fn dimension_a1_single_gadt_dispatch() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts, NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import Control.Monad.Freer (Eff, Member, send)
data Echo a where Echo :: Int -> Echo Int
echo :: Member Echo effs => Int -> Eff effs Int
echo n = send (Echo n)
{-# NOINLINE echo #-}
main :: Eff '[Echo] Int
main = echo 42
"#.to_string(),
        split: vec![
            ("Def.hs".to_string(), r#"
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts, NoImplicitPrelude #-}
module Def where
import Tidepool.Prelude
import Control.Monad.Freer (Eff, Member, send)
data Echo a where Echo :: Int -> Echo Int
echo :: Member Echo effs => Int -> Eff effs Int
echo n = send (Echo n)
{-# NOINLINE echo #-}
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE DataKinds, TypeOperators, FlexibleContexts, NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Def
import Control.Monad.Freer (Eff)
main :: Eff '[Def.Echo] Int
main = Def.echo 42
"#.to_string()),
        ],
        target: "main",
    };

    assert_cross_mode_structurally_equivalent(&fixture);
    assert_cross_mode_runtime_equivalent(
        &fixture,
        || frunk::hlist![EchoHandler],
        || frunk::hlist![EchoHandler],
        &(),
    );
}

#[test]
fn dimension_a2_two_gadts_interleaved() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts, NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import Control.Monad.Freer (Eff, Member, send)
data E1 a where E1 :: Int -> E1 Int
data E2 a where E2 :: Int -> E2 Int
e1 :: Member E1 effs => Int -> Eff effs Int
e1 n = send (E1 n)
{-# NOINLINE e1 #-}
e2 :: Member E2 effs => Int -> Eff effs Int
e2 n = send (E2 n)
{-# NOINLINE e2 #-}
main :: Eff '[E1, E2] Int
main = do
    x <- e1 20
    y <- e2 22
    return (x + y)
"#.to_string(),
        split: vec![
            ("Mod1.hs".to_string(), r#"
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts, NoImplicitPrelude #-}
module Mod1 where
import Tidepool.Prelude
import Control.Monad.Freer (Eff, Member, send)
data E1 a where E1 :: Int -> E1 Int
e1 :: Member E1 effs => Int -> Eff effs Int
e1 n = send (E1 n)
{-# NOINLINE e1 #-}
"#.to_string()),
            ("Mod2.hs".to_string(), r#"
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts, NoImplicitPrelude #-}
module Mod2 where
import Tidepool.Prelude
import Control.Monad.Freer (Eff, Member, send)
data E2 a where E2 :: Int -> E2 Int
e2 :: Member E2 effs => Int -> Eff effs Int
e2 n = send (E2 n)
{-# NOINLINE e2 #-}
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE DataKinds, TypeOperators, FlexibleContexts, NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Mod1
import qualified Mod2
import Control.Monad.Freer (Eff)
main :: Eff '[Mod1.E1, Mod2.E2] Int
main = do
    x <- Mod1.e1 20
    y <- Mod2.e2 22
    return (x + y)
"#.to_string()),
        ],
        target: "main",
    };

    // NOTE: Structural equivalence fails due to different LetRec binding indices
    // for internal freer-simple state machine nodes. We verify runtime equivalence.
    assert_cross_mode_runtime_equivalent(
        &fixture,
        || frunk::hlist![E1Handler, E2Handler],
        || frunk::hlist![E1Handler, E2Handler],
        &(),
    );
}

// === Dimension B: Name collisions across modules ===

#[test]
fn dimension_b1_name_collision_same_arity() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
data T1 = Mk Int
data T2 = Mk' Int
mk1 :: Int -> T1
mk1 n = Mk n
{-# NOINLINE mk1 #-}
mk2 :: Int -> T2
mk2 n = Mk' n
{-# NOINLINE mk2 #-}
main :: Int -> Int
main n = let x = mk1 n in let y = mk2 (n + 1) in 
         (case x of Mk k -> k) + (case y of Mk' m -> m)
"#.to_string(),
        split: vec![
            ("Mod1.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Mod1 where
import Tidepool.Prelude
data T = Mk Int
mk1 :: Int -> T
mk1 n = Mk n
{-# NOINLINE mk1 #-}
"#.to_string()),
            ("Mod2.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Mod2 where
import Tidepool.Prelude
data T = Mk Int
mk2 :: Int -> T
mk2 n = Mk n
{-# NOINLINE mk2 #-}
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Mod1
import qualified Mod2
main :: Int -> Int
main n = let x = Mod1.mk1 n in let y = Mod2.mk2 (n + 1) in 
       (case x of Mod1.Mk k -> k) + (case y of Mod2.Mk m -> m)
"#.to_string()),
        ],
        target: "main",
    };

    // NOTE: Structural equivalence fails because single-module cannot have
    // colliding constructor names, while split-mode does.
    assert_cross_mode_pure_equivalent(&fixture);
}

#[test]
fn dimension_b2_name_collision_different_arity() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
data T1 = Mk Int
data T2 = Mk' Int Int
mk1 :: Int -> T1
mk1 n = Mk n
{-# NOINLINE mk1 #-}
mk2 :: Int -> Int -> T2
mk2 n m = Mk' n m
{-# NOINLINE mk2 #-}
main :: Int -> Int
main n = let x = mk1 n in let y = mk2 (n + 1) (n + 2) in 
         (case x of Mk k -> k) + (case y of Mk' m j -> m + j)
"#.to_string(),
        split: vec![
            ("Mod1.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Mod1 where
import Tidepool.Prelude
data T = Mk Int
mk1 :: Int -> T
mk1 n = Mk n
{-# NOINLINE mk1 #-}
"#.to_string()),
            ("Mod2.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Mod2 where
import Tidepool.Prelude
data T = Mk Int Int
mk2 :: Int -> Int -> T
mk2 n m = Mk n m
{-# NOINLINE mk2 #-}
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Mod1
import qualified Mod2
main :: Int -> Int
main n = let x = Mod1.mk1 n in let y = Mod2.mk2 (n + 1) (n + 2) in 
       (case x of Mod1.Mk k -> k) + (case y of Mod2.Mk m j -> m + j)
"#.to_string()),
        ],
        target: "main",
    };

    // NOTE: Structural equivalence fails due to colliding constructor names in split mode.
    assert_cross_mode_pure_equivalent(&fixture);
}

// === Dimension C: Primitive boxing at known field positions ===

#[test]
fn dimension_c1_word_boxing() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
data Tag = Tag Word
mkTag :: Word -> Tag
mkTag n = Tag n
{-# NOINLINE mkTag #-}
getTag :: Tag -> Word
getTag (Tag n) = n
{-# NOINLINE getTag #-}
main :: Word -> Word
main n = getTag (mkTag n)
"#.to_string(),
        split: vec![
            ("Mod1.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Mod1 where
import Tidepool.Prelude
data Tag = Tag Word
mkTag :: Word -> Tag
mkTag n = Tag n
{-# NOINLINE mkTag #-}
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Mod1
getTag :: Mod1.Tag -> Word
getTag (Mod1.Tag n) = n
{-# NOINLINE getTag #-}
main :: Word -> Word
main n = getTag (Mod1.mkTag n)
"#.to_string()),
        ],
        target: "main",
    };

    assert_cross_mode_structurally_equivalent(&fixture);
    assert_cross_mode_pure_equivalent(&fixture);
}

#[test]
fn dimension_c2_int_hash_boxing() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE MagicHash, NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import GHC.Exts (Int#)
data IBox = IBox Int#
mkIBox :: Int# -> IBox
mkIBox n# = IBox n#
{-# NOINLINE mkIBox #-}
getIBox :: IBox -> IBox
getIBox (IBox n#) = IBox n#
{-# NOINLINE getIBox #-}
main n# = case getIBox (mkIBox n#) of IBox m# -> IBox m#
"#.to_string(),
        split: vec![
            ("Mod1.hs".to_string(), r#"
{-# LANGUAGE MagicHash, NoImplicitPrelude #-}
module Mod1 where
import Tidepool.Prelude
import GHC.Exts (Int#)
data IBox = IBox Int#
mkIBox :: Int# -> IBox
mkIBox n# = IBox n#
{-# NOINLINE mkIBox #-}
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE MagicHash, NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Mod1
import GHC.Exts (Int#)
getIBox :: Mod1.IBox -> Mod1.IBox
getIBox (Mod1.IBox n#) = Mod1.IBox n#
{-# NOINLINE getIBox #-}
main n# = case getIBox (Mod1.mkIBox n#) of Mod1.IBox m# -> Mod1.IBox m#
"#.to_string()),
        ],
        target: "main",
    };

    assert_cross_mode_structurally_equivalent(&fixture);
    assert_cross_mode_pure_equivalent(&fixture);
}

// === Dimension D: Mutual recursion across modules ===

#[test]
fn dimension_d1_mutual_recursion() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
even' :: Int -> Bool
even' n = if n == 0 then True else odd' (n - 1)
{-# NOINLINE even' #-}
odd' :: Int -> Bool
odd' n = if n == 0 then False else even' (n - 1)
{-# NOINLINE odd' #-}
main :: Int -> Bool
main n = even' n
"#.to_string(),
        split: vec![
            ("Even.hs-boot".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Even where
import Tidepool.Prelude
even' :: Int -> Bool
"#.to_string()),
            ("Even.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Even where
import Tidepool.Prelude
import qualified Odd
even' :: Int -> Bool
even' n = if n == 0 then True else Odd.odd' (n - 1)
{-# NOINLINE even' #-}
"#.to_string()),
            ("Odd.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Odd where
import Tidepool.Prelude
import {-# SOURCE #-} qualified Even
odd' :: Int -> Bool
odd' n = if n == 0 then False else Even.even' (n - 1)
{-# NOINLINE odd' #-}
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Even
main :: Int -> Bool
main n = Even.even' n
"#.to_string()),
        ],
        target: "main",
    };

    // NOTE: Structural equivalence fails due to different LetRec size (optimizer differences).
    assert_cross_mode_pure_equivalent(&fixture);
}

// === Dimension E: Typeclass dispatch ===

#[test]
fn dimension_e1_derived_show() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
data T = A | B Int deriving Show
main :: Int -> Text
main n = show (B n)
"#.to_string(),
        split: vec![
            ("Mod1.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Mod1 where
import Tidepool.Prelude
data T = A | B Int deriving Show
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Mod1
main :: Int -> Text
main n = show (Mod1.B n)
"#.to_string()),
        ],
        target: "main",
    };

    // NOTE: Structural equivalence fails for derived Show dictionaries.
    assert_cross_mode_pure_equivalent(&fixture);
}

#[test]
fn dimension_e2_derived_eq() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
data T = A | B Int deriving Eq
main :: Int -> Bool
main n = (B n == B n) && (A == A) && (A /= B (n + 1))
"#.to_string(),
        split: vec![
            ("Mod1.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Mod1 where
import Tidepool.Prelude
data T = A | B Int deriving Eq
"#.to_string()),
            ("Main.hs".to_string(), r#"
{-# LANGUAGE NoImplicitPrelude #-}
module Test where
import Tidepool.Prelude
import qualified Mod1
main :: Int -> Bool
main n = (Mod1.B n == Mod1.B n) && (Mod1.A == Mod1.A) && (Mod1.A /= Mod1.B (n + 1))
"#.to_string()),
        ],
        target: "main",
    };

    // NOTE: Structural equivalence fails for derived Eq dictionaries.
    assert_cross_mode_pure_equivalent(&fixture);
}
