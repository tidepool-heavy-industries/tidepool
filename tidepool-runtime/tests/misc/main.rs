//! `misc` suite — small, independent regression/coverage files with no shared
//! theme, absorbed here as submodules. Each former top-level test binary
//! keeps its own private helpers; nextest still runs every `#[test]` in its
//! own process.

mod edge_cases;
mod integration;
mod json_decode_haskell;
mod text_breakon_replace_pure;
mod text_module_coverage;
mod time_module_coverage;
