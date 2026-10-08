load("@prelude//:rules.bzl", "rust_binary", "rust_library", "rust_test", "sh_test")
load("//build:native_profile.bzl", "rust_optimization_level")

def _common(name, package_name, package_dir, version, env, rustc_flags, is_library = False):
    manifest_env = {
        "CARGO_MANIFEST_DIR": package_dir,
        "CARGO_PKG_NAME": package_name,
        "CARGO_PKG_VERSION": version,
    }
    manifest_env.update(env)
    opt = rust_optimization_level(package_name, is_library)
    return (manifest_env, ["-Copt-level=" + opt] + rustc_flags)

def tidepool_rust_library(name, package_name, package_dir, version, env = {}, rustc_flags = [], **kwargs):
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags, is_library = True)
    rust_library(name = name, env = compiler_env, rustc_flags = flags, **kwargs)

def tidepool_rust_binary(name, package_name, package_dir, version, env = {}, rustc_flags = [], **kwargs):
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    if "--test" in rustc_flags:
        compiler_env = _test_regression_environment(compiler_env, package_dir, name)
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

def _test_regression_environment(env, package_dir, name):
    env = dict(env)
    if "TIDEPOOL_PROPTEST_REGRESSIONS" in env:
        fail("native regression path is owned by its declared test target")
    # Buck may replace Cargo's manifest directory with the source projection.
    # Retain replay seeds beside the declared package, outside that artifact.
    env["TIDEPOOL_PROPTEST_REGRESSIONS"] = package_dir + "/proptest-regressions/" + name + ".txt"
    return env

def tidepool_rust_test(name, package_name, package_dir, version, env = {}, rustc_flags = [], haskell_worker = False, **kwargs):
    env = _test_environment(env, haskell_worker)
    compiler_env, flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    compiler_env = _test_regression_environment(compiler_env, package_dir, name)
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
        resource_env = {},
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
    runtime_env = dict(env)
    resource_env = dict(resource_env)
    # The counted runner owns delegated admission and cleanup. These tools are
    # execution resources even when the shared libtest binary needs no host CLI.
    for key, executable in {
        "TIDEPOOL_TEST_SYSTEMD_RUN": "systemd-run",
        "TIDEPOOL_TEST_SYSTEMCTL": "systemctl",
    }.items():
        if key in runtime_env or key in resource_env:
            fail("delegated runner tools are owned by tidepool_rust_test_cases: " + key)
        resource_env[key] = "$(location toolchains//:exomonad_runtime_tools)/bin/" + executable
    resources = list(resources)
    if "toolchains//:exomonad_runtime_tools" not in resources:
        resources.append("toolchains//:exomonad_runtime_tools")
    for key, path in resource_env.items():
        if key in runtime_env:
            fail("resource_env must not duplicate ordinary env: " + key)
        runtime_env[key] = path
        args.extend(["--resource-env", key])
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
        "env": _test_environment(runtime_env, haskell_worker),
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
        resource_env = {},
        compile_env = {},
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
    # Runtime resources stay out of the compile action: changing a prepared
    # fixture rebuilds its producer and test execution without relinking Rust.
    compiler_env, flags = _common(name, package_name, package_dir, version, compile_env, rustc_flags)
    compiler_env = _test_regression_environment(compiler_env, package_dir, name)
    runtime_env, _flags = _common(name, package_name, package_dir, version, env, rustc_flags)
    rust_binary(
        name = name + "_binary",
        env = compiler_env,
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
        env = runtime_env,
        resource_env = resource_env,
        resources = resources,
        haskell_worker = haskell_worker,
        run_env = run_env,
        test_rule_timeout_ms = test_rule_timeout_ms,
        visibility = kwargs.get("visibility", []),
    )
