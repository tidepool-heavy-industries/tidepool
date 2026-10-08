"""Verified test-only source ownership shared by build and check selection.

Only explicitly reviewed paths belong here. Unknown source remains production
input; moving a registered source into production requires removing its entry.
The named Cargo unit-test target owns these module and embedded fixture bytes.
"""
import re

TEST_ONLY_SOURCES = {
    'exomonad-node': frozenset({
        'exomonad/node/src/inbox/history_properties.rs',
    }),
    'exomonad-worktree': frozenset({
        'exomonad/worktree/src/journal_properties.rs',
    }),
    'exomonad-tool': frozenset({
        'exomonad/tool/src/surface/properties.rs',
    }),
    'tidepool-heap': frozenset({
        'tidepool/heap/src/static_region/properties.rs',
    }),
    'tidepool-extract-cmd': frozenset({
        'tidepool/extract-cmd/src/diagnostics_tests.rs',
        'tidepool/extract-cmd/src/fixtures/build_products_worker.rs',
        'tidepool/extract-cmd/src/response_properties.rs',
    }),
    'tidepool-codegen': frozenset({
        'tidepool/codegen/src/prepared_program/machine/literal_manifest_tests.rs',
    }),
    'tidepool': frozenset({
        'bridge/facade/src/actor_host/embedded_agent_spec_tests.rs',
        'bridge/facade/src/actor_host/embedded_operation_settlement_tests.rs',
        'bridge/facade/src/actor_host/recipe_checks/prepared_contract_tests.rs',
        'bridge/facade/src/actor_host/recipe_checks/prepared_contract_expression.hs',
        'bridge/facade/src/exomonad/workspace/source_capture_tests.rs',
        'bridge/facade/src/actor_host/embedded_idle_retirement_tests.rs',
        'bridge/facade/src/actor_host/restart_effect_once.hs',
        'bridge/facade/src/actor_host/fixtures/retained_handler_agent_spec.hs',
        'bridge/facade/src/actor_host/fixtures/retained_handler_tools.hs',
        'bridge/facade/src/actor_host/embedded_captured_child_reuse_nominal.hs',
        'bridge/facade/src/actor_host/command_tool_facts_gate.hs',
        'bridge/facade/src/actor_host/prepared_display_tests.rs',
        'bridge/facade/src/actor_host/prepared_display_success.hs',
        'bridge/facade/src/actor_host/prepared_display_failure.hs',
        'bridge/facade/src/actor_host/prepared_runtime_acceptance.rs',
        'bridge/facade/src/actor_host/prepared_runtime_children.hs',
        'bridge/facade/src/actor_host/prepared_runtime_spec.hs',
        'bridge/facade/src/actor_host/notebook_explicit_display_siblings.hs',
        'bridge/facade/src/actor_host/notebook_explicit_display_budget_failure.hs',
        'bridge/facade/src/actor_host/notebook_explicit_display_untrusted_callback.hs',
        'bridge/facade/src/actor_host/cargo_report_contract.hs',
        'bridge/facade/src/actor_host/native_prefix_publication_tests.rs',
        'bridge/facade/src/actor_host/notebook_prefix_baseline.hs',
        'bridge/facade/src/actor_host/notebook_prefix_failure.hs',
        'bridge/facade/src/exomonad/source/publication_fault_tests.rs',
        'bridge/facade/src/exomonad/source/publication_fault_driver.hs',
        'bridge/facade/src/actor_host/fixtures/browser_agent_spec.hs',
        'bridge/facade/src/actor_host/fixtures/unconstrained_spec_later_helper.hs',
        'bridge/facade/src/actor_host/fixtures/request_reload_agent_spec.hs',
        'bridge/facade/src/actor_host/m1_request_reload_tests.rs',
        'bridge/facade/src/actor_host/m1_eight_actor_launch.hs',
        'bridge/facade/src/actor_host/m1_eight_actor_performance.rs',
        'bridge/facade/src/actor_host/m1_cancel_cell.hs',
        'bridge/facade/src/actor_host/m1_cancel_performance.rs',
        'bridge/facade/src/actor_host/m1_warm_cell_performance.rs',
        'bridge/facade/src/actor_host/m1_compiler_attribution_tests.rs',
        'bridge/facade/src/actor_host/m1_warm_cell_workloads.json',
    }),
    'exomonad-actor': frozenset({
        'exomonad/actor/src/resident_actor/forest_shutdown_tests.rs',
        'exomonad/actor/src/local_actor/reply_settlement_history.rs',
        'exomonad/actor/src/fixtures/prepared-instance-agent-spec.hs',
        'exomonad/actor/src/fixtures/prepared-instance-provider.hs',
        'exomonad/actor/src/fixtures/ForkReplyContracts.hs',
        'exomonad/actor/src/local_actor/failure_origin.hs',
        'exomonad/actor/src/request/sequence_tests.rs',
        'exomonad/actor/src/resident_actor/capture_workspace_child.hs',
        'exomonad/actor/src/resident_actor/captured_readers_parent.hs',
        'exomonad/actor/src/resident_actor/captured_reader_reuse.hs',
        'exomonad/actor/src/resident_actor/capture_workspace_tests.rs',
        'exomonad/actor/src/resident_actor/capture_partial_startup_failure.hs',
        'exomonad/actor/src/resident_actor/owned_workbench/model_tests.rs',
        'exomonad/actor/src/resident_workbench/display_callback_tests.rs',
        'exomonad/actor/src/resident_workbench/display_callback_immediate.hs',
    }),
    'tidepool-runtime': frozenset({
        'tidepool/runtime/src/session/registry_properties.rs',
        'tidepool/runtime/src/session/fixtures/ProvenanceSharedRequest.hs',
        'tidepool/runtime/src/session/fixtures/provenance-shared-request-producer.hs',
        'tidepool/runtime/src/session/fixtures/provenance-shared-request-receiver.hs',
        'tidepool/runtime/src/session/fixtures/resident-receive-value.hs',
        'tidepool/runtime/src/session/fixtures/resident-receive-value-runner.hs',
        'tidepool/runtime/src/session/fixtures/activation-preview-retained-prefix.hs',
        'tidepool/runtime/src/session/fixtures/activation-preview-retained-request.hs',
        'tidepool/runtime/src/session/fixtures/unrelated-home-value.hs',
        'tidepool/runtime/src/session/exact_recovery_acceptance_tests.rs',
        'tidepool/runtime/src/session/fixtures/checked-cell-template.hs',
        'tidepool/runtime/src/session/fixtures/checked-home-relay.hs',
        'tidepool/runtime/src/session/fixtures/checked-home-value.hs',
        'tidepool/runtime/src/session/fixtures/checked-interface-publication.hs',
        'tidepool/runtime/src/session/fixtures/checked-interleaved-declaration.hs',
        'tidepool/runtime/src/session/fixtures/checked-local-declaration.hs',
        'tidepool/runtime/src/session/fixtures/checked-search-alternate.hs',
        'tidepool/runtime/src/session/fixtures/checked-search-invalid.hs',
        'tidepool/runtime/src/session/fixtures/checked-search-original.hs',
        'tidepool/runtime/src/session/fixtures/checked-tiny-support.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-independent-original.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-local-fixity.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-mixed-original.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-native-binding-declaration.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-native-binding-support.hs',
        'tidepool/runtime/src/session/fixtures/checked-native-support.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-record-selector.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-simple.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-source-selected-declaration.hs',
        'tidepool/runtime/src/session/fixtures/constraint-tuple-G1.hs',
        'tidepool/runtime/src/session/fixtures/exact-join-original.hs',
        'tidepool/runtime/src/session/fixtures/paired-instance-only.hs',
        'tidepool/runtime/src/session/fixtures/paired-late-operator.hs',
        'tidepool/runtime/src/session/fixtures/paired-public-value-only.hs',
        'tidepool/runtime/src/session/fixtures/paired-rebase-A.hs',
        'tidepool/runtime/src/session/fixtures/paired-rebase-B.hs',
        'tidepool/runtime/src/session/fixtures/paired-rich-A1.hs',
        'tidepool/runtime/src/session/fixtures/paired-rich-A2.hs',
        'tidepool/runtime/src/session/fixtures/paired-rich-B.hs',
        'tidepool/runtime/src/session/fixtures/paired-rich-base.hs',
        'tidepool/runtime/src/session/fixtures/protected-scale-bind.hs',
        'tidepool/runtime/src/session/fixtures/protected-scale-foundation.hs',
        'tidepool/runtime/src/session/fixtures/retained-import-consumer.hs',
        'tidepool/runtime/src/session/fixtures/retained-import-producer.hs',
        'tidepool/runtime/src/session/fixtures/retained-import-replacement.hs',
        'tidepool/runtime/src/session/fixtures/recovery-control.hs',
        'tidepool/runtime/src/session/fixtures/recovery-dependent.hs',
        'tidepool/runtime/src/session/fixtures/recovery-live-bind.hs',
        'tidepool/runtime/src/session/fixtures/recovery-live-declaration.hs',
        'tidepool/runtime/src/session/fixtures/recovery-original.hs',
        'tidepool/runtime/src/session/paired_publication/linearization_tests.rs',
        'tidepool/runtime/src/session/turn_scaling_tests.rs',
        'tidepool/runtime/src/session/fixtures/typed-segment-illtyped.hs',
        'tidepool/runtime/src/session/fixtures/typed-segment-let-generalization.hs',
        'tidepool/runtime/src/session/fixtures/typed-segment-oracle.hs',
        'tidepool/runtime/src/session/fixtures/typed-segment-probe-support.hs',
        'tidepool/runtime/src/session/fixtures/typed-segment-retained-bottom.hs',
        'tidepool/runtime/src/session/fixtures/typed-segment-strict-publication.hs',
        'tidepool/runtime/src/session/typed_segment_tests.rs',

    }),
    'tidepool-toolchain': frozenset({
        'tidepool/toolchain/src/artifact_inventory/view_read_properties.rs',
        'tidepool/toolchain/src/artifacts/tests/private_package_input_history.rs',
        'tidepool/toolchain/src/cache/source_manifest_properties.rs',
        'tidepool/toolchain/src/declaration_context/native_availability_tests.rs',
        'tidepool/toolchain/src/module_candidates/codec_measurement.rs',
        'tidepool/toolchain/src/module_candidates/deployment/tests/product_carry.rs',
        'tidepool/toolchain/src/module_candidates/fixture_packets.rs',
        'tidepool/toolchain/src/module_candidates/fixture_packets/codec.rs',
        'tidepool/toolchain/src/module_candidates/product_decode_observer.rs',
        'tidepool/toolchain/src/module_candidates/protected_native_availability_tests.rs',
        'tidepool/toolchain/src/module_candidates/tests/product_carry.rs',
        'tidepool/toolchain/src/certified_products/home_self_issuer_tests.rs',
        'tidepool/toolchain/src/certified_products/retained_core/properties.rs',
        'tidepool/toolchain/src/certified_products/tests/artifact_view_group_index_properties.rs',
        'tidepool/toolchain/src/certified_products/tests/issued_interface_selection_history.rs',
        'tidepool/toolchain/src/certified_products/tests/promotion_import_history.rs',
        'tidepool/toolchain/src/certified_products/tests/sparse_interface_selection_properties.rs',
        'tidepool/toolchain/tests/fixtures/home-self-issuer/HomeSelf.hs',
        'tidepool/toolchain/tests/fixtures/home-self-issuer/HomeSelfCapture.hs',
        'tidepool/toolchain/tests/fixtures/deployment-module-package/Consumer.hs',
        'tidepool/toolchain/tests/fixtures/materialization-fault.c',
        'tidepool/toolchain/tests/fixtures/owned-declaration/ExactConsumer.hs',
        'tidepool/toolchain/tests/fixtures/owned-declaration/G1.hs',
        'tidepool/toolchain/tests/fixtures/owned-declaration/G3.hs',
        'tidepool/toolchain/tests/fixtures/typeable-tuple/G1.hs',
    }),
}


