"""Declared formatting and strict Clippy checks on the native first-party graph."""
load("@prelude//rust:outputs.bzl", "RustcExtraOutputsInfo")

def _checks_impl(ctx):
    proof = ctx.actions.declare_output("proof.json")
    command = cmd_args([ctx.attrs.python[RunInfo], ctx.attrs.driver,
                        "--rustfmt", ctx.attrs.rustfmt[RunInfo], "--output", proof.as_output()])
    for source in ctx.attrs.sources:
        outputs = source[DefaultInfo].default_outputs
        if len(outputs) != 1:
            fail("formatting source groups must expose one tree")
        command.add("--source-tree", outputs[0])
    for target in ctx.attrs.targets:
        command.add("--diagnostic", str(target.label), target[RustcExtraOutputsInfo].clippy.compile_output.diag_json)
    ctx.actions.run(command, category = "native_rust_checks", env = {"PATH": read_root_config("nix", "action_path")})
    return [DefaultInfo(default_output = proof)]

native_rust_checks = rule(
    impl = _checks_impl,
    attrs = {
        "sources": attrs.list(attrs.dep()),
        "targets": attrs.list(attrs.dep(providers = [RustcExtraOutputsInfo])),
        "driver": attrs.source(default = "//build/rust:native_checks_driver"),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
        "rustfmt": attrs.exec_dep(default = "toolchains//:rustfmt", providers = [RunInfo]),
    },
)
