"""Retain the owning optimizer-probe check as a declared native GHC action."""

def _probe_impl(ctx):
    output = ctx.actions.declare_output("probes", dir = True)
    command = cmd_args([
        ctx.attrs.python[RunInfo], ctx.attrs.driver,
        "--ghc", ctx.attrs.ghc[RunInfo],
        "--checker", ctx.attrs.checker,
        "--manifest", ctx.attrs.manifest,
        "--output", output.as_output(),
    ])
    for module, source in ctx.attrs.sources.items():
        command.add("--source", module, source)
    ctx.actions.run(command, category = "probe_opacity", env = {"PATH": read_root_config("nix", "action_path")})
    return [DefaultInfo(default_output = output)]

native_probe_opacity = rule(
    impl = _probe_impl,
    attrs = {
        "sources": attrs.dict(attrs.string(), attrs.source()),
        "manifest": attrs.source(),
        "ghc": attrs.exec_dep(default = "toolchains//:ghc", providers = [RunInfo]),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
        "driver": attrs.source(default = "//build/haskell:probe_opacity_driver"),
        "checker": attrs.source(default = "//scripts:probe_opacity_checker"),
    },
)
