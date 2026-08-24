use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// EntropyReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// body below are hand-written.
tidepool_mcp::entropy_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct EntropyHandler;

impl EntropyHandler {
    /// Fresh OS entropy as a 64-bit seed. `std::collections::hash_map::RandomState`
    /// draws its per-instance SipHash keys from the OS RNG at construction
    /// time (documented libstd behavior, the same source `HashMap`'s DOS
    /// resistance relies on) — hashing zero bytes and taking `finish()`
    /// exposes those keys as one `u64` without adding a dependency.
    fn entropy_seed(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let seed = RandomState::new().build_hasher().finish() as i64;
        cx.respond(seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;

    #[test]
    fn test_entropy_dispatch_seed() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![EntropyHandler];
        let con_id = table.get_by_name("EntropySeed").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
        // i64::to_value boxes as Con(I#, [LitInt]).
        match &result {
            Value::Con(_, fields) if fields.len() == 1 => match &fields[0] {
                Value::Lit(tidepool_repr::Literal::LitInt(_)) => {}
                _ => panic!("Expected Con(_, [LitInt]), got {:?}", result),
            },
            _ => panic!("Expected Con(I#, [LitInt(seed)]), got {:?}", result),
        }
    }

    // === Entropy JIT e2e tests ===
    //
    // One compile bundles: pure StdGen/randomR/randoms/split (bounds
    // respected, seeded determinism, split branches diverge) with the
    // impure newStdGen/randomRIO seam (two draws from two OS-entropy-seeded
    // generators differ) — see root CLAUDE.md's family-bundle test rule.

    fn entropy_jit_handlers(
        cwd: std::path::PathBuf,
        kv_path: std::path::PathBuf,
    ) -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput> {
        crate::build_base_stack(&crate::HandlerConfig {
            cwd,
            kv_path,
            llm_model: "ollama:llama3.2".to_string(),
        })
    }

    #[tokio::test]
    async fn test_jit_entropy_family() {
        if !tidepool_testing::eval_harness::extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        let source = jit_test_source(&[
            "let check nm ok = if ok then [] else [nm]",
            "let g0 = mkStdGen 42",
            "let (a, g1) = randomR (1 :: Int, 100 :: Int) g0",
            "let (b, _) = randomR (1 :: Int, 100 :: Int) g1",
            "let inBounds x = x >= 1 && x <= 100",
            "let g0' = mkStdGen 42",
            "let (a', _) = randomR (1 :: Int, 100 :: Int) g0'",
            "let (gs1, gs2) = split g1",
            "let (c, _) = randomR (1 :: Int, 100 :: Int) gs1",
            "let (d, _) = randomR (1 :: Int, 100 :: Int) gs2",
            "let (dbl, _) = randomR (0.0 :: Double, 1.0 :: Double) g1",
            "n <- randomRIO (1 :: Int, 100 :: Int)",
            "sg1 <- newStdGen",
            "sg2 <- newStdGen",
            "let (s1, _) = randomR (1 :: Int, 1000000000 :: Int) sg1",
            "let (s2, _) = randomR (1 :: Int, 1000000000 :: Int) sg2",
            "let c1 = check \"randomR-bounds-respected\" (inBounds a && inBounds b && inBounds c && inBounds d && inBounds n)",
            "let c2 = check \"mkStdGen-deterministic\" (a == a')",
            "let c3 = check \"split-branches-diverge\" (c /= d)",
            "let c4 = check \"randomR-double-in-unit-interval\" (dbl >= 0.0 && dbl < 1.0)",
            "let c5 = check \"newStdGen-seeds-vary\" (s1 /= s2)",
            "pure (concat [c1, c2, c3, c4, c5])",
        ]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![
            include.as_path(),
            effects_dir.core.as_path(),
            effects_dir.shim.as_path(),
        ];
        let kv_path = std::env::temp_dir().join("tidepool_entropy_family_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = entropy_jit_handlers(cwd, kv_path);
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(v) => {
                assert_eq!(
                    v.to_json(),
                    serde_json::json!([]),
                    "failed checks: {:?}",
                    v.to_json()
                );
            }
            Err(e) => panic!("JIT entropy family eval failed: {:?}", e),
        }
    }
}
