# Persona executable-seed delivery

Branch: `companion/persona-executable-seeds`, isolated in
`/srv/swarm/checkouts/tidepool-persona-seeds-20261012`.
Base: `6c6c3d4d72d6d7719432b49df8c3abd7dd437f6f`, the merged integration
checkpoint. Published main still predates this persona integration. The shared
interactive checkout, live configuration and services were not changed.

## Changes

- `Persona.Orientation` is the editable policy now installed at startup. It
  retains preparation evidence before returning the initial context text.
- `Persona.AttentionNotebook` supplies a small instruction language, its
  stateful interpreter, an existing record-actor host, and a form that returns
  those same instructions. Actor startup remains an explicit choice.
- `Persona.Context.activateInitial` connects preparation to the existing
  startup publication receipt and synchronous context-editing effect.
- The prompt presents working modules as seeds, assumes Haskell fluency, and
  judges abstractions through the broad families of programs they express.
  It accurately describes the current source-editing limit.
- Native test rosters include the new modules. The hosted startup fixture now
  checks the real saved inspection as well as initial context publication.

The graph generator also found three existing lifecycle modules missing from
Buck's source roster. Their nine generated entries are isolated in prerequisite
commit `45bc3b8456fc37f084fe07d0e4b0e6d47364249a`; a coordinator whose base already
registers them can skip that commit. No lifecycle implementation was modified.
Cargo dependency resolution and browser asset pins are unchanged.

## Executed verification

`swarm-build just test-native //bridge/haskell:model_turn_test --pattern persona`
compiled the persona modules and concrete actor composition, selected and
executed 37 tests, and exited 0. The existing property case ran 100 examples.
The suite covers the instruction interpreter, retained draft/result distinction,
form submission into the same interpreter, evidence construction, persistence
requests and failure, and unavailable-startup refusal. It uses the configured
pinned native workflow with local Buck execution.

The GHC test binary SHA-256 is
`f32d237bd0785d6df3515e90d96edffadebe335a86d6144699e11384a07715a5`.
The retained log is
`/srv/swarm/evidence/inanna/persona-seeds-tests-20261012-final.log`.
Configuration/module paths, touched documentation links, native generated input
registration and whitespace were checked. The changed Rust fixture was formatted
through the pinned development shell.

## Hosted validation blocker

Attempted command:

```sh
swarm-build just test-lib tidepool --exact actor_host::startup_persona_acceptance_tests::shipped_persona_initializer_uses_actual_preparation_before_first_request --expected-count 1
```

The build exited 3 before the test could run: the configured Harness fetch
refused pinned revision `8c00038ff48cf3e5f153b8087d6c04fa1a14d886` with
`upload-pack: not our ref`. Requested count: 1; executed count: 0. Evidence:
`/srv/swarm/evidence/inanna/persona-seeds-hosted-startup-20261012.log`.
The dependency was not replaced or repinned. Resume this command after the
coordinator makes the matching dependency available or integrates this feature
onto their qualified bundle.

Plain GHC has a stub for the native JSON encoder. The focused tests separately
check the production inspection value and write effect; they do not prove the
persisted JSON bytes. The hosted fixture contains that assertion but remains
unexecuted. The optional actor composition is compiled, not live-executed.
No live Tavily/Jev/persona trial or frozen-bundle qualification was performed.

## Remaining source capability

The [source-editing handoff](SEEDS.md#source-editing-handoff) identifies the
existing source/module/reload mechanism to connect. Persona can currently write
state and create notebook definitions, but cannot directly write its authored
source or actor-local SessionHelpers files. This requires the Exomonad owner's
integration; the persona implementation does not widen runtime grants.
