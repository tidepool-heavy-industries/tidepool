load("@prelude//haskell:toolchain.bzl", "HaskellPlatformInfo", "HaskellToolchainInfo")
load("@prelude//rust:rust_toolchain.bzl", "PanicRuntime", "RustToolchainInfo")

def _nix_rust_toolchain_impl(ctx):
    return [
        DefaultInfo(),
        RustToolchainInfo(
            compiler = RunInfo(args = [ctx.attrs.rustc]),
            rustdoc = RunInfo(args = [ctx.attrs.rustdoc]),
            clippy_driver = RunInfo(args = [ctx.attrs.clippy]),
            default_edition = "2021",
            panic_runtime = PanicRuntime("unwind"),
            rustc_target_triple = "x86_64-unknown-linux-gnu",
            rustc_flags = ["-Cforce-frame-pointers=yes", "-Copt-level=2", "-Clinker=" + ctx.attrs.linker],
            nightly_features = False,
        ),
    ]

nix_rust_toolchain = rule(
    impl = _nix_rust_toolchain_impl,
    attrs = {
        "rustc": attrs.string(),
        "rustdoc": attrs.string(),
        "clippy": attrs.string(),
        "linker": attrs.string(),
    },
    is_toolchain_rule = True,
)

def _nix_haskell_toolchain_impl(ctx):
    compiler = ctx.attrs.ghc
    if ctx.attrs.extra_compile_inputs:
        # Prelude's compile action wraps this value in cmd_args, so hidden
        # artifacts become declared inputs without becoming GHC arguments.
        compiler = cmd_args(
            compiler,
            hidden = ctx.attrs.extra_compile_inputs,
        )
    return [
        DefaultInfo(),
        HaskellToolchainInfo(
            compiler = compiler,
            linker = ctx.attrs.ghc,
            packager = ctx.attrs.ghc_pkg,
            haddock = ctx.attrs.haddock,
            compiler_flags = ["-fwrite-if-simplified-core", "-fexpose-all-unfoldings"],
            linker_flags = [],
        ),
        HaskellPlatformInfo(name = "x86_64"),
    ]

nix_haskell_toolchain = rule(
    impl = _nix_haskell_toolchain_impl,
    attrs = {
        "ghc": attrs.string(),
        "ghc_pkg": attrs.string(),
        "haddock": attrs.string(),
        "extra_compile_inputs": attrs.list(attrs.source(), default = []),
    },
    is_toolchain_rule = True,
)

def _nix_tool_impl(ctx):
    return [
        DefaultInfo(),
        RunInfo(args = [ctx.attrs.executable]),
    ]

nix_tool = rule(
    impl = _nix_tool_impl,
    attrs = {"executable": attrs.string()},
)


def _nix_directory_impl(ctx):
    output = ctx.actions.declare_output(ctx.label.name, dir = True)
    ctx.actions.run(
        cmd_args([ctx.attrs.cp, "-a", ctx.attrs.store_path + "/.", output.as_output()]),
        category = "nix_directory",
    )
    return [DefaultInfo(default_output = output)]

nix_directory = rule(
    impl = _nix_directory_impl,
    attrs = {
        "cp": attrs.string(),
        "store_path": attrs.string(),
    },
)
