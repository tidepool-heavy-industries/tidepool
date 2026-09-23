#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
#[path = "../support/mod.rs"]
mod support;

#[path = "../constructor_authority.rs"]
mod constructor_authority;
#[path = "../error_cases.rs"]
mod error_cases;
#[allow(dead_code)]
#[path = "../haskell_record.rs"]
mod haskell_record;
#[path = "../proptest_text.rs"]
mod proptest_text;
#[path = "../roundtrip.rs"]
mod roundtrip;