def integration_target_sources(targets):
    """Map every Rust source reachable from an integration target to its target.

    Unknown module declarations are reported separately so selectors can widen
    their obligation instead of silently missing support files.
    """
    from pathlib import Path

    path_attr = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')
    module = re.compile(r'(?m)^[ \t]*(?P<attrs>(?:#\[[^\n]+?\][ \t]*)*)(?:(?:pub(?:\([^)]*\))?)[ \t]+)?mod[ \t]+(?P<name>\w+)[ \t]*;')
    result = {}
    unknown = set()

    for target in targets:
        if "test" not in target.get("kind", []):
            continue
        entry = Path(target["src_path"]).resolve()
        pending = [entry]
        seen = set()
        while pending:
            source = pending.pop()
            if source in seen:
                continue
            seen.add(source)
            target_name = target.get("name", Path(target["src_path"]).stem)
            result.setdefault(source, set()).add(target_name)
            try:
                text = source.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                unknown.add(target_name)
                continue
            source_lines = text.splitlines()
            for match in module.finditer(text):
                line_index = text.count("\n", 0, match.start())
                attrs = []
                cursor = line_index - 1
                while cursor >= 0:
                    line = source_lines[cursor].strip()
                    if not line or line.startswith(("#[", "//", "/*", "*", "*/")):
                        attrs.append(source_lines[cursor])
                        cursor -= 1
                        continue
                    break
                attr = [value for line in reversed(attrs) for value in path_attr.finditer(line)]
                attr.extend(path_attr.finditer(match["attrs"]))
                if attr:
                    child = (source.parent / attr[-1][1]).resolve()
                else:
                    name = match["name"]
                    base = source.parent if source == entry or source.name in ("mod.rs", "lib.rs", "main.rs") else source.parent / source.stem
                    options = (base / f"{name}.rs", base / name / "mod.rs")
                    child = next((item.resolve() for item in options if item.is_file()), None)
                    if child is None:
                        unknown.add(target_name)
                        continue
                if not child.is_file():
                    unknown.add(target_name)
                else:
                    pending.append(child)
    return result, unknown


