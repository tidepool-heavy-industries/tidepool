"""Declared source obligations for one first-party Rust test compilation."""

def _test_source_requirements_impl(ctx):
    output = ctx.actions.write_json("test-source-requirements.json", {
        "version": 1,
        "target": str(ctx.label),
        "package": ctx.attrs.package,
        "package_dir": ctx.attrs.package_dir,
        "modules": ctx.attrs.modules,
        "fixtures": ctx.attrs.fixtures,
    })
    return [DefaultInfo(default_output = output)]

test_source_requirements = rule(
    impl = _test_source_requirements_impl,
    attrs = {
        "package": attrs.string(),
        "package_dir": attrs.string(),
        "modules": attrs.list(attrs.string()),
        "fixtures": attrs.list(attrs.string()),
    },
)
