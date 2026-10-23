# World-primary and selected TYPE API

Source base: `196b6c43ea`. Core owner branch: `work/c-completion-owner-20261022`.

`SelectedTypeInterfaces` is public with private `support: CompilerSourceSupport`
and `producer: [u8; 32]`. Producers return this owner:

- `ValidatedOriginalSource::project_type_interfaces(...) -> Result<SelectedTypeInterfaces, CompileError>`
- `ExactDeclarationContext::select_original_type_interfaces(...) -> Result<SelectedTypeInterfaces, CompileError>`
- checked-value and host-prototype issuance use the same exact selected projection.

Public observations: `toolchain_identity_sha256()`, `semantic_sha256()`,
`same_authenticated_interfaces(&Self)`, and signature/witness narrowing through
`select(witnesses, signatures)`. Checked-row issuance belongs to the selected
owner through private `from_checked_value` and `from_checked_value_signature`.
Authenticated comparison includes exact dependency facts. Semantic identity is computed only when
consumed. No context, projection, source or execution escape is public.
`ExactDeclarationContext::extend_type_interfaces(self, &SelectedTypeInterfaces)`
admits the selected TYPE evidence into an existing context.

`ExactHostBindingPrototype::type_interfaces() -> &Arc<SelectedTypeInterfaces>`
replaces `context()`. `from_original_input` consumes
`Arc<SelectedTypeInterfaces>`; its witness is checked against those selected rows.
`ExactCompileContext::declarations()` remains a declaration context: runtime
request preparation must explicitly extend its existing base with TYPE evidence.

`WorldObservationScaffold::new` takes `Vec<Arc<SelectedTypeInterfaces>>`;
`type_interfaces()` borrows that list. `CompilerWorld::with_type_interfaces`
takes `&[Arc<SelectedTypeInterfaces>]`.

`WorldPrimaryInputs` is crate-private with private coissued
`Arc<RequestCompilerInputs>` and `Arc<ExactDeclarationContext>` fields and borrowed
`inputs()`/`context()` accessors. World-only advance methods live on it. Delta
owners retain `primary: WorldPrimaryInputs` plus changed owners/counters.
Restoration and program continuation retain the pair. The initial
`Arc<WorldNativeDemandCompletion>` and its operation remain fixed while temporary
TYPE overlays and prefix support advance independently. Private promotion still
uses its actually validated context and never manufactures completed output.

Runtime worker owns consumers outside tidepool/toolchain; core owner owns all
files within tidepool/toolchain. Context9 format is unchanged. No cold builds;
joined compilation and execution use the root warm lane.
