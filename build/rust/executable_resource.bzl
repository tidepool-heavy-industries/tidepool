"""A declared command resource; execution adapters do not forward test evidence."""

def _runtime_executable_impl(ctx):
    executable = ctx.attrs.executable
    outputs = executable[DefaultInfo]
    return [
        DefaultInfo(default_outputs = outputs.default_outputs, other_outputs = outputs.other_outputs),
        executable[RunInfo],
    ]

runtime_executable = rule(
    impl = _runtime_executable_impl,
    attrs = {"executable": attrs.dep(providers = [RunInfo])},
)
