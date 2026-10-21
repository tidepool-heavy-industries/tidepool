# N: publish native invocations without inventing checked cells

Primary outcome: publication consumes the compiler progress actually issued by
private execution. Secondary questions: preserve retained native results, keep
cancellation/cleanup atomic, and remove optional-proof success fallbacks.

## Confirmed counterexample

The installed `reload` endpoint returns the expected ReloadUnavailable value,
then publication fails with `private publication lacks a checked cell prefix`.
It was not given cell source and did not compile a cell. The missing prefix is
not a lost compiler receipt. Read `peer-review-private-publication.md` for the
full real-dispatch trace and artifact locators.

Relevant owners at the reviewed base:

- `owned_workbench.rs`: private admission around 1126, cell-only preparation
  around 1149, unconditional successful private publication around 2354.
- `resident_workbench.rs::begin_installed_entry`: retained endpoint execution;
  native result retention and after-tool paths remain real writes.
- `session/paired_publication.rs::freeze_private_execution`: freezes the intent.
- `session/persistent.rs`: prospective promotion, final claim and durable commit.
- `compiler_world.rs::promote_private_bindings`: unconditionally requires the
  checked position; only real CellProgram successors issue that position.

## Chosen design

At private admission, capture the actual borrowed compiler base under its owning
runtime transaction. Carry a private typed progress value alongside execution's
outcome, through freeze to promotion. Distinguish authenticated unchanged
compiler state from an actual completed checked successor, and both from work
that started but never completed. Use the existing issuance/lineage types.

The unchanged branch is issued by the owner that can establish its fences and
absence of compiler additions. It is not `position.is_none()`, a public boolean,
a reload-name exception, an arbitrary Tool variant, or Arc pointer equality.
Preserve newer valid public progress when merging a genuinely unchanged branch.
No synthetic zero-item checked prefix and no broad Tool publication bypass.

Retained handlers, pruned output and after-tool code may produce host values or
other facts. Carry those actual issuers through the existing transaction. If
they change compiler facts, represent their real progress; don't strand the
values by declaring every native invocation unchanged. If you discover another
legitimate progress form, provide its concrete producer/consumer before adding
an enum case. Do not make `Other` or `Unknown` a successful branch.

Keep the final cancellation claim and durable bindings/World commit in the
current publication owner. Preserve committed/refused/recovery-required
distinctions and original semantic replies on publication failure. No effect
replay to recover a lost publication receipt.

## Production-path controls

1. Installed no-cell endpoint after an actual seed: exact semantic result,
   successful settlement, seed still readable, invocation/private resources
   released, and no invented checked-cell request.
2. Existing ReloadUnavailable histories with parked sibling calls and failed
   mutable-source observation: no recapture or binding loss; later work succeeds.
3. Actual checked multi-item cells still publish atomically. Incomplete prefix,
   wrong certificate/base and foreign owner refuse; they cannot take unchanged.
4. Native/after-tool retained host result survives private retirement; a foreign
   carrier cannot publish partial names. Include this rather than only a no-op.
5. Cancellation before claim publishes nothing; cancellation after claim observes
   the existing retained settlement. Publication failure preserves the handler's
   original result/effect evidence without replay.
6. Reopen committed public state and invoke again; unresolved private intent
   stays unresolved/refused. Cold inventory is not live completion authority.

Use the existing production test harness and actual issuers. The component helper
`snapshot_declaration_publication` intentionally passes `world: None`; it cannot
prove World promotion. Don't patch it to manufacture a successful prefix.

Adjacent investigative control: `with_primary_and_lexical` clears position when
changing compiler input. A completed-cell then scaffold/type-extension history
should either carry valid owner evidence or refuse correctly. This is a lead,
not the proven cause of the two installed-tool failures; don't blindly copy the
old position to make it pass.

Compile runtime, toolchain, actor and affected test consumers early. R reviews
the progress constructors, both promotion paths, claim/drop behavior and the
test oracles. Integrate independently of P/W when its own controls pass; full
notebook qualification still uses the joined source.
