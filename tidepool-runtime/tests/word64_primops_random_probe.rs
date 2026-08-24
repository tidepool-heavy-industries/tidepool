//! End-to-end unlock probe for the Word64#/Int64# primop family
//! (`plusWord64#`/`subWord64#`/`timesWord64#`/`quot`/`remWord64#`,
//! `and64#`/`or64#`/`xor64#`/`not64#`, the 64-bit shifts, `Word64`/`Int64`
//! comparisons, and `wordToWord64#`/`word64ToWord#`) added in
//! `tidepool-codegen/src/emit/primop.rs`.
//!
//! Before this family existed, the real `random`/`splitmix` packages failed
//! JIT codegen on exactly these primops (`Tidepool.Random`'s header names the
//! specific ops), which is why `Tidepool.Random` shipped as a hand-rolled
//! generator rather than re-exporting the real packages. This probe compiles
//! and runs a plain module that imports the REAL `System.Random` package
//! (not the `Tidepool.Random` stdlib shim) directly under the JIT — the exact
//! case that used to fail.
//!
//! The expected sequence is a GOLDEN value computed by running the identical
//! `genSeq` against a plain GHC (`runghc`, the real `random`-1.2.1.3 package,
//! no Tidepool involved) — so this pins JIT-vs-real-GHC agreement, not just
//! JIT self-consistency.
//!
//! Needs `random`/`splitmix` on the resolving GHC's package path (added to
//! `flake.nix`'s `ghcEnv` alongside `freer-simple`/`lens`/etc.) — a plain
//! `nix develop` shell's bare GHC does not carry them; use the `with-packages`
//! GHC on PATH per `haskell/CLAUDE.md`'s local-iteration recipe.
use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

// Golden sequence for `mkStdGen 42`, five draws of `randomR (1, 100 :: Int)`,
// computed independently via `runghc` against the real `random` package:
// `print (genSeq 42 5)` prints `[49,24,96,16,81]`.
#[test]
fn system_random_real_package_compiles_and_runs_under_jit() {
    let src = r#"module M where

import System.Random (mkStdGen, randomR)

genSeq :: Int -> Int -> [Int]
genSeq seed n = go n (mkStdGen seed)
  where
    go 0 _ = []
    go k g =
      let (val, g') = randomR (1, 100 :: Int) g
      in val : go (k - 1 :: Int) g'

x :: String
x = show (genSeq 42 5)
"#;
    let v = EvalHarness::new()
        .run_pure(src, "x")
        .expect("System.Random probe failed under the JIT")
        .to_json();
    assert_eq!(v, json!("[49,24,96,16,81]"));
}
