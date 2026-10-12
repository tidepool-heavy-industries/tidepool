load("//build:native_profile.bzl", "GENERATED_RUST_POLICY_ENV", "generated_rust_policy_environment", "rust_optimization_level", "selected_native_profile")
load("@prelude//rust:cargo_buildscript.bzl", "buildscript_run")

def tidepool_buildscript_run(first_party = False, **kwargs):
    env = dict(kwargs.pop("env", {}))
    if first_party:
        if GENERATED_RUST_POLICY_ENV in env:
            fail("build script has conflicting first-party and Reindeer policies")
        optimization = rust_optimization_level(kwargs["package_name"], True)
    else:
        env, optimization = generated_rust_policy_environment(env)
    env["PATH"] = read_root_config("nix", "action_path")
    env["HOST"] = "x86_64-unknown-linux-gnu"
    env["OPT_LEVEL"] = optimization
    # Current native rules do not request debuginfo in any profile.
    env["DEBUG"] = "false"
    env["PROFILE"] = "release" if selected_native_profile() == "production" else "dev"
    buildscript_run(env = env, **kwargs)
