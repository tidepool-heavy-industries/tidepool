# Assignment 01 — share checked interface rows

Base: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. Future implementation assignment; this document reports no executed checks. Integrate only after exact-candidate independent review and repair, following [the supervisor waves](README.md).

Primary goal: a warm checked-cell baseline borrows the exact immutable interface parts already issued for its selected values. It writes, hashes and acquires new output bytes once at their owning boundary; it does not reconstruct all baseline `.hi` files in another request directory. Secondary goals are independent value lifetime and preserving the complete native-selection contract.

## Owning source and production path

| Owner / symbol | Current behavior to change or preserve |
|---|---|
| `tidepool/toolchain/src/checked_cell.rs`: `CheckedValueArtifact`, `ValueInterfaceBytes`, `ValueInterfaceDirectory` | Certificate, producer/receipt, selected support, lexical/template evidence, exact bytes/path/digest and directory lifetime remain coupled. An ordinary value owns more than an interface payload. |
| Same: `CheckedValueInputs::capture_checked`, `write_input`, `baseline_authorization`, `import_authority` | Current capture compares byte/certificate vectors, writes each old interface into fresh TempDir and hashes it; authorization already names absolute per-value paths. Replace baseline duplication through an issuer-owned exact row reference. |
| Same: `CheckedValueImportAuthority::{capture,matches_interface,validate}` | Current path-based authority rereads/hashes old bytes. Change its owner-issued input, not just delete reads against mutable paths. Receiving metadata/role checks remain. |
| Same: `capture_produced_types`, `capture_output` | New generation outputs are read from this request's staging root and matched to exact receipts; P6 adds source/template/native evidence. Do not reuse old input paths as writable outputs. |
| `tidepool/toolchain/src/declaration_context.rs`: `RetainedArtifactRow`, `RetainedArtifactMaterialization::{select_custody_inputs,selected_rows}`, `materialize_retained_artifacts`, `input_transport_files` | Existing exact physical rows and immutable part transport are the owner to extend. Materialization handles `ArtifactPayload::Interface`, including values. Reuse selected row handles without retaining broad parent metadata. |
| `tidepool/toolchain/src/owned_input_arena.rs`: `OwnedInputArenaBuilder`, `OwnedInputSlice` | Append/hash new bytes before sealing; borrowed slices retain actual file/backing owner. Do not append the same old parts every request. |
| `tidepool/runtime/src/session/{admission,turn,inspection}.rs` and `session/binding_table.rs` | Admission and next-offer consumers retain checked certificates; binding index rejects equal bytes from different issued authorities. Coordinate the new selected-support reference with host-inputs so raw parallel vectors do not recreate authority. |
| `bridge/haskell/src/Tidepool/ExactScope.hs`: `bindCapturedPurposeValues`, `scopeInterfaceToken`, `readScopedInterfaceClosure` | Owned transport already aliases protected value paths to captured materialized rows after exact owner/seal checks. Preserve exact scope membership and typed checked-value evidence. |
| `bridge/haskell/src/Tidepool/{GhcPipeline,Session,SessionArtifacts}.hs` | Checked injection uses each artifact's `exactPath`; source-less finder registration takes that path. `prepareTypedSegmentSessionBindings` requires fresh staging and refuses existing generation outputs. Legacy unchecked root-relative injection is a different contract. |

Read nested rules before implementation. Related `declaration_context/original_inputs.rs` and extraction original-input transport schemas are shared with 03/warm-workers; assign an edit owner before changing them.

## Input and output contract

Input is host-inputs' opaque issued exact selection plus selected checked value artifacts; no broad artifact census becomes a substitute. A reusable row binds:

- the exact logical interface owner/certificate and required parts;
- the existing acquisition issuer, including per-view absence overrides;
- immutable bytes and their issuing digest/length, plus actual physical row/slice/file leases;
- supporting source/type/template/native facts by their already issued exact references, without promoting them into lexical roots or executable demand.

Do not make every field publicly constructible. Reuse private domain constructors and `CompilerSourceSupport` rather than introduce a cache inventory that can assemble arbitrary projections, offers and views. Equal payload or interface digest cannot replace a checked value's receipt or dependency selection. A row may be shared physically without sharing those permissions.

Output to offer/registration is the selected baseline row references plus one fresh output-only staging owner. Requests may serialize stable existing absolute baseline paths and borrow their immutable part transport. `checked_value_root` may continue selecting fresh output staging; it must cease implying baseline files live there on the checked path. Output generations may not collide with any baseline owner. Failed/canceled staging never publishes a row or successor selection.

Seal each successful new value into independently retained physical ownership. Prefer a value-sized leaf owner containing that value's interface and associated immutable parts, sharing required dependency rows. Do not make a value borrow the entire producing request directory indefinitely. If promoting a new output out of mutable staging requires a copy, perform it once for that new output and count it; never label this as zero-copy. Existing artifact materialization is the implementation owner for that transfer. 03 determines final file/arena granularity and FD budget before this parcel is called retention-complete.

## Wave A — baseline borrowing and early joined history

