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

def tidepool_rust_test_cases(
        name,
        binary,
        exact_tests = [],
        expected_count = None,
        ignored = False,
        timeout = 300,
        jobs = None,
        env = {},
        resources = [],
        haskell_worker = False,
        run_env = {},
        test_rule_timeout_ms = None,
        visibility = []):
    """Wrap a shared Rust libtest binary in a counted, process-isolated sh_test."""
    if exact_tests and expected_count == None:
        fail("expected_count is required when exact_tests is set")
    if exact_tests and len(exact_tests) != len(set(exact_tests)):
        fail("exact_tests must not contain duplicate names")
    if exact_tests and len(exact_tests) != expected_count:
        fail("expected_count must match the number of exact_tests")
    if expected_count != None and expected_count <= 0:
        fail("expected_count must be positive")
    if timeout <= 0:
        fail("timeout must be positive")
    if jobs != None and jobs <= 0:
        fail("jobs must be positive")
    args = ["$(location " + binary + ")", "--timeout", str(timeout)]
    if jobs != None:
        args.extend(["--jobs", str(jobs)])
    for test in exact_tests:
        args.extend(["--exact", test])
    if expected_count != None:
        args.extend(["--expected-count", str(expected_count)])
    if ignored:
        args.append("--ignored")
    sh_test_kwargs = {
        "name": name,
        "test": "//build/rust:isolated_libtest",
        "args": args,
        "env": _test_environment(env, haskell_worker),
        "run_env": run_env,
        "resources": resources,
        "visibility": visibility,
    }
    if test_rule_timeout_ms != None:
        sh_test_kwargs["test_rule_timeout_ms"] = test_rule_timeout_ms
    sh_test(
        **sh_test_kwargs
    )


def tidepool_rust_isolated_test(
        name,
        package_name,
        package_dir,
        version,
        env = {},
        rustc_flags = [],
        haskell_worker = False,
        exact_tests = [],
        expected_count = None,
        ignored = False,
        timeout = 300,
        jobs = None,
        resources = [],
        run_env = {},
        test_rule_timeout_ms = None,
        **kwargs):
    # This compatibility macro owns one test binary; focused consumers can share
    # a binary by invoking tidepool_rust_test_cases directly.
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    rust_binary(
        name = name + "_binary",
        env = _test_environment(compiler_env, haskell_worker),
        rustc_flags = flags + ["--test"],
        **kwargs
    )
    tidepool_rust_test_cases(
        name = name,
        binary = ":" + name + "_binary",
        exact_tests = exact_tests,
        expected_count = expected_count,
        ignored = ignored,
        timeout = timeout,
        jobs = jobs,
        env = compiler_env,
        resources = resources,
        haskell_worker = haskell_worker,
        run_env = run_env,
        test_rule_timeout_ms = test_rule_timeout_ms,
        visibility = kwargs.get("visibility", []),
    )
