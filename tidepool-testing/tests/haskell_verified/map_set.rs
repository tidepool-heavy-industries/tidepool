//! Verified-generator templates for Data.Map and Data.Set operations:
//! Map.fromList/lookup/insert/union, Set.fromList/member/union (over Int keys
//! and values).
//!
//! Stub: filled in by the `map_set` leaf. Follows the patterns in
//! `fmap.rs` / `text.rs` / `cousins.rs`. ASCII-only, Int-only, pinned types,
//! `ProptestConfig::with_cases(50)` per template.

#![allow(unused_imports)]

use crate::{arb_int, run_template};
use proptest::prelude::*;
use serde_json::json;
