"""Declared native catalog production and bundle assembly."""

load("@prelude//:rules.bzl", "genrule")
load("//build:native_profile.bzl", "selected_native_profile")

def _catalog_sources_impl(ctx):
    cohort = ctx.actions.write_json("cohort.json", {
        "components": ctx.attrs.components,
        "modules": ctx.attrs.modules,
    })
    output = ctx.actions.declare_output("catalog-sources", dir = True)
    entry = ctx.actions.declare_output("TidepoolPreparedDriver.hs")
    ctx.actions.run(
        cmd_args([
            ctx.attrs.entry_source[RunInfo],
            "--module", "TidepoolPreparedDriver",
            "--entry", "Tidepool.Actors.Internal.ExomonadDriver.rootDriver",
            "--effects", "Tidepool.Actors.Internal.ExomonadDriver.RootEffects",
            "--output", entry.as_output(),
        ]),
        category = "native_root_entry_source",
    )
    ctx.actions.run(
        cmd_args([
            ctx.attrs.python[RunInfo], ctx.attrs.qualification,
            "snapshot-sources",
            "--sources", ctx.attrs.sources,
            "--effects", ctx.attrs.effects,
            "--jev-sources", ctx.attrs.jev_sources,
            "--cohort", cohort,
            "--root-entry-source", entry,
            "--output", output.as_output(),
        ]),
        category = "native_catalog_sources",
    )
    return [DefaultInfo(default_output = output)]

_catalog_sources = rule(
    impl = _catalog_sources_impl,
    attrs = {
        "components": attrs.list(attrs.string()),
        "modules": attrs.dict(attrs.string(), attrs.string()),
        "sources": attrs.source(),
        "effects": attrs.source(),
        "jev_sources": attrs.source(),
        "entry_source": attrs.exec_dep(default = "//tidepool/runtime:tidepool-entry-source", providers = [RunInfo]),
        "qualification": attrs.source(default = "//build/package:qualification_script"),
        "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
    },
)

def native_catalog_sources(name, cohort, **kwargs):
    _catalog_sources(name = name, components = cohort["components"], modules = cohort["modules"], **kwargs)


def _native_products_impl(ctx, operation, output_name, category):
    if not ctx.attrs.source_root or not ctx.attrs.retention_record_origin:
        fail("native products require the original retained source selection")
    output = ctx.actions.declare_output(output_name, dir = True)
    ctx.actions.run(
        cmd_args([
            ctx.attrs.python[RunInfo], ctx.attrs.qualification, operation,
            "--snapshot", ctx.attrs.snapshot,
            "--source-root", ctx.attrs.source_root,
            "--declared-source-root", ctx.attrs.declared_source_root,
            "--retention-record", ctx.attrs.retention_record,
            "--retention-record-origin", ctx.attrs.retention_record_origin,
            "--runtime-tools", ctx.attrs.runtime_tools_root,
            "--producer", ctx.attrs.producer[DefaultInfo].default_outputs[0],
            "--frontend", ctx.attrs.frontend[DefaultInfo].default_outputs[0],
            "--worker", ctx.attrs.worker[DefaultInfo].default_outputs[0],
            "--deployment", ctx.attrs.deployment,
            "--ghc-libdir", ctx.attrs.ghc_libdir,
            "--libraries", ctx.attrs.libraries,
            "--output", output.as_output(),
        ], hidden = [ctx.attrs.runtime_tools, ctx.attrs.declared_ghc_libdir]),
        category = category,
    )
    return [DefaultInfo(default_output = output)]

def _native_catalog_impl(ctx):
    return _native_products_impl(ctx, "build-catalog", "native-catalog", "native_catalog")

def _native_root_entry_impl(ctx):
    return _native_products_impl(ctx, "build-root-entry", "prepared-root-entry", "native_root_entry")

_native_products_attrs = {
    "snapshot": attrs.source(),
    "source_root": attrs.string(),
    "declared_source_root": attrs.source(),
    "retention_record": attrs.source(),
    "retention_record_origin": attrs.string(),
    "runtime_tools": attrs.source(),
    "runtime_tools_root": attrs.string(),
    "producer": attrs.exec_dep(providers = [RunInfo]),
    "frontend": attrs.exec_dep(providers = [RunInfo]),
    "worker": attrs.exec_dep(providers = [RunInfo]),
    "deployment": attrs.source(),
    "ghc_libdir": attrs.string(),
    "declared_ghc_libdir": attrs.source(),
    "libraries": attrs.source(),
    "qualification": attrs.source(default = "//build/package:qualification_script"),
    "python": attrs.exec_dep(default = "toolchains//:python", providers = [RunInfo]),
}

