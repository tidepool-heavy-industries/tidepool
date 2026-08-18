# PRD 22 lane 4 — Event survey (pre-work checklist)

Mirrors the Worktree lane (§11 of `22-p1-protocol-scaffold.md`). Written before
any schema edit, per §9 step 2.

## Registry 1 — `tidepool-mcp/src/effect_defs.rs`, `event_effect_def!`

- **type_defs** (11 entries, emission order):
  1. `EventId` — `data EventId = EventId Int deriving (Show, Eq)` — Identity/Int
  2. `SubscriptionId` — `data SubscriptionId = SubscriptionId Int deriving (Show, Eq)` — Identity/Int
  3. `Watch` — Sum, 5 variants, 2 reference `WorktreeId` (CROSS-EFFECT, Worktree's type)
  4. `HeadChangeKind` — Sum, 6 variants, some reference `GitOid`/`[GitOid]`/`[(GitOid,GitOid)]` (CROSS-EFFECT)
  5. `HeadChangeReceipt` — Record, 6 fields, references `WorktreeId`/`GitOid`/`BranchName` (CROSS-EFFECT)
  6. `CommitReceipt` — Record, 7 fields, references `WorktreeId`/`GitOid` (CROSS-EFFECT)
  7. `Tick` — Record, 1 field (`firedAtMs :: Int`)
  8. `RepositoryEvent` — Sum, 5 variants, each carrying `EventId` + payload (`CommitReceipt`/`HeadChangeReceipt`/`Tick`/`Int`/`Value`)
  9. `Observed a` — polymorphic record `{ eventId :: EventId, value :: a }` — **NOT representable** (no type-param support in `TypeShape`)
  10. `Event a` — polymorphic record `{ eventWatches :: [Watch], eventProject :: RepositoryEvent -> Maybe a }` — **NOT representable** (function-typed field, type param)
  11. `instance Functor Event where fmap f e = ...` — **NOT representable** (typeclass instance, no schema vocabulary)
  - No `ToJSON` instances anywhere in this block (unlike Worktree's 7).
- **errors EventError** — 6 variants, all single/double `Text`/`Int` fields. No cross-effect fields.
- **verbs** (7): `RepoEventSubscribe`, `RepoEventDrain`, `RepoEventAwait`, `RepoEventUnsubscribe`, `MailboxNew`, `MailboxSend`, `MailboxDrop`. All errors-tagged `EventError`.
- **helpers** (22, all `raw`): `commit`, `projectCommit`, `headChanged`, `projectHead`, `(<|>)`, `pumpEff`, `drainSubscription`, `withHandler`, `awaitSubscriptionRaw`, `eventIdOf`, `firstMatch`, `nextEvent`, `awaitFirst`, `after`, `projectTick`, `mailbox`, `projectMailbox`, `asyncDone`, `projectAsyncDone`, `mailboxNew`, `mailboxSend`, `mailboxDrop`.
  - Representable (thin one-verb wrapper): `awaitSubscriptionRaw` (Applied/2), `mailboxNew` (Nullary), `mailboxSend` (Applied/3), `mailboxDrop` (Pointfree/1).
  - Not representable (18): relocate to `haskell/lib/Tidepool/Event.hs` as definitions, same lever as Worktree's ten (§11.9).

## Registry 2 — `tidepool-bridge-effects/src/lib.rs`, `Ev*` block

`EvEventId`, `EvSubscriptionId`, `EvWatch`, `EvHeadChangeKind`, `EvHeadChangeReceipt`,
`EvCommitReceipt`, `EvTickReceipt`, `EvRepositoryEvent` (ToCore-only, no FromCore/Eq
— ret-only). All reference `Wt*` Worktree wire types directly (`WtWorktreeId`,
`WtGitOid`, `WtBranchName`) — the cross-effect reference the schema needs a new
`Effect::foreign_types` lookup for (`wire_rust_of` is per-effect-local today).

No separate Rust "domain" type exists for any Event wire type — `SubscriptionRegistry`
(`tidepool-handlers/src/handlers/event.rs`) uses the `Ev*` structs directly as its
working representation. So every Event `TypeDef.domain` is `None`; no adapter module
is generated (`has_adapters` stays false).

## Registry 3 — `haskell/src/Tidepool/Translate.hs`

**Zero references to Event/RepoEvent anywhere.** `intrinsicVerbModules`/`sitedVerbs`
name only the RunLLMTurn/Fork/Finalize family (10 names) — confirmed by grep. Event's
rows never touch `vsMisShapeIsError` (PRD 22 open question 4) — flagged as N/A in the
[READY] note per the spawn context's instruction (7), not silently skipped.

## Registry 4 — `tidepool-harness/src/engine.rs`, `classify_hole`

Already correct — all 7 Event verbs (`RepoEventSubscribe`/`Drain`/`Await`/
`Unsubscribe`/`MailboxNew`/`MailboxSend`/`MailboxDrop`) route to
`HoleRouting::OuterEffect(OuterEffectKind::RepoEvent)` (lines ~535-553). The
comment there already documents the `RepoEventAwait`-class bug and why the three
Mailbox arms were added. This means R4 was already fixed by a prior lane and this
migration's job is to make the SAME classification GENERATED from one
`HandlingClass::OuterDispatch(OuterEffect::RepoEvent)` annotation per verb, so a
future verb without a class fails generation instead of relying on a human
remembering to add a match arm here.

## Schema extensions this lane needs (beyond what Worktree exercised)

1. **Cross-effect named-type references.** `Watch`/`HeadChangeKind`/
   `HeadChangeReceipt`/`CommitReceipt` reference `WorktreeId`/`GitOid`/`BranchName`,
   declared by the WORKTREE effect, not Event's own `type_defs`. `Effect::wire_rust_of`
   and `Effect::validate`'s undeclared-reference check are effect-local only.
   Fix: add `Effect::foreign_types: &'static [(&'static str, &'static str)]` —
   (Haskell name, Rust wire name) pairs for names declared elsewhere, consulted as
   a fallback by both `wire_rust_of` and the validator. `foreign_types: &[]` added
   to Exec/Journal/Worktree's existing literals (no behavior change there).
2. **`HsType::Value` as a wire record field type.** `RepositoryEvent`'s
   `ObservedMessage EventId Int Value` needs `Value -> serde_json::Value` in
   `wire_rs.rs::rust_type` — not wired today (only Text/Int/Bool/List/Maybe/Named).
   One match arm added.
3. **Type-def relocation, not just helper relocation.** `Event a`/`Observed a`/the
   `Functor` instance move to `haskell/lib/Tidepool/Event.hs` as hand-written
   definitions — the Worktree lane only had to relocate HELPERS; this is the first
   lane relocating actual `type_defs` declarations. Moves the Class A golden bytes
   for Event's `type_defs`, documented in the flip commit same as §11.9.
