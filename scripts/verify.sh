#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-steps.sh

# Each step answers an independent question; all of them run.
run_step "scripts/check.sh (lint, default-tier tests)" scripts/check.sh
run_step "scripts/test-suite-check.sh" scripts/test-suite-check.sh
run_step "scripts/fixtures.sh check" scripts/fixtures.sh check

finish_steps "verify"
