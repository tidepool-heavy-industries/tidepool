# TYPE frontier repair after Argon review

Base: `a57377524cce6fa9179fac70316524efadbee44a` (code `5e10df3bef`).
Prior evidence: `/srv/swarm/evidence/inanna/g2-value-type-20261024/`.
New evidence: `/srv/swarm/evidence/inanna/g2-type-frontier-repair-20261024/`.
Status: implementation and verification in progress; G2 remains unqualified.

Argon's report is retained as a hashed input in the new evidence directory.
Its proposed code snippets are illustrative: actual signatures, ownership, and
entry points at this revision govern implementation.

## Repairs and preservation requirements

**C: producer facts and inventory facts.** `issue_selected_custody` captures the
incoming producer facade before canonicalizing its immutable facts owner. Validate
the existing retained merge and metadata conflicts, then align the incoming
facade's facts to that retained owner before recording producer entries. Preserve
the incoming Original admission/transport/custody; do not substitute a previous
producer facade. Keep `same_issued_facts` and exact binding checks unchanged.
Controls must distinguish independent equal allocations from competing facts,
and prove producer authority survives canonical facts reuse.

**A: operand TYPE remains stage-borrowed.** Remove the operand TYPE merge from
`WorldNativeDemandResolver::new`'s persistent primary. The existing checked-value
authority already includes operands in per-stage TYPE selection. Prove operand
types reach the stage without entering the program's primary namespace, and rerun
the genuine operand actor and captured-source overlap cases. Do not relax SOURCE
admission or promote TYPE into executable authority.

**B: standalone package definitions.** Package-group emission must neutralize
the consuming entry and auxiliary roots. Package roots seed reachability, not
standalone definition evidence. Preserve actual target auxiliary answer evidence.
Strip leading forall binders before testing auxiliary result free variables.
Compare exact package bytes and definitions hashes across consumer root sets,
including a discriminating nonempty auxiliary set; cover polymorphic and concrete
target answers. Keep HOME/code identity and ordered slot checks unchanged.

**Repeated package-sidecar decoding.** Admission currently decodes then discards
package evidence; retain forced decoded facts on the actual admitted-row owner
and use them during selection. Preserve selected file/semantic correlation and
installed package validation. No second cache or expanded lifetime is needed.

**Decoder retention correction.** The report's reset claim does not match this
revision: `commitWorldInputRetention` already retains the decoder and prunes it
to admitted retained-original seals. `worldRetainedDecode`, `WorldRowFact`, and
`loadRetainedPackageImports` are not current symbols. Investigate live World seal
retention separately before changing lifetime/accounting; do not implement a
fictional reset removal or retain retired interfaces without a charged owner.

## Ownership and verification

Rust owner: inventory admission, native-demand primary, owning Rust controls.
Package owner: ExecutionProjection, CompilerRequest, PreparedRecovery tests.
Row owner: ExactScope and actual package/decoder consumers, RequestInputs tests.
Root: integration, source review, registration, joined gates and evidence ledger.
Workers preserve existing branches and use separate mounted checkouts.

Compile production and affected tests early; run focused tests that fail at the
old boundary, then the corrected actor nominal/operand pair and five notebook
cases on the integrated candidate. Run required compiler corpus validation after
projection changes. Keep evidence tied to exact sources and executable hashes.

Any new failure is retained with the last successful boundary and typed refusal.
Acceptance requires executed joined histories; a smaller list of known bugs or
component passes does not qualify the release bundle. Update the prior review
packet with final outcomes and remaining obligations, then hand the exact pushed
candidate back to Argon if another ownership decision is needed.
