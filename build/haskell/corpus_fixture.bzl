load("@prelude//:rules.bzl", "sh_test")
"""Declared corpus compilation and native Suite oracle issuance."""

def _output(dependency):
    outputs = dependency[DefaultInfo].default_outputs
    if len(outputs) != 1:
        fail("{} must expose one default artifact".format(dependency.label))
    return outputs[0]

def _roots(command, roots):
    for tree, subdirectory in _checked_roots(roots):
        command.add("--include", cmd_args(tree, format = "{}/" + subdirectory) if subdirectory else tree)

def _checked_roots(roots):
    for _tree, subdirectory in roots:
        if subdirectory.startswith("/") or any([part in [".", ".."] for part in subdirectory.split("/")]):
            fail("source subtree must be a relative directory")
    return roots

PreparedCorpusInfo = provider(fields = {
    "directory": Artifact,
    "manifest": Artifact,
    "metadata": Artifact,
    "dependencies": Artifact,
    "inventory": Artifact,
})

def _corpus_impl(ctx):
    output = ctx.actions.declare_output("corpus", dir = True)
    libdir = read_root_config("nix", "ghc_libdir", "")
    if not libdir.startswith("/nix/store/"):
        fail("corpus producer requires the pinned production GHC package closure")
    command = cmd_args([
        ctx.attrs.python[RunInfo], ctx.attrs.driver,
        "--producer", _output(ctx.attrs.producer),
        "--validator", _output(ctx.attrs.validator),
        "--source", ctx.attrs.source,
        "--module", ctx.attrs.module,
        "--targets", ctx.attrs.targets_file,
        "--output", output.as_output(),
        "--ghc-libdir", libdir,
        "--runtime-libraries", _output(ctx.attrs.runtime_libraries),
    ], hidden = [
        ctx.attrs.extra_inputs,
        ctx.attrs.ghc[RunInfo],
        ctx.attrs.producer[RunInfo],
        ctx.attrs.validator[RunInfo],
    ])
    _roots(command, ctx.attrs.source_roots)
    if ctx.attrs.all_tops:
        command.add("--all-tops")
    for target in ctx.attrs.metadata_targets:
        command.add("--metadata-target", target)
    ctx.actions.run(command, category = "prepared_corpus", env = {"PATH": read_root_config("nix", "action_path")})
    manifest = output.project("manifest.json")
    metadata = output.project("meta.cbor")
    dependencies = output.project("dependencies.json")
    inventory = output.project("diagnostic-inventory.json")
    return [
        DefaultInfo(default_output = output, sub_targets = {
            "manifest": [DefaultInfo(default_output = manifest)],
            "metadata": [DefaultInfo(default_output = metadata)],
            "dependencies": [DefaultInfo(default_output = dependencies)],
            "inventory": [DefaultInfo(default_output = inventory)],
        }),
        PreparedCorpusInfo(directory = output, manifest = manifest, metadata = metadata, dependencies = dependencies, inventory = inventory),
    ]

tidepool_prepared_corpus = rule(
    impl = _corpus_impl,
    attrs = {
        "source": attrs.source(),
        "module": attrs.string(),
        "targets_file": attrs.source(),
        "source_roots": attrs.list(attrs.tuple(attrs.source(), attrs.string())),
        "extra_inputs": attrs.list(attrs.source(), default = []),
        "all_tops": attrs.bool(default = False),
        "metadata_targets": attrs.list(attrs.string(), default = []),
        "producer": attrs.exec_dep(default = "//bridge/haskell:execution_corpus_producer", providers = [RunInfo]),
        "validator": attrs.exec_dep(default = "//tidepool/prepared-corpus:prepared-corpus", providers = [RunInfo]),
        "runtime_libraries": attrs.dep(default = "//build/package:tidepool_extract_runtime_libraries"),
        "ghc": attrs.exec_dep(default = "toolchains//:ghc", providers = [RunInfo]),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
        "driver": attrs.source(default = "//build/haskell:corpus_fixture_driver"),
    },
)

def _oracle_impl(ctx):
    output = ctx.actions.declare_output("expectations.json")
    command = cmd_args([
        ctx.attrs.python[RunInfo], ctx.attrs.driver,
        "--manifest", ctx.attrs.corpus[PreparedCorpusInfo].manifest,
        "--ghc", ctx.attrs.ghc[RunInfo],
        "--oracle-source", ctx.attrs.source,
        "--classifications", ctx.attrs.classifications,
        "--nonterminating", ctx.attrs.nonterminating,
        "--timeout", str(ctx.attrs.timeout),
        "--module", ctx.attrs.module,
        "--output", output.as_output(),
    ])
    _roots(command, ctx.attrs.source_roots)
    if ctx.attrs.contract:
        command.add("--contract")
    ctx.actions.run(command, category = "native_suite_oracle", env = {"PATH": read_root_config("nix", "action_path")})
    return [DefaultInfo(default_output = output)]