Trace every `capture_checked` caller and every checked-root consumer, including lookup/inspection, renderer offers, host-built interfaces and failure diagnostics. Classify raw/legacy paths explicitly. Introduce the minimum private borrowing operation at the existing materialization owner and consume its issued row in checked input capture. Preserve current duplicate/missing/excess certificate and byte-mismatch refusals for external/unissued inputs; internally issued exact roots should not rehash immutable bytes merely because parallel raw vectors survived an older API.

Have host-inputs replace or justify those parallel vectors through the real admission boundary. If the reference does not establish the same selected certificates and bytes, do the necessary admission instead of treating Arc equality as authority. Preserve exact component bounds, cancellation and inaccessible-versus-missing distinctions.

Run a small real checked-cell history immediately: value output → published binding → next checked cell using its type/value → lookup/native renderer consumer. Assert unchanged baseline path/body issuance, fresh new output path, exact binder/type identities, and no old-baseline filesystem writes or hashing. Do not wait for package pinning or full worker snapshot handles to prove this path.

Independent review focuses on actual selected producer-to-consumer ownership. Repair and fold this parcel with a testable counter baseline before adding retention optimizations. Source review and shallow-clone assertions are insufficient.

## Wave B — independent value files and alternate consumers

Finish `capture_output`/host-built output promotion so one retained value no longer keeps copied baseline or unrelated sibling files solely through `Arc<ValueInterfaceDirectory>`. Integrate with 03's actual backing ownership. The certificate/path/prototype and selected source/template evidence must survive independently when the producing cell and siblings disappear. Keep output receipt-seal verification before publishing physical ownership.

Update diagnostics to report expected selected bytes and observed actual transport separately. Missing diagnostic copies cannot authorize reacquisition or broaden selection. Remove checked baseline `write_input` calls and stale root-relative assumptions from all production checked consumers; raw fixture/legacy paths may remain only with distinct purpose and explicit tests. Do not leave a public “fast” and “safe” checked API where callers can accidentally choose weaker authority.

After fold, independently inspect runtime turn, inspection, renderer, recovery and worker registration joins for a surviving baseline-copy route. A row created only by the new unit test is not accepted.

## Semantic and cost acceptance

Use real compiler-produced value interfaces and owning request/acquisition operations. Add maintained history/property coverage with shrinking where histories expose contributor/lifetime behavior; retain independent semantic expectations. Include:

1. Repeated same issued value, duplicate module with a different receipt, equal bytes from a different issuer, mismatched requested bytes and excess/missing selected certificates. Issued identity survives; reconstructed authority refuses.
2. Explicit Original A versus equal-interface retained Original B, in both winner orders. B does not become optional direct input. A genuinely required historical B child retains its exact edge/permissions and still refuses if it conflicts with the explicit consuming selection. Keep the two mandatory handoff regressions in README and exercise this through next-cell/worker admission.
3. Two baseline values from different physical directories and a fresh output root. The checked worker hydrates exact `scopeValueInterfaces`, registers stable input paths and writes new generation outputs only in staging. Collision, missing/unreadable part, wrong length/seal/owner and unavailable lease refuse distinctly.
4. One output with nominal type dependencies on another old value/declaration; partial-name shadowing, qualified old generation and moved value survival. Thin value evidence does not create original instance completeness or authored source imports.
5. Multi-output producer → retain one value elsewhere → drop producer and siblings → next compile/display → drop last borrower. Assert selected semantic support and exact physical survival/release, not merely Arc counts.
6. Cancellation during output capture/promotion, END uncertainty, failed runtime publication and stale settlement. Existing valid baseline survives; no fresh failed row or name is selected. Retry does not rerun effects or treat an indeterminate accepted request as a cache miss.

Add owner counters for baseline files/bytes written, baseline bytes hashed/read, immutable row/arena parts appended, fresh output bytes and receipts, selected baseline references and physical rows issued. For unchanged input selection, repeated requests must add zero baseline file copies and zero reauthentication byte work against owned immutable transport; genuine new output work is separately nonzero. Vary irrelevant sibling/history size independently from selected scope size. Accumulate request, P6 promotion and later materialization, proving the copies were not moved to publication.

Compile the changed toolchain, runtime and Haskell consumer/test targets through pinned admitted commands. Discover exact native labels before selecting leaves. Run the smallest counted regressions plus the real actor/shared-workspace workflow at the folded candidate. Record actual selected/executed counts and source OID; no zero-test or link-only pass.

## Deletion, handoff and escalation

Delete superseded checked-baseline copying and duplicate host rehash mechanisms once the issued row route covers production consumers. Preserve raw input authentication where it remains necessary, with explicit purpose. Remove tests whose sole expectation is old scratch layout, replacing them with semantic/lifetime/physical-work evidence. Keep real damaged-file/diagnostic refusals on the path that actually reads mutable files.

Return candidate/base, complete touched-path diff, new ownership contract, executed history, growth counters, remaining raw paths and cleanup evidence to supervisor and warm-workers/host-inputs. Stop on unproven row provenance, a consumer needing baseline-root-relative writes, unavoidable whole-producer retention, ambiguous receipt/selection association, or a required external schema change without the joint contract. Escalate rather than weaken selection/namespace validation or reissue missing availability from custody.
