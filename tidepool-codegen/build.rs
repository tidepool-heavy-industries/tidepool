fn main() {
    #[cfg(unix)]
    {
        println!("cargo:rerun-if-changed=csrc/sigsetjmp_wrapper.c");
        cc::Build::new()
            .file("csrc/sigsetjmp_wrapper.c")
            .compile("sigsetjmp_wrapper");
    }
}
