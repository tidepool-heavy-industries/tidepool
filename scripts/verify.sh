#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-steps.sh

# Each step answers an independent question; all of them run. The fixture
# corpus uses its own resident compiler process, not the check step's compile
# daemon, so it runs beside check instead of after it.
trap 'kill_background_steps; exit 130' INT TERM
run_step_background "scripts/fixtures.sh check" scripts/fixtures.sh check
run_step "scripts/check.sh (lint, default-tier tests)" scripts/check.sh
run_step "scripts/test-suite-check.sh" scripts/test-suite-check.sh
wait_background_steps

finish_steps "verify"
