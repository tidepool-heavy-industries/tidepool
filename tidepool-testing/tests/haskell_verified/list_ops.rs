//! Verified-generator templates for list operations: filter, map, take, drop,
//! zip/unzip, reverse, sort, replicate, elem.
//!
//! Stub: filled in by the `list_ops` leaf. Follows the patterns in
//! `fmap.rs` / `text.rs` / `cousins.rs`. ASCII-only, Int-only, pinned types,
//! `ProptestConfig::with_cases(50)` per template.

#![allow(unused_imports)]

use crate::{arb_int, run_template};
use proptest::prelude::*;
use serde_json::json;
