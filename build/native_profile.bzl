"""Shared native compiler profiles selected through Buck config."""

_PROFILES = ["fast-dev", "debug", "production"]
_HOT_RUST_PACKAGES = ["tidepool-codegen", "tidepool-repr", "tidepool-heap", "tidepool-toolchain"]

def selected_native_profile():
    profile = read_root_config("tidepool", "profile", "fast-dev")
    if profile not in _PROFILES:
        fail("unknown Tidepool native profile %r; expected one of %s" % (profile, ", ".join(_PROFILES)))
    return profile

def rust_optimization_level(package_name, is_library):
    # Fast-dev reuses optimized hot libraries. Test harnesses, including
    # engine unit tests that compile their owning code again, stay at O0.
    profile = selected_native_profile()
    if profile == "production":
        return "3"
    if profile == "debug":
        return "0"
    return "3" if is_library and package_name in _HOT_RUST_PACKAGES else "0"

def haskell_optimization_flags():
    return ["-O2"] if selected_native_profile() == "production" else []


def rust_optimization_flags(level, flags):
    for flag in flags:
        if flag == "-O" or flag.startswith("-Copt-level") or flag.startswith("opt-level"):
            fail("optimization belongs to native profile policy, not rustc_flags")
    return ["-Copt-level=" + level] + list(flags)

def generated_rust_optimization_level(policy):
    profile = selected_native_profile()
    if policy == "harness":
        return "2" if profile == "production" else "0"
    if policy == "dependency":
        return "0" if profile == "debug" else "2"
    fail("unknown generated Rust policy %r" % policy)
