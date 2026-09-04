# Actor implementation status

This file is the current landed-versus-pending inventory. The active request
and activation contract is specified in
[persistent applications, typed replies, and watches](persistent-applications-replies-and-watches.md).
Older `Complete`/`AgentAction` sections in the adjacent long-form plans are a
superseded design record, not the executable surface.

Backward compatibility is not a constraint for internal actor APIs. Preserve
serialized and externally consumed formats only through an explicit migration
decision.

## Implemented architecture

### Actor substrate

- Ractor is the sole in-process scheduler and mailbox owner.
- Tidepool adds exact incarnation identity, retained terminal observation,
  live Haskell message custody, call ancestry, and subtree shutdown.
- One `ResidentKernelBehavior` serializes installed Haskell program work,
  external-application workbench calls, mailbox handling, and supervision.
- Root and child actors use the same local-actor and resident-machine paths.
- The resident session is the sole owner of Haskell heap roots,
  continuations, declarations, bindings, and machine checkout.

### Persistent applications and root lifecycle

- An interactive application attaches once per actor incarnation and remains
  available independently of the installed program's mailbox standing.
- A model response ends by ordinary response termination. The public Haskell
  surface has no `complete`, `yield`, `park`, `AgentAction`, or `nextTurn`.
- The private permanent root attaches its application and blocks on an
  uninhabited mailbox protocol. Only the supervisor intentionally stops it.
- Request scopes retain `sessionInput`, `sessionReply`, and monomorphic
  `respond` across any number of model responses until terminal settlement.
- An accepted reply closes that request's current workbench activation and
  suppresses its effectful suffix; it does not terminate the agent actor.

### Replies and watches

- `request @Result` returns requester-side `Response Result`; the target gets
  a distinct one-shot `Reply Result`.
- The fixed `Replies` effect owns request identity, private mailbox admission,
  settlement, and repeatable response polling. Admission either transfers the
  private request payload under live-value custody or marks the response
  unavailable; callers do not perform a second cast.
- Rust owns closed request state and authority checks. The Haskell heap owns
  arbitrary live result values and applicative result combiners.
- `attemptReply` resumes only on a typed rejection. Accepted settlement never
  manufactures `Right Void`; `reply`/`respond` use terminal transfer.
- First terminal transition wins, duplicate settlement cannot overwrite a
  value, and exact actor incarnation checks prevent ABA retargeting.
- `Await` has `Functor` and `Applicative` but deliberately no `Monad`.
  `watch` registers known response dependencies atomically and `pollWatch`
  remains repeatable.
- Unwatched response settlement does not wake the model. A terminal watched
  condition publishes one durable typed transition after both response and
  watch polling can observe it.

### Workbench and observability

- Root and request activations use the same fixed authored row:
  `ActorEffects = '[Replies, Watches, Actor, Worktree]`.
- The GHCi-shaped workbench preserves committed prefixes, reports explicit
  `committed`/`rejected`/`replied` dispositions, and keeps effectful rejection
  non-transactional.
- `:status` projects actor incarnation, application/program standing, and
  pending/ready/unavailable response and watch identities from runtime state.
- Durable application inbox rows are typed actor events. Legacy string rows
  remain readable during migration; prose is rendered only at delivery.
- Worktree head/branch reads are genuinely `Member Worktree effs`-polymorphic,
  and effect-level failures use the existing typed worktree error boundary.

## Active verification

- `tidepool-actor` component tests cover exactly-once settlement, fan-in wake,
  ready-before-registration, incarnation fencing, target/requester shutdown,
  and owner authorization.
- The Haskell surface fixture proves persistent agents, `Replies`-only request
  admission, applicative watches, and absence of root `complete`.
- The focused host vertical proves permanent-root attachment, typed request
  presentation, `respond`, terminal transfer, durable watch notification,
  repeatable typed response/watch observation, and `:status` transitions.
- Protocol freshness, Worktree Haskell contracts, strict Clippy, and the
  217-case extractor fixture suite are integration gates.
- The completion-era monolithic host scenario and its escaped fixtures are
  quarantined during this migration. It is replaced by focused semantic
  scenarios; it is not a gate that should be repeatedly rewritten as one huge
  generated transcript.

## Remaining work

### Supervisor policy

- Add typed deadlines and cancellation against the existing request-state
  owner.
- Replace unit-returning `stopAgent` only when the supervisor can truthfully
  distinguish already stopped, cancellation requested, graceful settlement,
  and forced termination.
- Exercise every affected response/watch transition through target and owner
  shutdown races.

### Retention and bounded observability

- Prove reclamation of the live result graph after the final response and
  dependent watch handle disappear, then add only the minimum registry cleanup
  signal the proof requires.
- Extend `:status` with request age and durable inbox watermarks when those
  values have one runtime owner available at the query boundary.
- Correlate request, watch, activation, hosted-tool, and actor-incarnation IDs
  in existing structured tracing rather than creating a second event store.

### Live dogfood

- Run the two-request, one-persistent-worker canary in `shoal-console` using
  two result types and one applicative watch.
- Verify ordinary response termination leaves root and child applications
  attached, request scopes survive a response without settlement, and queued
  requests retain FIFO custody.
- Independently inspect the worker commits and tests; tmux remains diagnostic,
  not authoritative.

## Retirement rule

These plan files retire when their stable contracts have moved into crate
guides, public API documentation, and the glossary. Historical API variants
are deleted rather than maintained as standing architecture.
