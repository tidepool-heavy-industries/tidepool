# Original source authority and lexical resource lifetime

Status: accepted implementation contract. This is required completion work,
not a claim that the receiving APIs or execution controls already pass.

## Decision

Introduce one opaque canonical `RuntimeOriginalSource` at the existing runtime
admission owner. It retains original source identity, issuing epoch, admission
gate and immutable source prerequisites. It does **not** retain a lexical
lease, compiler World, native program, binding root or callback to their owner.

`RuntimeLexicalScopeLease` continues to own actual lexical/native captures and
their strong prerequisite DAG. It also retains its canonical source token.
Source authority and lexical resource custody have different lifetimes and
must be different types.

This distinction fixes two actual ownership problems. Installed binding
provenance currently retains its own scope lease, preventing the last-drop
event that removes those bindings. Retaining only foreign scope leases also
fails: existing transfer APIs permit A-to-B and B-to-A transfers. Weakening all
provenance instead loses A's authority when its last external custody is consumed
into B. Neither workaround is an accepted implementation.

A separate dynamic obligation table on actor generations would also have to
follow generic sessions, detached captures, binding leases, aliases and parcel
imports. The source-token split preserves their existing provenance flow without
duplicating membership and removal accounting.

## Canonical issuance

Extend the existing `RuntimeAdmissionOwner` record with a weak source-token
witness alongside its weak lexical-lease witness. There is no second registry.
Only that owner issues the token; scope IDs, gate equality, compiler products,
source hashes and public constructors cannot recreate it.

The token retains the existing owner, captured epoch, diagnostic origin scope,
actual admission gate and source-token prerequisite DAG. Distinct canonical
scopes may share a gate while retaining distinct identities. Gate identity alone
is not runtime authority.

Source validation checks session liveness, epoch, exact canonical token and
prerequisites under the existing issuance ordering. It does not require the
origin's lexical resources to remain live. Lexical validation separately checks
the actual lease and live target scope. Name these operations distinctly;
remove ambiguous compatibility accessors after migrating their consumers.

New work requires open source admission. Already admitted work retains its
earned claims through END and uses the existing restricted settlement path.
Explicit fences, retirement, stale epochs and uncertain cleanup remain
distinct outcomes.

## Retirement

Use the existing retirement queue with closed event kinds for lexical-resource
release and source-identity release.

| Event | Required behavior |
| --- | --- |
| Last lexical lease drops | Release that scope's binding/native roots and compiler tips through existing cleanup. Do not close a still-retained original source merely because its lexical roots disappeared. |
| Last source token drops | Release that exact source record when no remaining lexical owner needs it; an inherited gate must not close its parent. |
| Explicit `retire_scope`, reload fence or source retirement | Preserve explicit source cutoff semantics. Retained tokens do not authorize new work after cutoff. |
| Session loss or issuer epoch change | Invalidate retained tokens; never resurrect them from compiler evidence. |
| Unconfirmed lower work | Retain earned source claims and actual target/native obligations under the existing uncertainty owner. |

Automatic lexical release must not call the old combined retirement operation
and accidentally revoke foreign retained source authority. Explicit retirement
must not silently become resource-only cleanup.

Queue events identify their actual issuing record. Use existing scope/epoch
uniqueness where sufficient; prove that delayed events cannot remove a newer
record. Do not add another identity allocator without such a need. Drop queues
outside owner locks, as the current lease does.

## Receiving types and data flow

1. `RuntimeSourceCompilerTransaction` owns the original source token. Its actual
   target remains a lexical lease. Claims and unconfirmed END retain authority
   and physical target custody separately.
2. Prepared targets, settled source-bearing output and native source receipts
   carry the token issued by that transaction. Compiler source selections remain
   independent immutable evidence; they cannot issue runtime source authority.
3. `ProgramProvenance` retains tokens in its existing per-site
   Known/Ambiguous/Unknown variants. Unknown remains sticky. Equal compiler bytes
   do not merge distinct issuing origins.
4. Root custody, parcels, parked work, imported bindings and native source
   instances preserve their existing provenance flow. Stored metadata may own
   source tokens because those tokens cannot retain the metadata's lexical or
   native roots.
5. Activation and renderer input retain original A's token and actual B's
   lexical lease. Bind/spend validates both under the existing owner/gate order.
   Original compiler contexts are already retained by the compiled provenance;
   they do not require A's mutable lexical namespace.
6. Installed World associations observe a weak original-source token and weak
   target lexical lease. Actual admitted/completed operations retain the strong
   token/target pair. Generations and executions keep their real lexical custody.
7. Lexical capture constructors take their actual lexical parent explicitly.
   Delete the fallback that treats `transaction.source` as a lexical parent.
   A source-token-only operation cannot manufacture a capture of dead A.
8. Native installation reports the source contributions it actually resolved.
   Reused code retains its original token; incoming current source cannot
   restamp it. Package imports that resolve to an existing code export are not
   automatically source-neutral.

Do not place whole `CompiledTurn`, renderer owners or lexical leases inside the
new token or installed native source metadata. Do not rerun native resolution to
reconstruct contributions after installation.

## Owners and sequence

| Owner | Work |
| --- | --- |
| Runtime-prefix leaf | Canonical token, validators, source claims, typed retirement queue and explicit/automatic retirement split; publish the receiving signatures first |
| Lifecycle/site-source leaves | Per-site provenance, parked/activation/renderer consumers and actor original-source bindings |
| Native-selection leaf | Actual native contribution receipts, historical/current/package-alias source attribution |
| Engine integrator | Join the token core with private input and World custody changes; compile actual consumers incrementally |
| Independent lifetime reviewer | Check token ownership graph, retirement semantics and the real transfer controls |

Keep shared `admission.rs` edits coordinated with the engine's operation-owned
private operand. A small issuer scaffold may land before the consumer migration,
but it is not complete until old original-source lease routes are deleted.

## Decisive acceptance

- Compile source-bearing code in A through the real producer, retain its installed
  provenance, then release external lexical custody. The normal reaper releases
  A's lexical roots without explicit retirement or a reference-count oracle.
- Transfer source-bearing closures A-to-B and B-to-A through real custody
  consumers. Invocation uses each original source token. Removing the receiving
  roots releases both lexical/native resources and remaining source records.
- Retain/import A's closure after A's lexical cleanup, reuse an actual installed
  native instance, park it and capture an activation input under B. Explicit A
  fencing refuses fresh source work even though B remains open. Cancellation,
  lost successful delivery and unconfirmed END retain or release exactly their
  earned obligations.
- Preserve canonical identity across equal compiler facts, stale epochs, shared
  gates and transitive prerequisites. Include a delayed retirement-event control.

The source review and exact caller map are retained in
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/completion/source-provenance-implementation-design.md`.
Execution evidence must identify the integrated revision separately.
