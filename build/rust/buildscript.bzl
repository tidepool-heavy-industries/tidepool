load("@prelude//rust:cargo_buildscript.bzl", "buildscript_run")

def tidepool_buildscript_run(**kwargs):
    env = dict(kwargs.pop("env", {}))
    env["PATH"] = read_root_config("nix", "action_path")
    env["HOST"] = "x86_64-unknown-linux-gnu"
    env["OPT_LEVEL"] = "2"
    env["DEBUG"] = "false"
    env["PROFILE"] = "dev"
    buildscript_run(env = env, **kwargs)
