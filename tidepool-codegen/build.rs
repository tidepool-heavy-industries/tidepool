fn main() {
    #[cfg(unix)]
    {
        cc::Build::new()
            .file("csrc/sigsetjmp_wrapper.c")
            .compile("sigsetjmp_wrapper");
    }
}
