"""One declared compiler transaction for a module and its prepared targets."""

PreparedFixtureInfo = provider(fields = {
    "directory": "Portable prepared programs and metadata; no source-bound certificates.",
    "targets": "Requested target names mapped to prepared Artifact projections.",
})

def _output(dependency):
    outputs = dependency[DefaultInfo].default_outputs
    if len(outputs) != 1:
        fail("{} must expose exactly one default output".format(dependency.label))
    return outputs[0]

def _prepared_fixture_impl(ctx):
    targets = ctx.attrs.targets
    if not targets or len(targets) != len(set(targets)):
        fail("prepared fixtures require a nonempty distinct ordered target list")
    for target in targets:
        if not target or "," in target or "/" in target or "\\" in target or target in [".", ".."]:
            fail("invalid prepared fixture target: {}".format(target))
    ghc_libdir = read_root_config("nix", "ghc_libdir", "")
    action_path = read_root_config("nix", "action_path", "")
    if not ghc_libdir.startswith("/nix/store/") or not action_path:
        fail("configure the pinned production compiler/package closure before fixture compilation")
    output = ctx.actions.declare_output("prepared", dir = True)
    command = cmd_args([
        ctx.attrs.compiler[RunInfo],
        "--source", ctx.attrs.source,
        "--output", output.as_output(),
        "--frontend", _output(ctx.attrs.frontend),
        "--worker", _output(ctx.attrs.worker),
        "--deployment", _output(ctx.attrs.deployment),
        "--ghc-libdir", ghc_libdir,
        "--runtime-libraries", _output(ctx.attrs.runtime_libraries),
    ], hidden = [
        ctx.attrs.extra_inputs,
        ctx.attrs.ghc[RunInfo],
        ctx.attrs.frontend[RunInfo],
        ctx.attrs.worker[RunInfo],
    ])
    for tree, subdirectory in ctx.attrs.source_roots:
        if subdirectory.startswith("/") or any([part in [".", ".."] for part in subdirectory.split("/")]):
            fail("source root subtree must be a relative directory: {}".format(subdirectory))
        root = cmd_args(tree, format = "{}/" + subdirectory) if subdirectory else tree
        command.add("--include", root)
    for target in targets:
        command.add("--target", target)
    ctx.actions.run(
        command,
        category = "prepared_fixture",
        env = {"PATH": action_path},
    )
    programs = {target: output.project(target + ".prepared.cbor") for target in targets}
    return [
        DefaultInfo(
            default_output = output,
            sub_targets = {target: [DefaultInfo(default_output = artifact)] for target, artifact in programs.items()},
        ),
        PreparedFixtureInfo(directory = output, targets = programs),
    ]

tidepool_prepared_fixture = rule(
    impl = _prepared_fixture_impl,
    attrs = {
        "source": attrs.source(),
        # Each directory artifact is the complete declared readable source tree;
        # ordering determines GHC import selection and negative witnesses.
        "source_roots": attrs.list(attrs.tuple(attrs.source(), attrs.string()), default = []),
        "targets": attrs.list(attrs.string()),
        "extra_inputs": attrs.list(attrs.source(), default = []),
        "compiler": attrs.exec_dep(default = "//tidepool/toolchain:prepared-fixture", providers = [RunInfo]),
        "frontend": attrs.exec_dep(default = "//tidepool/extract-cmd:tidepool-extract", providers = [RunInfo]),
        "worker": attrs.exec_dep(default = "//bridge/haskell:tidepool_extract_bin", providers = [RunInfo]),
        "deployment": attrs.dep(default = "//build/package:compiler_deployment"),
        "runtime_libraries": attrs.dep(default = "//build/package:tidepool_extract_runtime_libraries"),
        "ghc": attrs.exec_dep(default = "toolchains//:ghc", providers = [RunInfo]),
    },
)
