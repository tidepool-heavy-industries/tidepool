set shell := ["bash", "-euo", "pipefail", "-c"]

# Buck actions use configured, materialized Nix tools; resource admission is external.
default:
    @just --list

[positional-arguments]
build *targets:
    bash scripts/buck2-run.sh build --local-only -c remote.enabled=false "$@"

# Native libtest runners own discovery, nonzero counts, isolation, and evidence.
[positional-arguments]
test-lib package *args:
    python3 scripts/native-workflow.py test-lib "$@"

[positional-arguments]
test-target package target *args:
    python3 scripts/native-workflow.py test-target "$@"

[positional-arguments]
test-bin package binary *args:
    python3 scripts/native-workflow.py test-bin "$@"

[positional-arguments]
test-list package kind target="":
    python3 scripts/native-workflow.py test-list "$1" "$2" ${3:+"$3"}

# Run a named native Rust/Tasty target, forwarding its owning runner's arguments.
[positional-arguments]
test-native target *args:
    bash scripts/buck2-run.sh run --local-only -c remote.enabled=false "$1" -- "${@:2}"

[positional-arguments]
suite package:
    python3 scripts/native-workflow.py suite "$1"

[positional-arguments]
suite-plan package:
    python3 scripts/native-workflow.py suite-plan "$1"

quick:
    python3 scripts/native-workflow.py quick

# Link all registered native consumers. This compiles harnesses without executing them.
check:
    python3 scripts/native-workflow.py check

lint:
    python3 scripts/native-workflow.py lint

[positional-arguments]
fixtures-check *cohorts:
    python3 scripts/native-workflow.py fixtures-check "$@"

probe-opacity-check:
    bash scripts/buck2-run.sh build --local-only -c remote.enabled=false //bridge/haskell:probe_opacity

# Broad integration gate; use focused targets during ordinary development.
verify:
    python3 scripts/native-workflow.py verify

test-toolchain-scripts:
    bash scripts/buck2-run.sh test --local-only -c remote.enabled=false //scripts:toolchain_script_tests

test-workflow-scripts:
    bash scripts/buck2-run.sh test --local-only -c remote.enabled=false //scripts:native_workflow_tests

# A raw build is frozen/qualified explicitly before it can own a run.
exomonad-build:
    bash scripts/buck2-run.sh build --local-only -c remote.enabled=false //build/package:native_runtime_bundle

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
