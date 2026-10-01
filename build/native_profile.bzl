"""Shared first-party native compiler profiles selected through Buck config."""

_PROFILES = ["fast-dev", "debug", "production"]

def selected_native_profile():
    profile = read_root_config("tidepool", "profile", "fast-dev")
    if profile not in _PROFILES:
        fail("unknown Tidepool native profile %r; expected one of %s" % (profile, ", ".join(_PROFILES)))
    return profile

def rust_optimization_level(target_name, hot_crates):
    profile = selected_native_profile()
    if profile == "production":
        return "3"
    if profile == "debug":
        return "0"
    return "3" if target_name in hot_crates else "0"

def haskell_optimization_flags():
    return ["-O2"] if selected_native_profile() == "production" else []
