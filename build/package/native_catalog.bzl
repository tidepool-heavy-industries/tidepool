"""Declared sources for the fixed native support cohort."""

def _catalog_sources_impl(ctx):
    cohort = ctx.actions.write_json("cohort.json", {
        "component": ctx.attrs.component,
        "modules": ctx.attrs.modules,
    })
    output = ctx.actions.declare_output("catalog-sources", dir = True)
    ctx.actions.run(
        cmd_args([
            ctx.attrs.python[RunInfo], ctx.attrs.qualification,
            "snapshot-sources",
            "--sources", ctx.attrs.sources,
            "--effects", ctx.attrs.effects,
            "--cohort", cohort,
            "--output", output.as_output(),
        ]),
        category = "native_catalog_sources",
    )
    return [DefaultInfo(default_output = output)]

_catalog_sources = rule(
    impl = _catalog_sources_impl,
    attrs = {
        "component": attrs.string(),
        "modules": attrs.dict(attrs.string(), attrs.string()),
        "sources": attrs.source(),
        "effects": attrs.source(),
        "qualification": attrs.source(default = "//build/package:qualification_script"),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
    },
)

def native_catalog_sources(name, cohort, **kwargs):
    _catalog_sources(name = name, component = cohort["component"], modules = cohort["modules"], **kwargs)
