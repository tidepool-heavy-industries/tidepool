# Self-hosted lookup handoff

The `codex/self-hosted-lookup` branch replaces the special hosted lookup path
with a structured `Lookup` effect and an editable, precompiled Haskell tool.
The workspace template adds one Jev scoring batch over compiler-resolved
references or failed-name alternatives, followed by at most four raw lookups.
Namespace identity and compile-view fencing survive the whole operation.
This checkout's active `.shoal` configuration is not part of the change.

## Joined engine result

The branch originally reproduced the JSON intrinsic demand defect before the
mock Jev backend:

```
just test-lib tidepool 'test(=actor_host::jev_tests::template_lookup_batches_related_declarations_and_recovers_from_jev_failure)'
```

Main repaired that shared contract before integration: the authenticated JSON
anchors now publish conservative demand, and their source is tracked as a
compiler-worker input. The test above passes on the joined revision. It
exercises actual Jev request batching, one-degree expansion, alternatives, and
provider-failure fallback. Its deterministic backend provides all scores; no
API credentials are required. Real Jev latency or model-round savings have not
been measured.

## Verification boundaries

Focused actor tests cover lookup mechanics, role ceilings, start decoding,
namespace-preserving expansion, capped candidate provenance, and hosted tool
publication. Runtime/compiler checks cover resolved references, exact visible
scope, public-member visibility, and worker receipt decoding. The extractor
request roundtrip includes the new scope-browse operation.

`template_lookup_raw_namespace_and_selection_policy_contracts` independently
exercises the registered tool fixture's raw lookup and pure selection
contracts. The full integration test remains separate and is not ignored.

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
