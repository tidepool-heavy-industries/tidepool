//! Regression test for multi-module DataCon tag inconsistency.

mod common;

use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;
use tidepool_runtime::{compile_and_run, compile_haskell};

use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn prelude_path() -> std::path::PathBuf {
    common::prelude_path()
}

#[derive(FromCore)]
enum FooReq {
    #[core(name = "Ping")]
    Ping,
}

struct FooHandler;

impl EffectHandler for FooHandler {
    type Request = FooReq;
    fn handle(&mut self, req: FooReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            FooReq::Ping => cx.respond(()),
        }
    }
}

fn setup_multi_module_test() -> (TempDir, String) {
    let effect_dir = TempDir::new().expect("failed to create temp dir");

    let effect_src = r#"{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module RemoteEffect where
import Control.Monad.Freer (Eff, Member, send)
data Foo a where Ping :: Foo ()
sendFoo :: Member Foo effs => Eff effs ()
sendFoo = send Ping
"#;
    std::fs::write(effect_dir.path().join("RemoteEffect.hs"), effect_src)
        .expect("failed to write RemoteEffect.hs");

    let main_src = r#"{-# LANGUAGE DataKinds, TypeOperators, FlexibleContexts #-}
module MinRepro where
import Control.Monad.Freer (Eff)
import qualified RemoteEffect as R
agent :: Eff '[R.Foo] ()
agent = R.sendFoo
"#;

    (effect_dir, main_src.to_string())
}

/// Inspect the expression tree for tag mismatches.
#[test]
fn test_cross_module_inspect_tags() {
    let (effect_dir, main_src) = setup_multi_module_test();
    let pp = prelude_path();
    let include: Vec<&Path> = vec![pp.as_path(), effect_dir.path()];

    let (expr, _table, _warnings) =
        compile_haskell(&main_src, "agent", &include).expect("compilation failed");

    // Pretty print the expression tree
    eprintln!("Expression tree ({} nodes):", expr.nodes.len());
    let pp_expr = tidepool_repr::pretty::pretty_print(&expr);
    eprintln!("{}", pp_expr);

    // Check for the mystery tag
    let target_tag = 0xfe39c1e45ffaa2ad_u64;
    eprintln!("\nSearching for mystery tag {:#018x}:", target_tag);

    use tidepool_repr::frame::CoreFrame;
    use tidepool_repr::types::AltCon;
    for (i, node) in expr.nodes.iter().enumerate() {
        match node {
            CoreFrame::Case { alts, .. } => {
                for alt in alts {
                    if let AltCon::DataAlt(id) = alt.con {
                        if id.0 == target_tag {
                            eprintln!("  FOUND at node {} in case alt", i);
                        }
                    }
                }
            }
            CoreFrame::Con { tag, .. } if tag.0 == target_tag => {
                eprintln!("  FOUND at node {} as Con", i);
            }
            CoreFrame::Var(vid) if vid.0 == target_tag => {
                eprintln!("  FOUND at node {} as Var", i);
            }
            _ => {}
        }
    }
}

/// Test that cross-module effect actually runs without CASE TRAP.
#[test]
fn test_cross_module_effect_runs() {
    let (effect_dir, main_src) = setup_multi_module_test();
    let pp = prelude_path();
    let effect_path: PathBuf = effect_dir.path().to_owned();
    let pp_clone = pp.clone();

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let _keep = effect_dir;
            let include: Vec<&Path> = vec![pp_clone.as_path(), effect_path.as_path()];
            let mut handlers = frunk::hlist![FooHandler];
            compile_and_run(&main_src, "agent", &include, &mut handlers, &())
                .expect("compile_and_run should succeed for cross-module effect")
        })
        .unwrap()
        .join()
        .unwrap();

    let json = result.to_json();
    assert_eq!(json, serde_json::json!(null));
}
