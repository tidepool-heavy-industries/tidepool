"""Compile contract controls/refusals against declared native Rust metadata."""

load("@prelude//rust:build_params.bzl", "MetadataKind")
load("@prelude//rust:link_info.bzl", "DEFAULT_STATIC_LINK_STRATEGY", "RustLinkInfo", "strategy_info")
load("@prelude//rust:rust_toolchain.bzl", "RustToolchainInfo")

def _compile_fail_impl(ctx):
    toolchain = ctx.attrs._rust_toolchain[RustToolchainInfo]
    direct = {}
    transitive = {}
    for alias, dependency in ctx.attrs.dependencies.items():
        info = dependency[RustLinkInfo]
        strategy = strategy_info(toolchain, info, DEFAULT_STATIC_LINK_STRATEGY)
        direct[alias] = strategy.outputs[MetadataKind("full")]
        for artifact in strategy.transitive_deps[MetadataKind("full")].traverse():
            transitive[artifact.artifact.short_path] = {
                "artifact": artifact.artifact,
                "crate": artifact.crate.simple,
                "dynamic_crate": artifact.crate.dynamic,
            }
    inputs = ctx.actions.write_json("inputs.json", {
        "direct": direct,
        "transitive": transitive.values(),
    }, with_inputs = True)
    output = ctx.actions.declare_output("compile-proof", dir = True)
    ctx.actions.run(cmd_args([
        ctx.attrs._python[RunInfo], ctx.attrs._runner,
        "--rustc", toolchain.compiler,
        "--inputs", inputs,
        "--control", ctx.attrs.control,
        "--source", ctx.attrs.source,
        "--expected", ctx.attrs.expected,
        "--edition", ctx.attrs.edition,
        "--output", output.as_output(),
    ]), category = "rust_compile_fail", identifier = ctx.label.name)
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
    attrs = {
        "source": attrs.source(),
        "control": attrs.source(),
        "expected": attrs.source(),
        "edition": attrs.string(default = "2021"),
        "dependencies": attrs.dict(attrs.string(), attrs.dep(providers = [RustLinkInfo])),
        "_rust_toolchain": attrs.toolchain_dep(default = "toolchains//:rust"),
        "_python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
        "_runner": attrs.source(default = "//build/rust:compile_fail_runner"),
    },
)
