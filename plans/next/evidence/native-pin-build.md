# Native pin build delivery

Source checked: b964c8ac9c9749dff12c37450f92af8672e211e9.
Only Codex input changed in flake.lock; native pin is exact
600be9df39096121f76745f7d7f73a96bae8c82e. No Cargo/service/native source edits.

Executed successfully:
- nix flake update codex (only locked Codex revision/hash/timestamp changed).
- nix build .#checks.x86_64-linux.codex-host-tools-contract --no-link --print-out-paths
- Built CLI --version, app-server --help and observe --help.
- nixfmt --check flake.nix; git diff --check.

Contract derivation result:
/nix/store/k8i4i6vv72xdr8s27x5zdfp4514y1k75-codex-host-tools-contract
Native executable:
/nix/store/rm93f2zsk2js7mbivk6x0fjhxmz7b6k0-codex-rs-0.0.0-dev+600be9d/bin/codex
Version: codex-cli 0.0.0-dev+600be9d
SHA256:21e5f1c172ca35f122c3e81e3fa996e62b758f6abf518a354386fb961ca1487e

The existing CLI contract check passed preserved host-tools/fork/queue/archive
surface plus newly added controller-token-file and observe remote flags. This is
CLI surface/build evidence, NOT controlled protocol, provider, mounted canary,
readiness behavior, external-effect reconciliation or process cleanup acceptance.
No running host was replaced; no external checkout changed. Native unit suites
were not rerun. Nix warned that configured Cachix substituter/key were untrusted;
build nevertheless completed successfully through the existing package owner.

Full lock/build logs and binary hash remain in target/native-pin-evidence in this
actor worktree. The build required about17 minutes; no exact cost claim. One broad
process-argument diagnostic accidentally expanded inherited prompt arguments;
using comm-only process summaries corrected that avoidable context waste. Future
build inspection should start with bounded owner logs and process names, not full
argv. Long retained native waits needed no Haskell API discovery or child launches.
