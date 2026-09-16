# Synthetic reply sites: ordinary effects on the prepared route

Closes the ordinary-effect obligation `continuation-2026-09-15.md` records at
lines 88-89 and 198-203. `designs/prepared-parking.md` stays the governing text
for the frame, evidence and invocation interfaces; this refines only *where the
site evidence comes from* when the request carries no dynamic `typedSite`.

## The gap, in source

- `sitedVerbs` (`haskell/src/Tidepool/EffectSchema.hs:96-155`) is sixteen rows.
  `lookupPreparedVerb` (`PreparedSites.hs:264-271`) matches an occurrence only
  against that table, so only those verbs are rewritten onto a sited sibling
  and only they mint a `YieldSite` (`PreparedSites.hs:193-205`).
- `lowerPreparedEvidence` (`ExecutionProjection.hs:509-554`) emits one `SiteRow`
  per `PreparedSite` owned by a retained executable top. No `PreparedSite`, no
  row.
- `try_park_suspension` reads the site id from the observed request's rendered
  JSON (`typed_site_of`, `prepared.rs:717-736`) and refuses an absent one with
  `PreparedRuntimeError::UntypedRequest` (`prepared.rs:1193`).
- So `say`, `readFile`, `kvGet`, `httpGet` and `ask` — whose requests are
  ordinary effect-GADT constructors, `Print :: Text -> Console ()`
  (`tidepool-mcp/src/lib.rs:445`), `readFile = send . FsRead`
  (`effect_defs.rs:1239`) — park on Core and refuse on prepared.

Everything downstream of the site id already works. `answer_plan` reads only
`row.delivery` and `row.wire` (`prepared.rs:1325-1338`); `lower_answer`
constructs `Data`, `Scalar`, `Text`, `Integer` and `Natural` answers
(`prepared.rs:343-407`), proven by `prepared_turn.rs`'s Bool, data/Maybe and
byte-backed dual-runs. A `()` answer is an ordinary nullary `Data` row.

## Decision 1 — generic, at the request constructor, not per verb

The projector emits a site for **every interned constructor whose
`dataConOrigResTy` is a saturated application of a type constructor to at least
one argument whose last argument is closed**. That last argument is the reply
index: `Console ()` gives `()`, `FsRead' (Either FsError Text)` gives
`Either FsError Text`, `Finalize v a` (`eval_prep.rs:138`) gives `a` and is
skipped as open. `dataConOrigResTy` is already read at
`ExecutionProjection.hs:1247` and `:621`, so no new GHC evidence is needed.

This deliberately does not test effect-row membership. An extra row for a
non-effect GADT costs one type-graph node and is never looked up; a missing row
would silently reproduce today's refusal. Breadth is the safer error.

**Correction to the proposal.** Request GADT constructors are *not* vanilla:
`TypePolicy.unsupportedConstructor = not . isVanillaDataCon`
(`TypePolicy.hs:214-215`) refuses `Print` in the `algebraic` branch
(`TypePolicy.hs:146-155`). This is not a blocker, because we never intern the
*request* type — only its reply index, which is an ordinary type. Do not call
`internType` on `dataConOrigResTy`; call it on the last argument.

**Correction to the proposal.** `inputs` must be `[]`, not the constructor's
argument types. `ysInputs` is documented as "only live inputs an interpreter
must mount back into a typed workbench" (`EffectSchema.hs:27-31`), and every
`DeliverHostAnswer` verb declares `vsInputTypeArgs = []`
(`EffectSchema.hs:98-121`). Inputs widen the type-graph reachability roots
(`ExecutionProjection.hs:528-529`) and have no consumer: `answer_plan` reads
`row.wire` alone (`prepared.rs:1338`). A `kvPut` request argument of type
`Value` would drag an unconstructible subgraph in for nothing.

## Decision 2 — reuse `SiteRow`; add one small side table

**Correction to the proposal.** Do not introduce a `VerbSiteRow` carrying its
own `wire`/`inputs`. That would fork the site vocabulary and force parallel
changes in the engine site index (`prepared.rs:760`), the structural
`SiteConflict` dedup, `PreparedFrameEvidence.site` and `answer_plan`. The least
invasive shape keeps one row type:

- Synthetic rows go into the existing `sites` list, with
  `delivery = HostAnswer`, `wire` = the reply index's `TypeNodeId`,
  `inputs = []`, `origin` = the request constructor's qualified identity,
  `ordinal = 0`. `check_sites` (`validation.rs:1756-1779`) validates them
  unchanged.
