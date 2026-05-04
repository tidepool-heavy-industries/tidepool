//! Verified-generator templates for additional Text ops (T.words/lines/unwords/
//! unlines, T.reverse, T.concat, pack/unpack roundtrip) and recursive patterns
//! (factorial via letrec, fib bounded depth, liftA2 over Maybe Int).
//!
//! Stub: filled in by the `more_text_recursive` leaf. Follows the patterns in
//! `fmap.rs` / `text.rs` / `cousins.rs`. ASCII-only, Int-only, pinned types,
//! `ProptestConfig::with_cases(50)` per template.

#![allow(unused_imports)]

use crate::{arb_int, arb_text, run_template};
use proptest::prelude::*;
use serde_json::json;
