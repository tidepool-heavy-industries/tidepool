"""Stage one executable and its declared sibling executables as real files."""

def _executable_companions_impl(ctx):
    executable = ctx.attrs.executable[DefaultInfo].default_outputs
    if len(executable) != 1:
        fail("staged executable must have exactly one output")
    files = {"test-bin": executable[0]}
    for name, target in ctx.attrs.companions.items():
        if not name or "/" in name or name in [".", "..", "test-bin"]:
            fail("companion must have a distinct sibling filename: " + name)
        inputs = target[DefaultInfo].default_outputs
        if len(inputs) != 1:
            fail("companion must have exactly one output: " + name)
        files[name] = inputs[0]
    directory = ctx.actions.copied_dir("bin", files, executable_bit_override = True)
    primary = directory.project("test-bin")
    return [DefaultInfo(default_output = primary, other_outputs = [directory]), RunInfo(args = cmd_args(primary))]

executable_companions = rule(
    impl = _executable_companions_impl,
    attrs = {
        "executable": attrs.dep(),
        "companions": attrs.dict(attrs.string(), attrs.dep()),
    },
)
