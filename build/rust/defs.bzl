load("@prelude//:rules.bzl", "rust_binary", "rust_library", "rust_test", "sh_test")

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

def _test_environment(env, haskell_worker):
    env = dict(env)
    if haskell_worker:
        ghc_bin = read_root_config("nix", "ghc_bin")
        ghc_libdir = read_root_config("nix", "ghc_libdir")
        if not ghc_bin or not ghc_libdir:
            fail("configure Buck with the pinned Nix GHC before building a Haskell-worker test")
        env["TIDEPOOL_GHC_LIBDIR"] = ghc_libdir
        env["PATH"] = read_root_config("nix", "action_path") + ":" + ghc_bin
    return env

def tidepool_rust_test(name, package_name, package_dir, version, env = {}, rustc_flags = [], haskell_worker = False, **kwargs):
    env = _test_environment(env, haskell_worker)
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    rust_test(name = name, env = compiler_env, rustc_flags = flags, **kwargs)

def tidepool_rust_isolated_test(name, package_name, package_dir, version, env = {}, rustc_flags = [], haskell_worker = False, **kwargs):
    # A build-only harness avoids exposing a second, unisolated test target.
    env = _test_environment(env, haskell_worker)
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    rust_binary(
        name = name + "_binary",
        env = compiler_env,
        rustc_flags = flags + ["--test"],
        **kwargs
    )
    sh_test(
        name = name,
        test = "//build/rust:isolated_libtest",
        args = ["$(location :" + name + "_binary)"],
        env = compiler_env,
        visibility = kwargs.get("visibility", []),
    )
