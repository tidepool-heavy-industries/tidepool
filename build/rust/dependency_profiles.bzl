"""Reindeer compiler policy; Cargo still owns platform and build-script handling."""
load("//build:native_profile.bzl", "generated_rust_optimization_level", "rust_optimization_flags")
load("@prelude//rust:cargo_package.bzl", "cargo")

_POLICY_ENV = "TIDEPOOL_NATIVE_RUST_POLICY"

def _arguments(env, rustc_flags, platform):
    env = dict(env)
    # This Reindeer fixup is build-policy metadata, never compiler/runtime env.
    policy = env.pop(_POLICY_ENV, "dependency")
    level = generated_rust_optimization_level(policy)
    for attributes in platform.values():
        if _POLICY_ENV in attributes.get("env", {}):
            fail("generated Rust policy belongs to the package fixup, not platform env")
        rust_optimization_flags(level, attributes.get("rustc_flags", []))
    return (env, rust_optimization_flags(level, rustc_flags))

def tidepool_cargo_rust_library(env = {}, rustc_flags = [], platform = {}, **kwargs):
    compiler_env, flags = _arguments(env, rustc_flags, platform)
    cargo.rust_library(env = compiler_env, rustc_flags = flags, platform = platform, **kwargs)

def tidepool_cargo_rust_binary(env = {}, rustc_flags = [], platform = {}, **kwargs):
    compiler_env, flags = _arguments(env, rustc_flags, platform)
    cargo.rust_binary(env = compiler_env, rustc_flags = flags, platform = platform, **kwargs)
