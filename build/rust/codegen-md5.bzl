load("@prelude//:rules.bzl", "cxx_library")

# Native Buck replacement for tidepool/codegen/build.rs's cc::Build action.
def tidepool_codegen_md5():
    cxx_library(
        name = "prepared_md5_native",
        srcs = ["csrc/prepared_md5/md5.c"],
        headers = ["csrc/prepared_md5/md5.h"],
        preferred_linkage = "static",
    )