tidepool_native_oracle = rule(
    impl = _oracle_impl,
    attrs = {
        "corpus": attrs.dep(providers = [PreparedCorpusInfo]),
        "source": attrs.source(),
        "source_roots": attrs.list(attrs.tuple(attrs.source(), attrs.string())),
        "classifications": attrs.source(),
        "nonterminating": attrs.source(),
        "timeout": attrs.int(default = 10),
        "module": attrs.string(default = "Suite"),
        "contract": attrs.bool(default = False),
        "ghc": attrs.exec_dep(default = "toolchains//:ghc", providers = [RunInfo]),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
        "driver": attrs.source(default = "//build/haskell:suite_oracle_driver"),
    },
)

def tidepool_corpus_test(name, corpus, expectations, visibility = []):
    """Run every compiler-emitted row through the existing isolated corpus owner."""
    sh_test(
        name = name,
        test = "//build/haskell:corpus_test",
        args = [
            "$(location //tidepool/prepared-corpus:prepared-corpus)",
            "$(location " + corpus + ")",
            "$(location " + expectations + ")",
        ],
        resources = [corpus, expectations, "//tidepool/prepared-corpus:prepared-corpus"],
        test_rule_timeout_ms = 14400000,
        visibility = visibility,
    )

def _fresh_corpus_inputs_impl(ctx):
    libdir = read_root_config("nix", "ghc_libdir", "")
    if not libdir.startswith("/nix/store/"):
        fail("fresh corpus requires the pinned production GHC package closure")
    inputs = ctx.actions.write_json("inputs.json", {
        "version": 1,
        "producer": _output(ctx.attrs.producer),
        "validator": _output(ctx.attrs.validator),
        "source": ctx.attrs.source,
        "module": ctx.attrs.module,
        "targets": ctx.attrs.targets_file,
        "source_roots": _checked_roots(ctx.attrs.source_roots),
        "all_tops": ctx.attrs.all_tops,
        "metadata_targets": ctx.attrs.metadata_targets,
        "ghc_libdir": libdir,
        "runtime_libraries": _output(ctx.attrs.runtime_libraries),
        "ghc": _output(ctx.attrs.ghc),
        "oracle_driver": ctx.attrs.oracle_driver,
        "oracle_source": ctx.attrs.oracle_source,
        "oracle_source_roots": _checked_roots(ctx.attrs.oracle_source_roots),
        "classifications": ctx.attrs.classifications,
        "nonterminating": ctx.attrs.nonterminating,
        "timeout": ctx.attrs.timeout,
    }, with_inputs = True)
    return [DefaultInfo(default_output = inputs, other_outputs = [
        ctx.attrs.source, ctx.attrs.targets_file, ctx.attrs.oracle_source,
        ctx.attrs.classifications, ctx.attrs.nonterminating, ctx.attrs.oracle_driver,
        _output(ctx.attrs.producer), _output(ctx.attrs.validator),
        _output(ctx.attrs.runtime_libraries), _output(ctx.attrs.ghc),
    ] + [tree for tree, _subdirectory in ctx.attrs.source_roots + ctx.attrs.oracle_source_roots])]

_fresh_corpus_inputs = rule(
    impl = _fresh_corpus_inputs_impl,
    attrs = {
        "source": attrs.source(),
        "module": attrs.string(),
        "targets_file": attrs.source(),
        "source_roots": attrs.list(attrs.tuple(attrs.source(), attrs.string())),
        "all_tops": attrs.bool(default = False),
        "metadata_targets": attrs.list(attrs.string(), default = []),
        "oracle_source": attrs.source(),
        "oracle_source_roots": attrs.list(attrs.tuple(attrs.source(), attrs.string())),
        "classifications": attrs.source(),
        "nonterminating": attrs.source(),
        "timeout": attrs.int(default = 10),
        "producer": attrs.exec_dep(default = "//bridge/haskell:execution_corpus_producer", providers = [RunInfo]),
        "validator": attrs.exec_dep(default = "//tidepool/prepared-corpus:prepared-corpus", providers = [RunInfo]),
        "runtime_libraries": attrs.dep(default = "//build/package:tidepool_extract_runtime_libraries"),
        "ghc": attrs.exec_dep(default = "toolchains//:ghc", providers = [RunInfo]),
        "oracle_driver": attrs.source(default = "//build/haskell:suite_oracle_driver"),
    },
)

def tidepool_fresh_corpus_test(name, visibility = [], **kwargs):
    """Compile an effectful cohort once during each native test invocation."""
    inputs_name = name + "_inputs"
    _fresh_corpus_inputs(name = inputs_name, **kwargs)
    sh_test(
        name = name,
        test = "//build/haskell:corpus_test",
        args = [
            "--fresh",
            "$(location toolchains//:python)",
            "$(location //build/haskell:corpus_fixture_driver)",
            "$(location :" + inputs_name + ")",
        ],
        env = {"PATH": read_root_config("nix", "action_path")},
        resources = [":" + inputs_name, "toolchains//:python", "//build/haskell:corpus_fixture_driver"],
        test_rule_timeout_ms = 14400000,
        visibility = visibility,
    )
