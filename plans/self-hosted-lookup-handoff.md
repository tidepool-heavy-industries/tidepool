# Self-hosted lookup handoff

The `codex/self-hosted-lookup` branch replaces the special hosted lookup path
with a structured `Lookup` effect and an editable, precompiled Haskell tool.
The workspace template adds one Jev scoring batch over compiler-resolved
references or failed-name alternatives, followed by at most four raw lookups.
Namespace identity and compile-view fencing survive the whole operation.
This checkout's active `.shoal` configuration is not part of the change.

## Engine blocker

The full integration test is active and currently fails before reaching the
mock Jev backend:

```
just test-lib tidepool 'test(=actor_host::jev_tests::template_lookup_batches_related_declarations_and_recovers_from_jev_failure)'
```

Observed failure:

```
GHC wired-in failure Absent: Arg: body Type: Value In module Jev.Host
```

The retained receipts show that raw lookup and reflection completed. The
failure occurs in the JSON transport before a Jev effect request is served.
`haskell/lib/Tidepool/Aeson/Value.hs` defines the intrinsic anchor as
`encodeValue _ = T.pack ""` with `OPAQUE`. GHC can infer an absent argument
from that body, while the prepared JSON intrinsic requires the argument.
This is an intrinsic/demand-contract finding for the engine work; this branch
does not repair the prepared engine or substitute a test-only codec into production.
The recorded failing run is under
`target/tidepool-test-runs/20260922T080849Z-1753452-battery`.

After the engine repair, run the integration test above to exercise actual
Jev request batching, one-degree expansion, alternatives, and provider failure
fallback. Its deterministic backend provides all scores; no API credentials
are required. Real Jev latency or model-round savings have not been measured.

## Verification boundaries

Focused actor tests cover lookup mechanics, role ceilings, start decoding,
namespace-preserving expansion, capped candidate provenance, and hosted tool
publication. Runtime/compiler checks cover resolved references, exact visible
scope, public-member visibility, and worker receipt decoding. The extractor
request roundtrip includes the new scope-browse operation.

`template_lookup_raw_namespace_and_selection_policy_contracts` independently
exercises the registered tool fixture's raw lookup and pure selection contracts
without requiring the failing JSON transport. The full integration test remains
separate and is not ignored or converted into an expected success.

Passed checks on the delivered revision:

- 45 focused actor tests; four runtime inspection tests; one extractor request
  roundtrip; three MCP schema checks; six protocol generation/validation checks.
- Eight prompt/scaffold tests and the independent prepared raw-lookup test.
- `lookup_tool_policy_matches_native_ghc_oracle`: eight checks against the
  production Haskell policy with a fake Lookup interpreter and deterministic
  selector, including bounded recovery, canonicalization, and failure preservation.
  This does not validate Jev transport.
- The Shoal binary build, shipped template check, and `shoal new` followed by
  `shoal check` in a fresh external repository (`/tmp/tidepool-lookup-smoke.AGmouI`).
- Registered embedded-artifact metadata, Rust formatting, and `git diff --check`.

The template checks used `scripts/dev-shell.sh` with
`scripts/lib-extract.sh`'s `resolve_tidepool_extract` so the worker matched this
checkout. No full verification battery was run during this worktree batch.

Existing workspaces must adopt the template's lookup tool in their agent spec;
see `docs/GETTING-STARTED.md`. The advertised argument shape is the `queries`
object. Ordinary `Introspection.info` and `typeOf` remain raw.
