"""Reindeer compiler policy; Cargo still owns platform and build-script handling."""
load("//build:native_profile.bzl", "GENERATED_RUST_POLICY_ENV", "generated_rust_policy_environment", "rust_optimization_flags")
load("@prelude//rust:cargo_package.bzl", "cargo")

def _arguments(env, rustc_flags, platform):
    # This Reindeer fixup is build-policy metadata, never compiler/runtime env.
    env, level = generated_rust_policy_environment(env)
    for attributes in platform.values():
        if GENERATED_RUST_POLICY_ENV in attributes.get("env", {}):
            fail("generated Rust policy belongs to the package fixup, not platform env")
        rust_optimization_flags(level, attributes.get("rustc_flags", []))
    return (env, rust_optimization_flags(level, rustc_flags))

def tidepool_cargo_rust_library(env = {}, rustc_flags = [], platform = {}, **kwargs):
    compiler_env, flags = _arguments(env, rustc_flags, platform)
    cargo.rust_library(env = compiler_env, rustc_flags = flags, platform = platform, **kwargs)

def tidepool_cargo_rust_binary(env = {}, rustc_flags = [], platform = {}, **kwargs):
    compiler_env, flags = _arguments(env, rustc_flags, platform)
    cargo.rust_binary(env = compiler_env, rustc_flags = flags, platform = platform, **kwargs)
