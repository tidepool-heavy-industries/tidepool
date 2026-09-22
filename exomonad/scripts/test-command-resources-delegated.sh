#!/usr/bin/env bash
set -euo pipefail

binary="$({
  cargo test -p exomonad-node --test command_resources --no-run --message-format=json
} | jq -r '
  select(
    .reason == "compiler-artifact"
    and .profile.test
    and .target.name == "command_resources"
  )
  | .executable // empty
' | tail -n 1)"

if [[ -z "$binary" || ! -x "$binary" ]]; then
  echo "could not locate the command_resources test binary" >&2
  exit 1
fi

# Service mode puts only the test process in the delegated cgroup. A transient
# --scope also contains the systemd-run client, which prevents the test owner
# from enabling controllers after it moves itself into its control subgroup.
unit="tidepool-command-resources-test-$$"
systemd-run \
  --user \
  --pipe \
  --wait \
  --collect \
  --service-type=exec \
  --property=Delegate=yes \
  --setenv="PATH=$PATH" \
  --unit="$unit" \
  "$(realpath "$binary")" \
  --ignored \
  --test-threads=1 \
  --nocapture
