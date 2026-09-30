# Engine and embedded harness completion wave

Approved for implementation after fresh-context review, 2026-09-30 UTC.
This refines `embedded-engine-completion.md` and supersedes conflicting rollout
or authority proposals. Sequential M1 ships first; full compiler/native,
publication/recovery, concurrent M2 and independent captures remain required.
Pushes, live trials and backend-default changes retain their separate gates.
Codex is never built with Buck. Native goal tools remain disabled.

## Shared baseline and ownership

Main `8387ca8f7` joins product `76ce77921a` into main `fc987d517` without losing
newer Buck/package owners. Constructor candidate `458fecd7a5` is joined as
`1bb33cfe3f`, not yet tested. Cargo/Nix library/assets match harness `abfbf3b3`.
The harness native branch `44b48fde` includes that library plus Buck inputs.
Product diagnostics and actor scaffold remain in their original worktrees and
the retained update-handoff snapshot. Never overwrite, stash or reset them.

Root owns joins and production host/browser integration. Sol 6.1 owns compiler,
runtime publication/recovery and actor state transitions. Luna owns bounded
fixtures, browser tooling, Buck generation/coverage and cleanup. Rotate fresh
Sol reviews of exact candidates. Use isolated worktrees and exclusive files;
no headcount-driven delegation. Follow root/nested contributor rules.

## Sequential browser contract

The selected operator path is direct Tailscale HTTP, no SSH forwarding/proxy.
The server presently needs Tailscale login, which belongs to the operator.
Facade accepts loopback or an explicit address verified on tailscale0, rejects
wildcard/public bindings, and passes a typed HTTP/HTTPS public scheme. Preserve
old HTTPS config semantics; configure HTTP explicitly for this run. Retain
session authentication, same-origin checks and credential-file ownership.

Host envelopes require a validated UUID ClientOperationId. Host Input carries
exact target/text; Retire exact target; Interrupt exact target and expected
EmbeddedRoundId. Reuse operation_id as queue/ack/receipt command_id. Missing IDs
or rounds refuse; no server-generated replacement identity for Host commands.
Standalone Submit remains separate. IDs bind run, actor incarnation, action,
exact payload and expected round, not the renewable browser cookie. Conflicts
refuse before effects. Round matching happens under the existing active slot;
a late interrupt cannot affect its successor.

Harness Store v6->v7 adds retained embedded command observations in its existing
owner, not a second scheduler/inbox. Phases distinguish Queued, Dispatching,
InputAdmitted, ControlRequested, Refused and Unconfirmed. Submission persists
before acknowledgement; existing host command loop claims once. Unclaimed
queued work must survive the persist/enqueue gap. Input envelope, input record
and command admission reference commit together. Claimed controls with unknown
outcomes never redispatch automatically. Authenticate before lookup/submission;
retained duplicate observations remain readable after retirement or receipt
projection eviction without new admission/wake. GET /api/commands/{operation_id}
reads retained Store state. Migration preserves old history without inventing
old retry/control guarantees. Keep identities/outcomes for the run lifetime.

Browser retains pending IDs/payloads before send, reconciles on reload, and only
explicitly retries the same operation. Reconnect itself never submits work.
Cryptographic UUID generation must work over HTTP without randomUUID-only
secure-context assumptions. Remove demo-only embedded guidance.

Test-only deterministic provider transport reuses production launch composition,
actual matched assets and real resident Haskell. Declare pinned Chromium,
Playwright/Node inputs through Nix/Buck; no downloads during tests or production
scripted-provider option. Distinguish this browser proof from packaged CLI
preflight/compiler/assets checks and separately approved live credentials.
M1 includes input inclusion, raw/typed calls, compaction, late output, pinned
reload, reconnect/retry, interrupt/continue, retirement, host loss and cleanup.

## Checked-cell authority

Diagnose strict source-hidden same-check fold failure with compiler9a42/runtime
214469 retained diagnostics; do not weaken the assertion. Integrate existing
context-aware authored certification from the exact-consumer candidate.

Runtime-issued immutable admission retains exact declarations/bindings/injected
interfaces/native instances/source/tool leases. Compiler same-offer
TPEXACTCHECK1 binds its digest, final checked source, ordered verdicts/binders,
expression plans and complete structural post-zonk signature Name inventory.
Existing ModuleCandidateOffer validates before scratch deletion and constructs
opaque ExactCheckedCell/ExactCheckedItem. Editable observations cannot mint
authority. Dedicated checked-item compilation uses TPEXACTSCOPE2 and a typed
purpose; no inspection/general-source/authored-purpose metadata shortcut.

Reserve original declaration identities before checking; compiler proves the
synthetic/local-to-original mapping within the same offer. Never rename already
compiled declarations or infer equality from printed types. Item N accepts only
protected completed links 0..N-1 from its same cell: certified products/exports,
injected interfaces, exact planned symbols and runtime-validated instance shares.
No ambient public tip or arbitrary Val module can extend a prefix.

