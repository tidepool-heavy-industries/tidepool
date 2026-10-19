def _tidepool_execution_platforms(ctx):
    remote = ctx.attrs.remote
    local = ctx.attrs.local
    cache = ctx.attrs.cache
    if not local and not remote:
        fail("an execution platform must enable local or remote execution")
    if remote and not ctx.attrs.toolchain:
        fail("remote Buck execution requires the toolchain property in .buckconfig.local")
    properties = {"OSFamily": "linux"}
    if remote:
        properties["toolchain"] = ctx.attrs.toolchain
    configuration = ctx.attrs.host_platform[PlatformInfo].configuration
    platform = ExecutionPlatformInfo(
        label = ctx.label.raw_target(),
        configuration = configuration,
        executor_config = CommandExecutorConfig(
            local_enabled = local,
            remote_enabled = remote,
            remote_cache_enabled = cache,
            allow_cache_uploads = cache,
            remote_execution_properties = properties,
            remote_execution_use_case = "buck2-default",
            remote_output_paths = "output_paths",
        ),
    )
    return [
        DefaultInfo(),
        platform,
        PlatformInfo(label = str(ctx.label.raw_target()), configuration = configuration),
        ExecutionPlatformRegistrationInfo(platforms = [platform]),
    ]

tidepool_execution_platforms = rule(
    impl = _tidepool_execution_platforms,
    attrs = {
        "host_platform": attrs.dep(providers = [PlatformInfo]),
        "cache": attrs.bool(),
        "local": attrs.bool(),
        "remote": attrs.bool(),
        "toolchain": attrs.string(),
    },
)
