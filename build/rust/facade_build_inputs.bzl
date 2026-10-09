def _tidepool_facade_build_inputs_impl(ctx):
    inputs = ctx.actions.symlinked_dir(
        "tidepool_facade_build_source_root",
        {
            "Cargo.toml": ctx.attrs.cargo_manifest,
            "build/native-workspace-gitlink.json": ctx.attrs.workspace_gitlink,
            "bridge/haskell": ctx.attrs.haskell_sources[DefaultInfo].default_outputs[0],
            "exomonad/examples/workspace": ctx.attrs.workspace_sources[DefaultInfo].default_outputs[0],
        },
        has_content_based_path = True,
    )
    return [DefaultInfo(default_output = inputs)]

tidepool_facade_build_inputs = rule(
    impl = _tidepool_facade_build_inputs_impl,
    attrs = {
        "cargo_manifest": attrs.source(),
        "workspace_gitlink": attrs.source(),
        "haskell_sources": attrs.dep(),
        "workspace_sources": attrs.dep(),
    },
)
