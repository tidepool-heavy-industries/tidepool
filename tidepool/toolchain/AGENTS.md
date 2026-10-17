# Toolchain and compilation policy

This crate owns toolchain/stdlib discovery, deploy compatibility, shared paths,
compile diagnostics, and the one policy-bearing artifact compile front door.
It sits below `tidepool-runtime` and must not depend on runtime/session errors.

- All compilation policy enters through `artifacts`; do not add an alternate
  frontend or an invocation-output memo alongside the physical compile path.
- Acquire required named outputs from the ordered request through the typed
  physical-output owner. Native products require their paired package sidecar;
  portable output never grants native certificate authority. Keep missing and
  inaccessible members distinct, decoding bounded and cancellation a refusal.
- Compiler evidence owns consumed bytes and import-resolution witnesses.
  Authored dependencies retain path identity; generated source markers exclude
  scratch-directory identity. Native candidates, deployment catalogs and retained
  entries validate that evidence through their own certified reuse boundaries.
  CPP, TH and other untracked inputs cannot authorize replay without complete
  evidence; completed source observations remain distinct from replay eligibility.
- `CertifiedSourceSelection` owns compiler roles. Full artifact custody and
  `recovery_products` may retain several native versions of one module;
  authored consumers use the issued `SelectedOriginalClosure`. Validate its
  native dependencies through exact artifact/group edges, including historical
  children, rather than flattening custody into a module map. History tests
  must carry certification through the production consumer that needs selection.
  A native group can retain an exact issued carrier without an Original entry
  in the compiler namespace. Test that case alongside explicit namespace choices
  and genuinely competing group carriers. A broader artifact lookup must not
  discard an existing exact selection or silently override an explicit choice.
  Retaining a complete native carrier must preserve its available exact child
  carriers for unselected bodies. Keep that byte custody separate from source
  roots, compiler input roles and executable group demand. Exact custody seeds
  retain artifact or group nodes through snapshots and recovery;
  only issued roots and their closure select executable groups. Later requests can
  select another body without recreating its issuing dependency versions.
  Complete authored group custody can issue private native availability only
  through one recorded original carrier paired to the selected canonical binding.
  Artifact-only custody and unions across carriers cannot supply that proof.
  Availability preserves exact dependency graphs without selecting executable
  groups or restoring source, lexical or instance authority.
  Published originals retain their issuer's immutable artifact/group selection
  separately from later executable demand. Reopening and recovery validate that
  proof against exact retained custody; a broader inventory cannot recreate it.
  Equal immutable artifact payload IDs do not imply equal exact dependency
  bindings. Each issued graph binding owns its canonical children and native
  edges; reuse bytes without borrowing another selection's ambient owner map.
  Value-interface extensions carry their issued compiler roles and selected
  native issuer before joining custody. Other captured source offers keep their
  exact graphs as custody rather than competing native roots for equal payloads.
  Retaining original byte custody copies owned binding nodes, not payload roots
  that would require a new namespace choice.
  Native group nodes have their own issuing scope. Their explicit carrier edge
  may target an artifact binding from an earlier selection; never manufacture
  an artifact node from a group's scope. Validate typed artifact/group node
  membership, and follow the recorded carrier edge when selecting group demand.
  Live views traverse only authenticated immutable scope snapshots. Each view
  owns a persistent payload map filtered to its selected and hidden-custody
  closure; proof-only ancestors retain no sibling payloads. The inventory may
  weakly intern equal semantic facts, never acquisition witnesses or payload
  authority. Issued views merge their scope and payload owners directly; raw
  decoded recovery facts still require authentication against certified bytes.
  Recovery records scoped bindings, complete binding digest witnesses, exact
  interface/native edges and executable group demand, and validates them against
  the certified payloads before restoring compiler roles. A completed source-compilation instance
  proof remains transaction-owned rather than reconstructed from durable bytes.
- Pure activation renderers preserve the original instance graph separately from
  native availability. Authored dictionary bodies use retained originals matching
  that capture's canonical interfaces; type custody alone cannot replace native
  bytes, and source-original Core recovery must not replay authored declarations.
  Renderer evidence contains no mounted-value or invocation authority. Fresh
  child interfaces and runtime admissions remain independent of specialization.
  Only the opaque display issuer may exclude value roots outside the original
  instance graph, sealed input closure and retained dependency graph. It retains
  complete canonical, family, orphan and exact native capture evidence. Sharing
  compares those authenticated facts after digest lookup; a nominal type or an
  equal digest cannot establish membership in a display owner.
- Validate compiler completion against the issued request roles, including private
  native availability. Its persistent declaration snapshot retains independent
  lexical and type roles and cannot validate a promoted request artifact closure.
- `tidepool-extract-cmd` stays the small invocation builder. Do not move cache
  or runtime policy into that dependency leaf.
- Native reuse tests cover invalidation, relocation, warnings and refusal as
  well as successful acquisition through the production consumer.
- The resident compiler owns dependency-validated module memoization, including
  immutable support reused across session requests. Runtime module candidates
  and retained entries own certified artifact reuse. Use matched measurements
  as evidence for costs and savings, not projected estimates.
- `build_prepared_fixture` is the build-action mode of that same front door.
  It requires action-owned current-directory scratch and declared source roots,
  uses the configured direct compiler, and has no runtime cache authority.
  Export portable prepared programs and metadata only; source-bound compiler
  certificates never move into fixture resources or get restamped.
- `build_deployment_module_package` shares build-action isolation and exports
  authenticated module records through the existing catalog owner. A declared
  probe and ordered targets select the cohort; deployment export never grants
  runtime candidate publication. Schema 4 keeps original source paths and
  proofs unchanged; product references resolve under the opened catalog's
  canonical parent. Source retention remains the qualification owner's job.
- Runtime retained entries may borrow only the admitted deployment catalog.
  Persist dependencies from the certified borrowed-original roles and exact
  native/group selection. Identity-equal freshly produced bytes remain owned
  by the entry. Reopening authenticates that selection against its retained
  catalog; available inventory cannot choose dependencies or replay source.
