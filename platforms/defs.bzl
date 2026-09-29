def _tidepool_execution_platforms(ctx):
    remote = ctx.attrs.remote
    if remote and not ctx.attrs.toolchain:
        fail("remote Buck execution requires the toolchain property in .buckconfig.local")
    properties = {"OSFamily": "linux"}
    if remote:
        properties["toolchain"] = ctx.attrs.toolchain
    platform = ExecutionPlatformInfo(
        label = ctx.label.raw_target(),
        configuration = ConfigurationInfo(constraints = {}, values = {}),
        executor_config = CommandExecutorConfig(
            local_enabled = not remote,
            remote_enabled = remote,
            remote_execution_properties = properties,
            remote_execution_use_case = "buck2-default",
            remote_output_paths = "output_paths",
        ),
    )
    return [DefaultInfo(), ExecutionPlatformRegistrationInfo(platforms = [platform])]

tidepool_execution_platforms = rule(
    impl = _tidepool_execution_platforms,
    attrs = {
        "remote": attrs.bool(),
        "toolchain": attrs.string(),
    },
)
