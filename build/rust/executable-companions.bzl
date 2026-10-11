"""Stage one executable and its declared sibling executables as real files."""

def _executable_companions_impl(ctx):
    executable = ctx.attrs.executable[DefaultInfo].default_outputs
    if len(executable) != 1:
        fail("staged executable must have exactly one output")
    directory = ctx.actions.declare_output("bin", dir = True)
    primary = directory.project("test-bin")
    command = cmd_args(ctx.attrs.python[RunInfo], "-c", "import os, shutil, sys; args = sys.argv[1:]; [(os.makedirs(os.path.dirname(dst), exist_ok=True), shutil.copyfile(src, dst), os.chmod(dst, 0o755)) for src, dst in zip(args[::2], args[1::2])]")
    command.add(executable[0], primary.as_output())
    for name, target in ctx.attrs.companions.items():
        if not name or "/" in name or name in [".", "..", "test-bin"]:
            fail("companion must have a distinct sibling filename: " + name)
        inputs = target[DefaultInfo].default_outputs
        if len(inputs) != 1:
            fail("companion must have exactly one output: " + name)
        output = directory.project(name)
        command.add(inputs[0], output.as_output())
    ctx.actions.run(command, category = "stage_executable_companions")
    return [DefaultInfo(default_output = primary, other_outputs = [directory]), RunInfo(args = cmd_args(primary))]

executable_companions = rule(
    impl = _executable_companions_impl,
    attrs = {
        "executable": attrs.dep(),
        "companions": attrs.dict(attrs.string(), attrs.dep()),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
    },
)
