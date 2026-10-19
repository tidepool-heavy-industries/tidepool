# World source admission adapter

Base: `e608274ca8d0493eb9b1114e2f7e21f9e8b702e0`. Branch: `feature/shared-warm/world-source-admission`.

This first mechanical parcel borrows the exact existing source-support Arc, metadata, native selection and retained-original membership owner through `ProductCompilerInputs`. It factors the shared source/dependency/import/selection payload decoder after the unchanged exact receipt header, request SHA, semantic SHA and snapshot checks. It introduces no world source admission or replacement compiler selection.

Paired external consumer changes belong to Warm: `CompilerSourceSupport::from_original_output` takes the borrowed support instead of `RequestCompilerInputs`; `CertifiedSourceSelection` needs the same common input view, retaining its old exact entry as a borrowing wrapper. This parcel is not independently compiled until those changes are joined.

The world adapter must retain the actual reservation and actual decoded typed-segment owner. Its named refs resolve exclusively against the output owner’s captured named-file map and retained Arc bytes. Its actual demanded compiler/native/source basis remains pending Warm’s existing world-selection owner; no full request, default empty authority or custody-derived native permission is permitted. Protocol is factoring selected-source qualification from genuinely fresh scaffold evidence at runCompileCycle; the existing fresh-owner collision refusal remains unchanged.

No native commands have run in this checkout (native grant zero). Storage provisioned with a real per-checkout buck-out bind mount. Pending after paired integration: pinned local `-j1` production and cfg(test) toolchain consumers; `swarm-build just check tidepool-toolchain` selects the owning check graph, and `swarm-build just test-lib tidepool-toolchain --exact declaration_context::tests::exact_product_binding_preserves_lexical_evidence_and_request_identity --expected-count 1` is the existing exact identity/refusal oracle. Discover labels and counted names through admitted owning recipes before execution; compilation and execution remain unqualified.
