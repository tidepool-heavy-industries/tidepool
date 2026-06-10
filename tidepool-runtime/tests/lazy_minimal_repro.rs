//! Minimal-stack regression coverage for lazy effect results: one
//! hand-written effect (no MCP template), one handler responding with a
//! 12k-element list. Historically this PASSED while the MCP-template shape
//! hung — because this harness ran on an 8 MiB thread, masking the real bug
//! (recursive Drop of the deep response spine overflowing the eval thread's
//! stack → SIGSEGV outside signal protection → silent thread exit → hang).
//! Kept as the minimal-stack half of that bisect.

mod common;

use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

#[derive(FromCore)]
enum ListingReq {
    #[core(name = "GetList")]
    GetList,
}

struct BigListing {
    n: usize,
}

impl EffectHandler for BigListing {
    type Request = ListingReq;
    fn handle(&mut self, req: ListingReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ListingReq::GetList => {
                let items: Vec<String> = (0..self.n).map(|i| format!("item-{i}")).collect();
                cx.respond(items)
            }
        }
    }
}

fn run_minimal(body: &str, n: usize) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds,
             TypeOperators, GADTs, FlexibleContexts, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
default (Int, Text)

data Listing a where
    GetList :: Listing [Text]

result :: Eff '[Listing] _
result = do
{body}
"#
    );
    std::env::set_var("TIDEPOOL_LAZY_RESULTS", "1");
    let pp = common::prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![BigListing { n }];
            compile_and_run(&src, "result", &include, &mut handlers, &())
                .expect("compile_and_run failed")
                .to_json()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn minimal_take_prefix() {
    let r = run_minimal("  xs <- send GetList\n  pure (take 3 xs)", 12_000);
    assert_eq!(r, serde_json::json!(["item-0", "item-1", "item-2"]));
}

#[test]
fn minimal_full_length() {
    // Control: full traversal worked under the MCP wrapper too.
    let r = run_minimal("  xs <- send GetList\n  pure (length xs)", 12_000);
    assert_eq!(r, serde_json::json!(12_000));
}