Canonical versioned compiler recipes derive wrappers from admitted runtime
specifications. Widen the parsed-stage seam to resolve hydrated exact Names and
rewrite only identified generated annotation AST nodes; do not widen RdrEnv or
import hidden originals. Seal body/verdict, generated annotations and expression
lift/display/presentation derivations. Initial proof: hidden nominal bind and
expression, local declaration -> binding -> expression, valid prefix, edited
body/wrapper refusal and unchanged hidden class/family lookup. Preserve current
DataKinds/promoted/effect-signature support. Unproved authority refuses before
effects, never bare pins or generic fallback.

## Compiler/native completion

Finish SCC/SOURCE-boot product reuse, fresh mutable compiler state, dependency
and lazy package/orphan/family provenance, final input revalidation and sealed
interface/product pairs. Unknown compile-time inputs miss. Home-EPS fallback
fails. Check retained family consistency against local/imported equations on
every authoritative compilation, not only joins.

Carry certified products through production resident consumers. Protected
CodeExport provenance must be complete and revalidated on retained resolution.
Demand/compile off checkout; atomically install target plus new reachable groups.
Only exact inherited leases share CAF/native instances; otherwise create fresh
ones. Preserve late-demand sibling ownership, rollback and last-owner retirement.

## Private admission/publication/recovery

Expand certificate-backed private admission to nonempty bases, multi-G suffixes,
retractions, record-parent exports, live dependencies, instance/family-only writes.
Freeze final execution intent with exact binding/declaration winners/retractions,
selected instances, full retained family closure, lexical roots/edges, products
and leases. Merge that fixed delta into latest public; never replay effects.

Preserve latest-public exact dfun/axiom selections; only compiler-certified
additions or explicit exact removals change them. Never infer instance removal
from spelling or absent private rows. Unsupported selection removal refuses.
GHC checks overlap/fundep compatibility; associated-family selection follows
certified owner edges. Retained hidden axioms constrain joins after retraction.

Use existing PublicationDecision and atomic-write owner. Stage -> certify ->
checkout/revalidate -> claim -> manifest rename -> preflighted infallible paired
visibility swap. Stale successes and rejections restage the same intent. Before
rename no publication; after rename durability failure remains Published and
only durability confirmation retries. Burn failed reserved identities.

Recovery v3/public schema persists exact lexical roots/edges and artifacts.
Hydrate via existing ExactDeclarationContext recovery path into fresh compiler;
no source/effects/heap/continuation/token/CAF replay. Preserve high-water and
tombstones; determine final winners before losses. Safe empty v2 migrates;
nonempty v2 without certified lexical authority refuses attachment, preserving
manifest. Keep reviewed safe-v1 migration. Certify surviving v3 projections;
never revive superseded definitions after missing artifacts/live values.

## Actor and capture completion

First wire owned preparation/real awaitWatch under single admission. Prefer
synchronous actor advancement returning typed owned tasks. Cursor/reply/control,
receipts/budgets/private authority/decision/cleanup claims survive every step.
Explicit task-local continuation and timing scope follows spawned successors.
Fence incarnation+execution+step generations and retain unconfirmed cleanup.

Before keyed multiple admission, test ingress for workbench/tools, structured
turns, inspection/reload/status/routes/releases/drains/cancel/shutdown. Structured
turns remain non-reentrant; prepare cannot select a newer public baseline.
Convert Sleep/External/Jev, commands, child/request operations and after-tool
waits in bounded parcels. Preserve nested after-tool state and aggregate posture.

Existing checkpoint/fork owners connect private executions and independent
captures. Prove two children from one unfinished parent, later parent failure,
token revocation preventing new admission while children retain shares, and
last-owner reclamation. Keep ordinary deferred unfold distinct; no second
supervisor/fake process actor.

## Build, acceptance and delivery

Both Buck generators consume one committed embedded-native feature profile:
tidepool/exomonad-agent defaults disabled, no native Codex. Reject forbidden
normal/build/test closure before output replacement. Regenerate without flags.
Migrate native test roots with declared fixtures/worker/GHC and process isolation;
authoritative just recipes stay until executed parity. Standard Haskell component
rules; finer module granularity requires measured import/boot/TH-correct graph.
Separate generation/compile/link/fixtures/browser dependency/test actions.

Root admits builds through tidepool-completion-build.slice (104 GiB max), with
separate Cargo/Cabal outputs for divergent worktrees and bind-mounted buck-out.
No shared daemon restart. Buck --local-only -c remote.enabled=false. Heavy Nix
realizations cores=2/max-jobs=1 under separate daemon cap. No universal compiler
slot. Retain source hashes, commands, executed counts, exit status and logs.

Acceptance: complete M1; protected folded/fresh checked prefixes; cache/boot/
family/package/demand/CAF/lifetime; A parks/B publishes/A resumes preserving B;
two parked plus progressing third/control; both cancel/commit orders; stale
proofs/completions and pre/post-rename faults/restart; independent captures.
Final boundary: all verify constituents, structural corpus/embedded producers,
matched harness/web checks and native package; Codex fallback uses existing path.
Cache gates record warm reuse and controlled Rust/fixture/TS and real locked npm
dependency changes, separating daemon reuse from action-cache hits.

Prepare exact single-root live packet after M1 and recursive Sol/Luna G5 packet
after M2; obtain separate approvals. Retain reviewed work, integrated checks,
trace, interview and resource disposition. Pushes held; verified bundles disclose
unverified canonical remote fetches. Remote/default cutover remain separate.