def registration_errors(metadata, _root):
    """Report missing suite roots, unresolved modules, and orphan test files."""
    from pathlib import Path
    errors = []
    members = set(metadata["workspace_members"])
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        package_root = Path(package["manifest_path"]).parent
        tests = package_root / "tests"
        if not (tests / "suites").is_dir():
            continue
        targets = package["targets"]
        graph, unknown = integration_target_sources(targets)
        root_counts = {}
        for target in targets:
            if "test" in target.get("kind", []):
                path = Path(target["src_path"]).resolve()
                root_counts[path] = root_counts.get(path, 0) + 1
        suite_roots = {p.resolve() for p in (tests / "suites").glob("*.rs")}
        for suite_root in suite_roots:
            if root_counts.get(suite_root, 0) != 1:
                errors.append(f"{package['name']}: suite entry point is not registered: {suite_root.name}")
        for target in sorted(unknown):
            errors.append(f"{package['name']}: cannot resolve every module for target {target}")
        test_attribute = re.compile(r"#\[(?:tokio::)?test\b|#\[test_case\b|#\[rstest\b")
        for source in sorted(tests.rglob("*.rs")):
            try:
                text = source.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            if not test_attribute.search(text):
                continue
            owners = graph.get(source.resolve(), set())
            if not owners:
                errors.append(f"{package['name']}: test source is not registered: {source.relative_to(tests)}")
    return errors


def test_source_owner(path):
    """Return the owning package for an explicitly registered test-only path."""
    return next((package for package, sources in TEST_ONLY_SOURCES.items()
                 if path in sources), None)
