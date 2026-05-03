//! Sanity tests for the cross-mode test harness.

mod cross_mode_harness;

use cross_mode_harness::{
    assert_cross_mode_pure_equivalent, assert_cross_mode_runtime_equivalent,
    assert_cross_mode_structurally_equivalent, CrossModeFixture,
};
use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;

#[test]
fn harness_pure_value_roundtrips() {
    let fixture = CrossModeFixture {
        single: r#"
module Test where
y = 42 :: Int
x = y
"#
        .to_string(),
        split: vec![
            (
                "Helper.hs".to_string(),
                r#"
module Helper where
y = 42 :: Int
"#
                .to_string(),
            ),
            (
                "Main.hs".to_string(),
                r#"
module Test where
import qualified Helper
x = Helper.y
"#
                .to_string(),
            ),
        ],
        target: "x",
    };

    // Assert structural equivalence (Core trees modulo IDs)
    assert_cross_mode_structurally_equivalent(&fixture);

    // Assert runtime equivalence
    assert_cross_mode_pure_equivalent(&fixture);
}

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

#[test]
fn harness_effect_roundtrips() {
    let fixture = CrossModeFixture {
        single: r#"
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module Test where
import Control.Monad.Freer (Eff, Member, send)
data Echo a where Echo :: Int -> Echo Int
main :: Eff '[Echo] Int
main = send (Echo 42)
"#
        .to_string(),
        split: vec![
            (
                "Def.hs".to_string(),
                r#"
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module Def where
import Control.Monad.Freer (Eff, Member, send)
data Echo a where Echo :: Int -> Echo Int
echo :: Member Echo effs => Int -> Eff effs Int
echo n = send (Echo n)
"#
                .to_string(),
            ),
            (
                "Main.hs".to_string(),
                r#"
{-# LANGUAGE DataKinds, TypeOperators, FlexibleContexts #-}
module Test where
import qualified Def
import Control.Monad.Freer (Eff)
main :: Eff '[Def.Echo] Int
main = Def.echo 42
"#
                .to_string(),
            ),
        ],
        target: "main",
    };

    // For effects, we assert runtime equivalence.
    assert_cross_mode_runtime_equivalent(
        &fixture,
        || frunk::hlist![EchoHandler],
        || frunk::hlist![EchoHandler],
        &(),
    );
}

#[test]
fn harness_detects_obvious_divergence() {
    let fixture = CrossModeFixture {
        single: r#"
module Test where
x = 1 :: Int
"#
        .to_string(),
        split: vec![
            (
                "Helper.hs".to_string(),
                r#"
module Helper where
y = 2 :: Int
"#
                .to_string(),
            ),
            (
                "Main.hs".to_string(),
                r#"
module Test where
import qualified Helper
x = Helper.y
"#
                .to_string(),
            ),
        ],
        target: "x",
    };

    // Runtime equivalence should fail because 1 != 2
    let result = std::panic::catch_unwind(|| {
        assert_cross_mode_pure_equivalent(&fixture);
    });

    assert!(
        result.is_err(),
        "harness should have detected value divergence"
    );
}
