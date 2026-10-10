"""Compile contract controls/refusals against declared native Rust metadata."""

load("@prelude//rust:build_params.bzl", "MetadataKind")
load("@prelude//rust:link_info.bzl", "DEFAULT_STATIC_LINK_STRATEGY", "RustLinkInfo", "RustProcMacroPlugin", "get_available_proc_macros", "strategy_info")
load("@prelude//rust:rust_toolchain.bzl", "RustToolchainInfo")

def _compile_fail_impl(ctx):
    toolchain = ctx.attrs._rust_toolchain[RustToolchainInfo]
    direct = {}
    transitive = {}
    proc_macros = set()
    for alias, dependency in ctx.attrs.dependencies.items():
        info = dependency[RustLinkInfo]
        strategy = strategy_info(toolchain, info, DEFAULT_STATIC_LINK_STRATEGY)
        proc_macros.update(strategy.transitive_proc_macro_deps)
        direct[alias] = strategy.outputs[MetadataKind("full")]
        for artifact in strategy.transitive_deps[MetadataKind("full")].traverse():
            transitive[artifact.artifact.short_path] = {
                "artifact": artifact.artifact,
                "crate": artifact.crate.simple,
                "dynamic_crate": artifact.crate.dynamic,
            }
    available_proc_macros = get_available_proc_macros(ctx)
    for marker in proc_macros:
        info = available_proc_macros[marker.label][RustLinkInfo]
        strategy = strategy_info(toolchain, info, DEFAULT_STATIC_LINK_STRATEGY)
        artifact = strategy.outputs[MetadataKind("full")]
        transitive[artifact.short_path] = {
            "artifact": artifact,
            "crate": info.crate.simple,
            "dynamic_crate": info.crate.dynamic,
        }
    inputs = ctx.actions.write_json("inputs.json", {
        # RustToolchainInfo.compiler is RunInfo, not a bare executable. Keep
        # its argument vector structured: Nix toolchains may wrap rustc in a
        # source-participation launcher whose arguments are part of the
        # compiler command.
        "rustc_command": toolchain.compiler.args,
        "direct": direct,
        "transitive": transitive.values(),
    }, with_inputs = True)
    output = ctx.actions.declare_output("compile-proof", dir = True)
    ctx.actions.run(cmd_args([
        ctx.attrs._python[RunInfo], ctx.attrs._runner,
        "--inputs", inputs,
        "--control", ctx.attrs.control,
        "--source", ctx.attrs.source,
        "--expected", ctx.attrs.expected,
        "--edition", ctx.attrs.edition,
        "--output", output.as_output(),
    ], hidden = [toolchain.compiler]), category = "rust_compile_fail", identifier = ctx.label.name)
    return [
        DefaultInfo(default_output = output),
        ExternalRunnerTestInfo(
            type = "custom",
            command = [ctx.attrs._python[RunInfo], ctx.attrs._runner,
                       "--verify", cmd_args(output, format = "{}/proof.json")],
        ),
    ]

rust_compile_fail = rule(
    impl = _compile_fail_impl,
    uses_plugins = [RustProcMacroPlugin],
    attrs = {
        "source": attrs.source(),
        "control": attrs.source(),
        "expected": attrs.source(),
        "edition": attrs.string(default = "2021"),
        "dependencies": attrs.dict(attrs.string(), attrs.dep(providers = [RustLinkInfo], pulls_plugins = [RustProcMacroPlugin])),
        "_rust_toolchain": attrs.toolchain_dep(default = "toolchains//:rust"),
        "_python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
        "_runner": attrs.source(default = "//build/rust:compile_fail_runner"),
    },
)
