"""Schema-owned generated sources: one declared root and addressable files."""

def _protocol_generated_impl(ctx):
    root = ctx.actions.declare_output("generated", dir = True)
    command = cmd_args(ctx.attrs.generator[RunInfo], "--output-root", root.as_output())
    for path in ctx.attrs.outputs:
        command.add("--expect-output", path)
    ctx.actions.run(
        command,
        category = "protocol_generate",
    )
    sub_targets = {}
    for path in ctx.attrs.outputs:
        name = path.replace("/", "_").replace(".", "_")
        if name in sub_targets:
            fail("duplicate protocol subtarget: " + name)
        sub_targets[name] = [DefaultInfo(default_output = root.project(path))]
    return [DefaultInfo(default_output = root, sub_targets = sub_targets)]

protocol_generated = rule(
    impl = _protocol_generated_impl,
    attrs = {
        "generator": attrs.exec_dep(providers = [RunInfo]),
        "outputs": attrs.list(attrs.string()),
    },
)

def _snapshots_check_impl(ctx):
    stamp = ctx.actions.declare_output("protocol-snapshots.checked")
    ctx.actions.run(
        cmd_args([
            ctx.attrs.python[RunInfo], ctx.attrs.driver,
            "--generated-root", ctx.attrs.generated,
            "--source-root", ctx.attrs.snapshots,
            "--check", "--stamp", stamp.as_output(),
        ]),
        category = "protocol_snapshots_check",
    )
    return [DefaultInfo(default_output = stamp)]

protocol_snapshots_check = rule(
    impl = _snapshots_check_impl,
    attrs = {
        "generated": attrs.source(),
        "snapshots": attrs.source(),
        "driver": attrs.source(default = "//scripts:protocol_snapshots_script"),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
    },
)
