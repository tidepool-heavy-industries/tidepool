#[test]
fn public_codegen_library_links_the_prepared_md5_archive() {
    // `session_var_id` uses the crate's native MD5 kernel. This fixed value is
    // MD5("M:x" encoded as UTF-32BE), reduced by GHC's stable-VarId formula.
    assert_eq!(
        tidepool_codegen::prepared_program::session_var_id("M", "x"),
        0xfeba_2fe7_94be_29bd,
    );
}
