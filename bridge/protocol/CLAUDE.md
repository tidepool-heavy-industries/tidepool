# tidepool-protocol — effect and control schemas

This std-only leaf owns effect schemas, nominal external references, ModelCall
control envelopes and their generators. Runtime/resource interpretation belongs
to consuming crates. External references own Haskell identity, Rust binding and
an optional leaf module imported by Core. Companion helper imports remain separate.

Native consumers depend on `//bridge/protocol:generated` artifacts. The generator
requires `--output-root` and validates the output roster supplied by the build.
Use `--list` to update `build/protocol/outputs.txt` when schema output membership
changes. Source refreshes and committed snapshots do not authorize native builds.
The production MCP composer exports Core/Authored/Effects through
`//bridge/mcp:effects_generated`; reuse it for Haskell consumers.

Validation includes schema checks, compiled Rust codecs, and the three real GHC
consumer/codec tests in `bridge/mcp/tests/generated_surface_contracts.rs`.
A refusal first establishes a valid compiled or decoded control.

`actor_profiles.rs` owns nominal actor/tool effect keys, defining module/type
identity, delegability and ordered default profiles. Its projections supply
Rust and Haskell sums, reflection and row witnesses; runtime interpreter
support and resource grants remain independently observed. External nominal
entries use `ExternalType`, including handwritten Replies and Watches.
Declaration origin distinguishes generated Core schemas, MCP's handwritten
FsRead/FsWrite definitions and external modules. A defining module alone does
not identify the declaration producer; real GHC reflection validates emission.
The legacy Rust `Forks` spelling remains decodable/re-encodable for retained
contracts, but has no nominal Haskell identity, witness, default membership or
installed interpreter. Authored launch uses AgentLaunch.