native_catalog = rule(impl = _native_catalog_impl, attrs = _native_products_attrs)
native_root_entry = rule(impl = _native_root_entry_impl, attrs = _native_products_attrs)

def native_prepared_products(**kwargs):
    native_catalog(name = "native_catalog", **kwargs)
    native_root_entry(name = "native_root_entry", **kwargs)


def native_runtime_bundle(name, catalog_backed):
    bundle_sources = {
        "host": "//bridge/facade:exomonad",
        "view-helper": "//bridge/facade:exomonad-view-helper",
        "libtest": "//bridge/facade:tidepool_unit_tests",
        "compiler-frontend": "//tidepool/extract-cmd:tidepool-extract",
        "compiler-worker": "//bridge/haskell:tidepool_extract_bin",
        "haskell-sources": "//bridge/haskell:facade_embedded_sources",
        "web-assets": "//web:dist",
        "browser-driver": "//build/testing/browser:driver_bundle",
        "web-provenance": ":embedded_web_provenance",
        "runtime-libraries": ":tidepool_extract_runtime_libraries",
        "qualification": ":qualification_script",
        "entrypoint": ":native_entrypoint",
        "runtime-tools": "toolchains//:exomonad_runtime_tools",
        "build-sources": "//build/rust:native_qualification_sources",
        "workspace-gitlink": "//build/rust:workspace_gitlink",
        "workspace-git-bundle": "//build/rust:workspace_git_bundle",
        "haskell-test-fixtures": "//bridge/testing:haskell_test_fixtures",
        "fixture-manifest": "//:test_fixture_manifest",
    }
    if catalog_backed:
        bundle_sources.update({"catalog": ":native_catalog", "root-entry": ":native_root_entry"})
    genrule(
        name = name,
        srcs = bundle_sources,
        out = ".",
        env = {
            "PACKAGE_GHC_LIBDIR": read_root_config("nix", "ghc_libdir"),
            "PACKAGE_RUNTIME_TOOLS": read_root_config("nix", "exomonad_runtime_tools", ""),
            "PACKAGE_NATIVE_PROFILE": selected_native_profile(),
        },
        bash = "set -euo pipefail\n" + ('CATALOG_ARGUMENTS=(--catalog "$PWD/$(location :native_catalog)" --root-entry "$PWD/$(location :native_root_entry)")\n' if catalog_backed else 'CATALOG_ARGUMENTS=()\n') + """
    "$(exe toolchains//:python)" "$PWD/$(location :qualification_script)" assemble \
      --output "$PWD/$OUT" \
      --host "$PWD/$(location //bridge/facade:exomonad)" \
      --view-helper "$PWD/$(location //bridge/facade:exomonad-view-helper)" \
      --libtest "$PWD/$(location //bridge/facade:tidepool_unit_tests)" \
      --build-sources "$PWD/$(location //build/rust:native_qualification_sources)" \
      --test-fixtures "$PWD/$(location //bridge/testing:haskell_test_fixtures)" \
      --fixture-manifest "$PWD/$(location //:test_fixture_manifest)" \
      --workspace-gitlink "$PWD/$(location //build/rust:workspace_gitlink)" \
      --workspace-git-bundle "$PWD/$(location //build/rust:workspace_git_bundle)" \
      --frontend "$PWD/$(location //tidepool/extract-cmd:tidepool-extract)" \
      --worker "$PWD/$(location //bridge/haskell:tidepool_extract_bin)" \
      --sources "$PWD/$(location //bridge/haskell:facade_embedded_sources)" \
      --assets "$PWD/$(location //web:dist)" \
      --browser-driver "$PWD/$(location //build/testing/browser:driver_bundle)" \
      --libraries "$PWD/$(location :tidepool_extract_runtime_libraries)" \
      --harness-revision "$PWD/$(location :embedded_web_provenance)" \
      --runtime-tools "$PACKAGE_RUNTIME_TOOLS" --ghc-libdir "$PACKAGE_GHC_LIBDIR" \
      --entrypoint-template "$PWD/$(location :native_entrypoint)" \
      --profile "$PACKAGE_NATIVE_PROFILE" "${CATALOG_ARGUMENTS[@]}"
    """,
        visibility = ["PUBLIC"],
    )
