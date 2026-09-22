//! Trybuild coverage for borrow contracts that type-level assertions cannot
//! express. `.stderr` goldens are rustc-version-sensitive; the toolchain here is
//! nix-pinned, so a future toolchain bump is the trigger to regenerate them
//! (`TRYBUILD=overwrite cargo test --test compile_fail`), not to debug a
//! diff.

#[test]
fn compile_fail_tokens() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
