# Workbench curation review

The prepared package gives Sol owners substantive engineering work and reusable
GHCi operations. A requested Astra engagement can edit and check the next package
from its own checkout. Runtime ownership, native TUI execution and frozen swarm
selection retain their existing owners.

Implementation: `92a47f26` and `60ce8a1866be3d6b91bef22cc6c7afa6dc92f5f5`.
Installed application package: `04122e7497ccca4e7d4c597c10ae90cdc1d75c6e` in
`/home/inanna/dev/shoal-repl`. All seven curation steps are complete.

## Responsibility review

These are authored responsibilities over ordinary workers, not Rust workflow roles
or a required procession of actors. The common prompt teaches local engineering,
fluent Haskell, evidence, ownership and compact grammatical communication. The
shared API guide supplies mechanics once; changing task data follows that prefix.

| Seat | Available packet and next useful action | Continuation that matters |
|---|---|---|
| Application owner | Plan tree, project language, exact app baseline; component/componentLead and independent result/question watches | Incorporate the checked contract before parallel consumers; return decisions through the existing response; accept partial deliveries independently |
| Sol component lead | Task with source, rationale, scope, acceptance and checked decisions; implements directly and calls reviewCandidate | Repair locally and reuse reviewAgain; delegate only meaningful independent work; retain its delivery while review or expert work waits |
| Independent reviewer | ReviewTask with one exact candidate, contract, gates and explicit repair owner | Return findings to an implementing lead, or request a separate retained implementer's repair directly; preserve pending review during questions |
| Implementation/repair recipient | Selected Task, then a concrete RepairTask or IncorporationTask with complete request guidance | Check the new source and return exact evidence; earlier replies remain history; never queue a question behind the requester waiting for this reply |
| Tagged Astra | Declared design plan, exact source, uncertainty, evidence, alternatives and consumers | Return supported semantics, a proposed amendment or specific missing evidence; checked incorporation precedes downstream task propagation |
| Requested RSI Astra | Selected outcomes, usage interval with coverage, source/definition identities and concrete friction | Edit normal .shoal source, compile it and run its portable recipes; return a checked candidate for explicit next-swarm adoption |

Fresh consumers receive the exact answered Question, accepted rationale and checked
source. A stale answer cannot clear a newer finding. The authoritative candidate
revision occurs once; reviewed and integrated revisions remain separate facts.

Removed the automatic reviewFrom wrapper: handles created inside a callback do
not become new GHCi bindings, making subsequent owning decisions awkward. The
package retains directly inspectable review handles and demonstrates generic
routes where the caller already owns the destination and continuation. Ordinary
unfold/request/watch/route composition remains available.

## Executable evidence

The current focused integration batch passed **14 tests** through `just test-lib
tidepool`, selecting the complete candidate recipes, candidate-only defect/repair,
shared guide, selected launch preview, independent worker lifetime, owned reply
routing/cancellation, workspace freeze/validation and prompt catalog boundaries.
The corrected routing-only recipe also passed independently. Protocol generation
and generated Rust formatting guards passed **15 tests**. Rust formatting and
`git diff --check` passed. No extractor translation or serialization changed in
this curation slice.

The package owns three ordinary Haskell check entries:

- `Project.Checks.workbench`: direct implementation, failed review, local repair,
  reuse of the same reviewer, partial delivery, actual Git incorporation and RSI
  prompt editing through an explicit new selection.
- `Project.CollaborationChecks.collaboration`: delegated implementation, direct
  retained repair, tagged expert amendment and checked incorporation, cumulative
  questions, failed/supported/unconfirmed steering, unrelated progress, a fresh
  consumer carrying the decision, acceptance of the latest combined source, and
  an RSI edit from that same completed work.
- `Project.RoutingChecks.routing`: exact-candidate forwarding without a model
  relay, cancellation and lost execution, duplicate attention suppression,
  cumulative outstanding questions and terminal subscription cleanup.

The negative check first runs a good candidate, removes its actual architectural
rationale, proves compilation still succeeds and behavioral checking rejects the
candidate, then restores the source and proves the same check succeeds. This
checks the candidate's current package rather than a compiled-in shipped copy.

The command `shoal check --workspace PATH --recipes` uses the extracted existing
resident driver. Haskell chooses the workflow. Temporary repositories contain
copied authored .shoal files and check-created source fixtures; these recipes do
not copy the application's source or run application acceptance tests. Ordinary
actors do not acquire the check-only effect or launch native providers.

## Fixed build and acceptance boundary

`nix build .#shoal --out-link target/shoal-curation-candidate` succeeded for the
implementation above. Executable:
`/nix/store/53g4rdbi08b87v9149p2yn9pbp399720-shoal/bin/shoal`.
The wrapper pins Codex `0.0.0-dev+4372d1a`; its version, hosted-tool, destination
fork, queue and archive CLI probes passed. The stable build link
`target/shoal-standalone-ready` now selects this build.

The installed executable passed **43 assertions** (10 workbench, 20 collaboration,
13 routing) in **199.3 seconds**, using its packaged compiler daemon with all
inherited TIDEPOOL overrides removed and only that daemon's socket selected via
`TIDEPOOL_EXTRACT_DAEMON_SOCKET`. No development compiler or library supplied the
check. Definition identity:
`c9679d58d7f0e0c76999da1e8c1cd4e097bd765ba80c27c9d5df68d69ca47060`.

A cold standalone invocation reached five workbench assertions in roughly eight
minutes before it was deliberately stopped; that invocation is partial evidence.
Repeated compiler startup makes the full cold recipe suite impractical for tight
iteration. A compatible existing compiler daemon makes the command practical;
automatic scoped compiler reuse for standalone checking is a concrete performance
follow-up. Normal Shoal initialization already owns a resident compiler. No new
compiler launcher or cache was introduced in this curation pass.

The application-only readiness record owns the final source/build/definition
identities and launch command. No paid actors were launched. Deterministic
coordination and this responsibility review establish preparation quality;
fresh-model usability, useful application delivery and measured spending remain
evidence for the subsequent application wave.
