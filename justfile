set shell := ["bash", "-euo", "pipefail", "-c"]

# Override explicitly with `just --set native_profile production test-lib ...`.
native_profile := "fast-dev"

# Buck actions use configured, materialized Nix tools; resource admission is external.
default:
    @just --list

[positional-arguments]
build *targets:
    bash scripts/buck2-run.sh build --local-only -c remote.enabled=false -c {{quote("tidepool.profile=" + native_profile)}} "$@"

# Native libtest runners own discovery, nonzero counts, isolation, and evidence.
[positional-arguments]
test-lib package *args:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} test-lib "$@"

# Retain activation compiler traces in a caller-owned, invocation-specific directory.
[positional-arguments]
activation-input-perf-trace output_dir:
    swarm-build env TIDEPOOL_KEEP_TEST_LOGS=1 TIDEPOOL_TIMING=1 python3 scripts/native-workflow.py --profile {{quote(native_profile)}} test-lib tidepool-runtime --exact session::resident::activation_input_tests::activation_preview_keeps_original_display_with_retained_prefix_and_refuses_explicit_heap_inputs --exact session::resident::activation_input_tests::shared_request_site_composes_but_demands_unique_original_preview_context --exact session::resident::activation_input_tests::activation_function_input_preserves_value_across_repeated_checked_mounts --expected-count 3 --jobs 1 --timeout 600 --delegated-service --service-slice tidepool-completion-build.slice --output-dir {{quote(output_dir)}} --retain-artifacts --compiler-mode owned-resident

[positional-arguments]
test-target package target *args:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} test-target "$@"

[positional-arguments]
test-bin package binary *args:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} test-bin "$@"

[positional-arguments]
test-list package kind target="":
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} test-list "$1" "$2" ${3:+"$3"}

# Run a named native Rust/Tasty target, forwarding its owning runner's arguments.
[positional-arguments]
test-native target *args:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} test-native "$@"

[positional-arguments]
suite package:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} suite "$1"

[positional-arguments]
suite-plan package:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} suite-plan "$1"

quick:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} quick

# Link all registered native consumers. This compiles harnesses without executing them.
check:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} check

lint:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} lint

[positional-arguments]
fixtures-check *cohorts:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} fixtures-check "$@"

probe-opacity-check:
    bash scripts/buck2-run.sh build --local-only -c remote.enabled=false -c {{quote("tidepool.profile=" + native_profile)}} //bridge/haskell:probe_opacity

# Broad integration gate; use focused targets during ordinary development.
verify:
    python3 scripts/native-workflow.py --profile {{quote(native_profile)}} verify

test-toolchain-scripts:
    bash scripts/buck2-run.sh test --local-only -c remote.enabled=false -c {{quote("tidepool.profile=" + native_profile)}} //scripts:toolchain_script_tests

test-workflow-scripts:
    bash scripts/buck2-run.sh test --local-only -c remote.enabled=false -c {{quote("tidepool.profile=" + native_profile)}} //scripts:native_workflow_tests

# A raw build is frozen/qualified explicitly before it can own a run.
exomonad-build:
    bash scripts/buck2-run.sh build --local-only -c remote.enabled=false -c {{quote("tidepool.profile=" + native_profile)}} //build/package:native_runtime_bundle

[positional-arguments]
exomonad-freeze *args:
    python3 build/package/qualification.py freeze "$@"

[positional-arguments]
exomonad-run bundle descriptor report *args:
    python3 "$1/share/exomonad/qualification.py" exec --report "$3" "$2" -- "${@:4}"

[positional-arguments]
exomonad-init bundle descriptor report *args:
    python3 "$1/share/exomonad/qualification.py" exec --report "$3" "$2" -- init "${@:4}"

[positional-arguments]
exomonad-check-recipes bundle descriptor reports workspace parallelism="1":
    bash exomonad/scripts/exomonad-check-recipes.sh "$@"

[positional-arguments]
doctor bundle descriptor:
    bash scripts/toolchain-doctor.sh "$@"

[positional-arguments]
daemon-start bundle descriptor:
    bash -c 'source scripts/lib-extract.sh; select_native_bundle "$1" "$2"; daemon_start_persistent' native-daemon "$@"

daemon-stop:
    bash -c 'source scripts/lib-extract.sh; daemon_stop_persistent'

# Only the actual test child enters the delegated service; Buck stays outside it.
[positional-arguments]
test-command-resources-delegated output:
    bash exomonad/scripts/test-command-resources-delegated.sh "$1"

[positional-arguments]
test-embedded-command-delegated bundle descriptor output:
    bash exomonad/scripts/test-embedded-command-delegated.sh "$@"

[positional-arguments]
test-m1 bundle descriptor output:
    bash build/testing/run-m1-acceptance.sh "$@"