- One new table beside it: `verb_sites: Vec<(ConstructorId, u64)>` — the
  request constructor and the site id whose row answers it. Encoded as a new
  trailing field (`ExecutionEncode.hs:33` array position 15), decoded beside
  `sites` (`codec.rs:216`), validated by checking each `ConstructorId` is
  declared and each `u64` names a row already admitted by `check_sites`.

`PreparedFrameEvidence.site` stays `u64` (`designs/prepared-parking.md:51-57`).
No enum.

**Site id range.** Dynamic ids are `max 1 (fingerprint .&. 0x7fffffffffffffff)`
(`PreparedSites.hs:297-302`), so the entire top half is free. A synthetic id is
`0x8000000000000000 .|. (fingerprint of the constructor's qualified identity
 .&. 0x7fffffffffffffff)`. Non-zero by construction, which `check_sites`
requires (`validation.rs:1767`). Identity-derived, so two programs compiling the
same `Print` agree and the install-time structural equivalence check accepts the
duplicate with the existing owner canonical.

Schema bump `SCHEMA_VERSION` 10 -> 11 (`execution_schema.rs:9`), matched in the
Haskell encoder, then `just fixtures-update`.

## Decision 3 — runtime classification

**Correction to the proposal.** `DescriptorInterner::by_host`
(`tidepool-codegen/src/prepared_program/interner.rs:215`) is not involved. By
the time the site is needed the request has already been observed into a bridge
`Value` (`prepared.rs:1187-1190`), so its outer constructor is directly
available as `Value::Con(host_id, _)`, and `ProgramFacts.constructors` already
carries `ConstructorId -> (SymbolIdentity, DataConId)` (`prepared.rs:241`,
`:316-320`).

The owner is not known before classification, so the verb index must be
machine-owned exactly as `sites` is: a second map
`verb_sites: BTreeMap<DataConId, SiteWitness>` on `PreparedEngine`, extended in
the same install transaction, conflicting duplicates refusing the install. The
new step at `prepared.rs:1193` becomes: read `typed_site_of`; on `None`, take
the request's outer `host_id` and look it up in `verb_sites`; on a miss, keep
`UntypedRequest`. Everything after (`witness`, `PreparedFrameEvidence`, `park`)
is byte-identical.

The linchpin: `resume_unit` builds its answer as `().to_value(table)` from the
*session* `DataConTable` (`tidepool-actor/src/resident_workbench.rs:4107-4120`),
while `lower_answer` matches `Value::Con`'s `host_id` against the projected
row's declared `DataConId` (`prepared.rs:363-373`). Both are the same
`varId`-minted bridge id (`execution_schema.rs:290-292`), so no translation is
needed and `answer_plan`/`lower_answer` are unchanged.

## Decision 4 — taken (2026-09-16): `Value`-carrying replies use a leaf adapter

Correction to the original premise: `Value` itself classifies as an ordinary
`TypeNode::Data` (module `Tidepool.Aeson.Value`; `Array` is `[Value]`, so no
`Vector`). Only its `Object` row is unconstructible: `type KeyMap v = Map Key v`
expands through `coreView` to `Data.Map.Internal.Map`, refused at
`TypePolicy.hs:243-248`. Scalar `Value`s (`String`, `Bool`, `Null`) and
`Nothing`/`Left` already build; `Number` (`Scientific`, strict unpacked fields)
and `Object` refuse with `AnswerUnconstructible`, frame intact.

Decision (Claude, at the user's direction, sol not in the loop): option 1 as
a **leaf adapter**. `lower_answer` treats a `TypeNode::Data` whose family is
`Tidepool.Aeson.Value.Value` as one leaf: the host renders the bridge value to
JSON text, builds it as the byte-backed `Text` `lower_text` already produces,
enters the program's `__decodeValue :: Text -> Either Text Value` root (the
same `eitherDecodeValue` every program can already call; a third auxiliary
root beside `__prepared`/`__resume`, admitted by name in `Main.hs`'s
`prepareArtifacts` list, no schema change), and splices the returned handle
into the outer answer as an `AnswerPlan::Handle` field. All adapter leaves are
materialized and rooted before the outer structure allocates; a `Left`, or a
refusal anywhere, releases them and leaves the frame parked. The host builder's
type vocabulary stays closed; `Map`'s balance invariants are the program's.

The rest of this section is the original analysis, kept for the record.

