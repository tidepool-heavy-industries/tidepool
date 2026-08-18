//! trybuild harness for the linear/custody token family's compile-time
//! guarantees that a `static_assertions` pin can't express (use-after-move,
//! double-borrow) — see each fixture's own comment for which guarantee it
//! pins. `.stderr` goldens are rustc-version-sensitive; the toolchain here is
//! nix-pinned, so a future toolchain bump is the trigger to regenerate them
//! (`TRYBUILD=overwrite cargo test --test compile_fail`), not to debug a
//! diff.

#[test]
fn compile_fail_tokens() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
