"""Verified test-only source ownership shared by build and check selection.

Only explicitly reviewed paths belong here. Unknown source remains production
input; moving a registered source into production requires removing its entry.
The named Cargo unit-test target owns these module and embedded fixture bytes.
"""

TEST_ONLY_SOURCES = {
    'tidepool-extract-cmd': frozenset({
        'tidepool/extract-cmd/src/diagnostics_tests.rs',
    }),
    'tidepool-codegen': frozenset({
        'tidepool/codegen/src/prepared_program/machine/literal_manifest_tests.rs',
    }),
    'tidepool': frozenset({
        'bridge/facade/src/exomonad/source/publication_fault_tests.rs',
        'bridge/facade/src/exomonad/source/publication_fault_driver.hs',
        'bridge/facade/src/actor_host/fixtures/browser_agent_spec.hs',
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
        'exomonad/actor/src/resident_actor/capture_workspace_child.hs',
        'exomonad/actor/src/resident_actor/captured_readers_parent.hs',
        'exomonad/actor/src/resident_actor/captured_reader_reuse.hs',
        'exomonad/actor/src/resident_actor/capture_workspace_tests.rs',
        'exomonad/actor/src/resident_actor/capture_partial_startup_failure.hs',
        'exomonad/actor/src/resident_actor/owned_workbench/model_tests.rs',
    }),
    'tidepool-runtime': frozenset({
        'tidepool/runtime/src/session/exact_recovery_acceptance_tests.rs',
        'tidepool/runtime/src/session/fixtures/checked-failed-display-cell.hs',
        'tidepool/runtime/src/session/fixtures/checked-fold-outcome-template.hs',
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
        'tidepool/runtime/src/session/fixtures/compiled-cell-record-selector.hs',
        'tidepool/runtime/src/session/fixtures/compiled-cell-simple.hs',
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
        'tidepool/runtime/src/session/fixtures/recovery-dependent.hs',
        'tidepool/runtime/src/session/fixtures/recovery-native-packet.json',
        'tidepool/runtime/src/session/fixtures/recovery-original.hs',
        'tidepool/runtime/src/session/paired_publication/linearization_tests.rs',
        'tidepool/runtime/src/session/turn_scaling_tests.rs',
    }),
    'tidepool-toolchain': frozenset({
        'tidepool/toolchain/src/module_candidates/codec_measurement.rs',
        'tidepool/toolchain/tests/fixtures/deployment-module-package/Consumer.hs',
        'tidepool/toolchain/tests/fixtures/materialization-fault.c',
        'tidepool/toolchain/tests/fixtures/owned-declaration/ExactConsumer.hs',
        'tidepool/toolchain/tests/fixtures/owned-declaration/G1.hs',
        'tidepool/toolchain/tests/fixtures/owned-declaration/G3.hs',
        'tidepool/toolchain/tests/fixtures/typeable-tuple/G1.hs',
    }),
}


def test_source_owner(path):
    """Return the owning package for an explicitly registered test-only path."""
    return next((package for package, sources in TEST_ONLY_SOURCES.items()
                 if path in sources), None)
