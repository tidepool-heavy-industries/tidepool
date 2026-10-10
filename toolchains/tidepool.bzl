load("@prelude//haskell:toolchain.bzl", "HaskellPlatformInfo", "HaskellToolchainInfo")
load("@prelude//rust:rust_toolchain.bzl", "PanicRuntime", "RustToolchainInfo")

def _nix_rust_toolchain_impl(ctx):
    return [
        DefaultInfo(),
        RustToolchainInfo(
            compiler = RunInfo(args = [ctx.attrs.python, ctx.attrs.test_source_wrapper,
                                      "--rustc", ctx.attrs.rustc, "--"]),
            rustdoc = RunInfo(args = [ctx.attrs.rustdoc]),
            clippy_driver = RunInfo(args = [ctx.attrs.clippy] + ctx.attrs.clippy_flags),
            default_edition = "2021",
            panic_runtime = PanicRuntime("unwind"),
            rustc_target_triple = "x86_64-unknown-linux-gnu",
            rustc_flags = ["-Cforce-frame-pointers=yes", "-Copt-level=2", "-Clinker=" + ctx.attrs.linker],
            nightly_features = False,
            clippy_toml = ctx.attrs.clippy_toml,
        ),
    ]

nix_rust_toolchain = rule(
    impl = _nix_rust_toolchain_impl,
    attrs = {
        "rustc": attrs.string(),
        "python": attrs.string(),
        "test_source_wrapper": attrs.source(),
        "rustdoc": attrs.string(),
        "clippy": attrs.string(),
        "linker": attrs.string(),
        "clippy_toml": attrs.source(),
        "clippy_flags": attrs.list(attrs.string(), default = []),
    },
    is_toolchain_rule = True,
)

def _nix_haskell_toolchain_impl(ctx):
    compiler = ctx.attrs.ghc
    if not compiler:
        fail("{} requires its pinned GHC closure; materialize the declared Nix output and configure Buck (--tests for host test targets)".format(ctx.label))
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
    if not ctx.attrs.executable:
        fail("{} requires its pinned executable; rerun scripts/buck2-configure.sh with the required selection (--browser for browser targets, --tests for host Haskell tests)".format(ctx.label))
    return [
        DefaultInfo(),
        RunInfo(args = [ctx.attrs.executable]),
    ]

nix_tool = rule(
    impl = _nix_tool_impl,
    attrs = {"executable": attrs.string()},
)


def _nix_directory_impl(ctx):
    path = ctx.attrs.store_path
    if not path.startswith("/nix/store/") or any([part in ["", ".", ".."] for part in path.split("/")[1:]]):
        fail("{} requires an immutable Nix store directory; materialize its pinned package output and rerun scripts/buck2-configure.sh".format(ctx.label))
    output = ctx.actions.declare_output(ctx.label.name, dir = True)
    ctx.actions.run(
        cmd_args([ctx.attrs.cp, "-a", ctx.attrs.store_path + "/.", output.as_output()]),
        category = "nix_directory",
    )
    sub_targets = {}
    for source in ctx.attrs.projected_sources:
        if source.startswith("/") or any([part in ["", ".", ".."] for part in source.split("/")]):
            fail("{} has an invalid source projection: {}".format(ctx.label, source))
        name = source.replace("/", "_").replace(".", "_").replace("-", "_")
        if name in sub_targets:
            fail("{} has duplicate source projection: {}".format(ctx.label, name))
        sub_targets[name] = [DefaultInfo(default_output = output.project(source))]
    return [DefaultInfo(default_output = output, sub_targets = sub_targets)]

nix_directory = rule(
    impl = _nix_directory_impl,
    attrs = {
        "cp": attrs.string(),
        "store_path": attrs.string(),
        "projected_sources": attrs.list(attrs.string(), default = []),
    },
)

def _native_catalog_retention_impl(ctx):
    if not ctx.attrs.record:
        fail("select retained native catalog source evidence before building the catalog")
    record = json.decode(ctx.attrs.record)
    if type(record) != "dict" or record.get("kind") != "native-catalog-source-retention":
        fail("native catalog retention must be the qualification owner's exact JSON record")
    output = ctx.actions.write_json("retained-catalog-sources.json", record)
    return [DefaultInfo(default_output = output)]

# The complete record bytes are an action input. Its original evidence pathname
# alone is never treated as the declared retention record.
native_catalog_retention = rule(
    impl = _native_catalog_retention_impl,
    attrs = {"record": attrs.string()},
)


def _checked_harness_source_impl(ctx):
    if bool(ctx.attrs.source) == bool(ctx.attrs.store_path):
        fail("checked_harness_source requires exactly one Git source or immutable Nix store path")
    if ctx.attrs.store_path:
        path = ctx.attrs.store_path
        if not path.startswith("/nix/store/") or any([part in ["", ".", ".."] for part in path.split("/")[1:]]):
            fail("checked_harness_source requires an immutable Nix store path")
        source = path + "/."
    else:
        source = cmd_args(ctx.attrs.source, format = "{}/.")
    revision = ctx.actions.declare_output("harness-source-revision.txt")
    ctx.actions.run(
        cmd_args([
            ctx.attrs.python,
            ctx.attrs.provenance_script,
            "--generated-source-revision", ctx.attrs.revision,
            "--generated-source-nar-hash", ctx.attrs.nar_hash,
            "--cargo-lock", ctx.attrs.cargo_lock,
            "--flake-lock", ctx.attrs.flake_lock,
            "--output", revision.as_output(),
        ]),
        category = "harness_source_revision",
    )
    output = ctx.actions.declare_output(ctx.label.name, dir = True)
    ctx.actions.run(
        cmd_args([ctx.attrs.cp, "-a", source, output.as_output()], hidden = [revision]),
        category = "harness_source",
    )
    return [DefaultInfo(
        default_output = output,
        sub_targets = {"revision": [DefaultInfo(default_output = revision)]},
    )]

checked_harness_source = rule(
    impl = _checked_harness_source_impl,
    attrs = {
        "cp": attrs.string(),
        "python": attrs.string(),
        "store_path": attrs.string(default = ""),
        "source": attrs.option(attrs.source(), default = None),
        "revision": attrs.string(),
        "nar_hash": attrs.string(),
        "cargo_lock": attrs.source(),
        "flake_lock": attrs.source(),
        "provenance_script": attrs.source(),
    },
)
