"""Capture actual declared source artifacts without adding Rust compile inputs."""

def _snapshot_impl(ctx):
    output = ctx.actions.declare_output("source-inputs", dir = True)
    command = cmd_args([ctx.attrs.python[RunInfo], ctx.attrs.driver, "--output", output.as_output()])
    for tree, prefix in ctx.attrs.trees:
        outputs = tree[DefaultInfo].default_outputs
        if len(outputs) != 1:
            fail("qualification source input must expose one directory")
        command.add("--tree", prefix, outputs[0])
    ctx.actions.run(command, category = "native_qualification_sources")
    return [DefaultInfo(default_output = output)]

native_source_snapshot = rule(
    impl = _snapshot_impl,
    attrs = {
        "trees": attrs.list(attrs.tuple(attrs.dep(), attrs.string())),
        "driver": attrs.source(default = "//build/rust:source_snapshot_driver"),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
    },
)