`kvGet :: Maybe Value` (`effect_defs.rs:1113`), `httpGet :: Either HttpError
Value` (`effect_defs.rs:806`) and `ask :: Value` were described as
`TypeNode::Unconstructible` because Aeson's `Value` nests `KeyMap` over
`Data.Map.Internal.Map` (refused at `TypePolicy.hs:243-248`). `lower_answer`
then returns `AnswerUnconstructible` (`prepared.rs:400-405`) — a clean refusal
with the frame intact, not a crash.

Two options, both one-way doors:

1. **Compiled adapter entry.** The turn artifact admits a third root beside
   `__prepared`/`__resume` that takes a `Text` and returns a `Value` by running
   the program's own `eitherDecode`. The host answers with `Text`, which
   `lower_text` already builds (`prepared.rs:439ff`), and the adapter is entered
   before the resume. Keeps the host builder's type vocabulary closed; costs one
   more generated root and one more machine entry per KV/HTTP/ask reply.
2. **Extend the host builder to Aeson.** Teach `TypePolicy`/`lower_answer` the
   `KeyMap`/`Vector`/`Map` spines directly, as Text-keyed association lists and
   list spines. Removes the extra entry; permanently commits the host answer
   builder to knowing a specific library's internal representation, which is
   what `isForbidden` currently exists to prevent.

Recommendation: option 1, on the grounds that it keeps the builder's refusal set
intact and reuses the byte-backed `Text` path that already landed. **This is
sol's decision, not this note's** — it fixes the shape of the host/program value
boundary for every future container type.

## Decision 5 — "generated `settleEff` forcing"

The phrase in the continuation map (lines 88-89, 198-203) is not about the
runtime templates. Those already work: `prepared_scaffold_binding`
(`tidepool-runtime/src/session/turn.rs:467-472`) emits
`__prepared = TidepoolResume.settle __result` and
`__resume q x = TidepoolResume.settle (TidepoolResume.resumeLifted q x)`
(`Tidepool.Session.preparedResumeTargetName`, `Session.hs:355-356`), and
`settle` forces a turn to exactly one constructor layer the host reads without
walking freer data.

The obligation is that the **production effect generator** — an authored
harness's own top, not a session turn module — be projected through the same
settlement scaffold, and that its forcing keep `CoreValue` distinct from
policy-authorized live payloads (line 202). For this design's slices that costs
nothing extra: `LivePayloadPolicy` is already a park argument
(`prepared.rs:1103`, `:1140`), and `answer_plan` admits only
`SiteDelivery::HostAnswer` (`prepared.rs:1332-1337`), so a `LiveReentry` verb
cannot be answered by this path even if it acquires a synthetic row. Ordinary
`()`/`Text`/`Either` replies need nothing beyond what `Bool` answers already use.

## First slices and ownership

**Slice A — `say`.** `say "hi" >> pure (42 :: Int)` on the prepared route
through `ResidentSession`: park on the `Print` request, host answers `()`, the
turn completes with 42. Dual-run in `tidepool-runtime/tests/prepared_turn.rs`,
mirroring `tidepool-harness/tests/outer_effects.rs`'s Console round trip
(`outer_effects.rs:1-8`). Assert the parked request carries no `typedSite` and
that the session's handle and parked counts return to their pre-turn values, as
`notebook_suspension` does (`prepared_turn.rs:383-402`).

**Slice B — `readFile`.** `Either FsError Text`: the `Right`/`Left` rows and the
`Text` leaf are all constructible today. Same bundle, same compile — do not add
a second extractor compile for it.

| Parcel | Files | Scope |
|---|---|---|
| Haskell projector | `ExecutionProjection.hs`, `PreparedSites.hs`, `ExecutionSchema.hs`, `ExecutionEncode.hs` | synthetic rows, `verb_sites`, id range, encoder |
| Rust codec | `tidepool-repr/src/execution_schema{.rs,/codec.rs,/validation.rs}`, `tests/execution_schema_codec.rs` | schema 11, decode, validate, limits |
| Runtime classification | `tidepool-runtime/src/session/prepared.rs` | `verb_sites` index, install transaction, `try_park_suspension` fallback |
| Tests | `tidepool-runtime/tests/prepared_turn.rs` | slices A and B, dual-run |

Each parcel is a separate Sonnet assignment; the codec parcel must land before
the runtime parcel compiles.

## Acceptance

1. Slice A and slice B pass on both engines with identical results.
2. A request whose reply index is unconstructible (`kvGet`) still parks and
   refuses the answer with `AnswerUnconstructible`, frame intact — proving the
   deferral is a clean refusal, not a gap.
3. Two programs declaring the same effect GADT install without `SiteConflict`.
4. A synthetic id never collides with a dynamic one (high bit set, asserted).
5. `just fixtures-update` after the producer change.
