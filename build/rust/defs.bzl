load("@prelude//:rules.bzl", "rust_binary", "rust_library", "rust_test")

_HOT_CRATES = ["tidepool_codegen", "tidepool_repr", "tidepool_heap"]

def _common(name, package_name, package_dir, version, env, rustc_flags):
    manifest_env = {
        "CARGO_MANIFEST_DIR": package_dir,
        "CARGO_PKG_NAME": package_name,
        "CARGO_PKG_VERSION": version,
    }
    manifest_env.update(env)
    opt = "3" if name in _HOT_CRATES else "0"
    return (manifest_env, ["-Copt-level=" + opt] + rustc_flags)

def tidepool_rust_library(name, package_name, package_dir, version, env = {}, rustc_flags = [], **kwargs):
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    rust_library(name = name, env = compiler_env, rustc_flags = flags, **kwargs)

def tidepool_rust_binary(name, package_name, package_dir, version, env = {}, rustc_flags = [], **kwargs):
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    rust_binary(name = name, env = compiler_env, rustc_flags = flags, **kwargs)

def tidepool_rust_test(name, package_name, package_dir, version, env = {}, rustc_flags = [], **kwargs):
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    rust_test(name = name, env = compiler_env, rustc_flags = flags, **kwargs)
