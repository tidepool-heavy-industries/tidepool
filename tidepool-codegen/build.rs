#![warn(clippy::unwrap_used, clippy::expect_used)]
fn main() {
    println!("cargo:rerun-if-changed=csrc/prepared_md5/md5.c");
    println!("cargo:rerun-if-changed=csrc/prepared_md5/md5.h");
    cc::Build::new()
        .file("csrc/prepared_md5/md5.c")
        .include("csrc/prepared_md5")
        .compile("prepared_md5");
}
