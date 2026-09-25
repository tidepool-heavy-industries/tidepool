# Request scoped typed tools

## Decision to check

Add one generic way for an installed Haskell tool handler to read the *current*
typed request. Use it to add an acceptance submission tool for bounded Luna
reviewers. Keep the Haskell notebook and the existing `Replies` settlement
path. Do not add a review protocol or a request registry.

This is a design check, not an implemented contract. The first implementation
must prove the structural type comparison and scoped binding borrow below
before publishing a new effect constructor.

## Current boundary

`exomonad/examples/workspace/.exomonad/AgentSpec.hs` installs
`Project.Tools.WorkspaceTools` (`shell`, `inspection`).
`Tidepool.Agent.Contract.installSpec` compiles one declared surface and a
retained dispatcher. `exomonad/actor/src/resident_actor.rs` calls
`application_workbench().prepare_tools` at policy installation; later
`reviewAgain` presents a new typed request without reinstalling the policy.
The tool closure therefore cannot capture `sessionInput` or `sessionReply`.
Those bindings are mounted by `ResidentActorWorkbench::mount_activation_input`
for the active request, and `ResponseExpectation::request_preamble` gives
notebook cells their typed `respond` function. Passing a copied
`ReviewRequest` through tool JSON would let a stale or invented basis become
the accepted basis.

The active request is already tracked by `outstanding_interactive` and selected
by `active_workbench`. `Replies::AttemptReplyWith` owns settlement, including
`ReplyStale`, `ReplyAlreadySettled`, and cancellation outcomes. The actor's
`WorkbenchExecutionId` journal already fences retries of one hosted tool call.
Neither needs a second owner.

## Generic request access

Desired authored API (names provisional):

```haskell
data RequestScope input result
  = RequestUnavailable RequestScopeError
  | RequestActive RequestId input
requestReplyOf :: RequestScope input result -> Maybe (Reply result)

currentRequest
  :: forall input result effects.
     Member Replies effects
  => Eff effects (RequestScope input result)
```

Add the primitive to the existing `Replies` effect in
`Tidepool.Agent.Reply.Internal`, which already owns typed requests and
settlement and is in `CodingEffects`. Its internal suspension must be site
aware. Its site evidence must contain
three closed types: the complete answer (`RequestScope input result`),
`input`, and `result`. `RequestActive` places the live input in its final
field, which is the shape the existing framed custody resume can borrow.
The existing sited verb
lowering in `Tidepool.EffectSchema`, `Tidepool.SiteClassifier`, and
`Tidepool.PreparedSites` can reject unresolved type variables. A raw
polymorphic `CurrentRequestWith` constructor without site evidence would have no such
proof and must not borrow a live input.

At activation, retain the original request's typed site id. The prepared
machine's canonical site index resolves it to the installed program and site
row whose answer is its reply type and whose first input is its assignment
type. `decode_typed_request_site` currently
retains rendered GHC names for presentation; they cannot authorize a borrow.
At the tool suspension, compare the new site's closed `input` and `result`
nodes with the original request site's input and answer nodes, respectively.
`tidepool/runtime/src/session/prepared.rs` already compares type graphs across
programs in `type_nodes_equivalent`: nominal family and constructor identities,
ordered arguments, and recursive fields, rather than local node numbers or
rendered strings. Expose the minimum comparison operation from that owner.
Mismatches and absent/settled requests return a typed refusal before any
handle is borrowed.

The mounted input is owned by the persistent binding store. Record its exact
`SessionVarId` and lexical scope when `mount_activation_input` succeeds, and
borrow that binding only if the active request still owns that id in that
scope. `ResidentSession::prepared_binding_handle(name)` is currently
unscoped, so it is unsuitable: a later `sessionInput` can shadow an older one.
Add a scoped/id checked borrow next to `current_binding_in` in
`tidepool/runtime/src/session/resident.rs`. The borrowed handle can then feed
the existing framed custody resume path. That path checks only runtime
representation for the borrowed field; the structural comparison above is
its necessary type guard. Keep request id and reply handle runtime issued;
the Haskell helper constructs `Reply result` only from the matched scope.

## Review tool after the primitive

Add `submit_review :: Call ReviewSubmitInput ReviewSubmitRefusal` in a new
`Project.ReviewTools` handler nested under `Project.Tools.WorkspaceTools`, and
include `Replies` in `AgentSpec.agentSpec`'s effect constraints. Its first
version submits **acceptance only**. `ReviewSubmitInput` carries the expected
request id, expected candidate OID, review checks, and rationale. It does not
carry a `ReviewBasis`, `Candidate`, or arbitrary Haskell source. The handler:

1. Gets `RequestScope ReviewRequest (Outcome ReviewDecision)` at invocation,
   and proceeds only from `RequestActive`.
2. Compares the expected request id and OID with that scope and
   `candidateCommit (reviewInput current)`. Check the bound review checkout's
   current HEAD against that OID before claiming source evidence.
3. Builds `ReviewedCandidate (reviewBasis current) (reviewInput current)
   checks rationale` and calls `attemptReply` through the scope's typed reply with
   `Produced (Accepted reviewed)`.
4. Returns a typed refusal for absent scope, mismatch, or a `ReplyError`.
   Successful settlement is the existing terminal reply transfer, not a
   second tool result claiming acceptance.

Each `reviewAgain` request supplies a fresh scope; no initial `sessionInput`
is retained by the tool. The candidate's claimed checks and remaining gates
remain the fields of the exact request's `reviewInput`. Newly performed
review checks belong in `reviewChecks`. Repairs, blocked outcomes, progress,
and arbitrary Haskell remain available through the notebook for this first
slice. The tool's declared schema is added for new actor incarnations;
`reload_agent_spec` correctly refuses a surface change in a running one.

## Alternatives and implementation order

Recompiling a request scoped AgentSpec on every activation could capture
`sessionInput` and `sessionReply`, but it changes policy installation and
requires a request specific spec entry for differently typed assignments.
It also must preserve one registered tool surface across `reviewAgain`. That
is more machinery than a generic typed scope access at tool invocation.

A workspace-only helper can make the Haskell expression shorter today, but
an installed handler cannot read the authoritative current request. A native
`submit_review` that generates Haskell source or decodes `Project.Types` in
Rust would duplicate the authored review contract. Neither is the selected
tool design.

Implement in two reviewable changes: first the generic primitive and tests;
then the workspace tool and its prompt/skill examples. Expected owners are
`bridge/haskell/lib/Tidepool/Agent/Reply/Internal.hs` and the public
`Tidepool.Agent.Reply` facade, the manual `RepliesReq` decoder in
`exomonad/actor/src/request_effect.rs`, the extractor's sited verb table,
`tidepool/runtime/src/session/{prepared,resident}.rs`, and
`exomonad/actor/src/{typed_request,resident_workbench,resident_actor}.rs`.
The authored tool touches `exomonad/examples/workspace/.exomonad/Project/`
and `AgentSpec.hs`, plus the mirrored development workspace and relevant
prompts. This spans protocol, extractor, runtime, actor, and workspace builds;
each owner needs a focused compiled target. Do not start with a broad battery.

## Focused acceptance cases

- A tool call with matching closed input/reply types reads the current request
  and settles the exact assigned or exact scope candidate. Preserve the full
  `Task`, including accepted decisions, on assigned reviews.
- A subsequent `reviewAgain` supplies the revised request and candidate;
  the earlier request id or OID is refused without settling the new request.
- Wrong generic input type, wrong generic reply type, no active request, and
  a later binding shadowing `sessionInput` all fail before borrowing a value.
  A failed comparison leaves the active request answerable from Haskell.
- A checkout at another HEAD is refused; review checks cannot be attached to
  an unincorporated revision. A dirty checkout must not be reported as proof
  of a clean candidate.
- Retrying the same hosted call returns the existing execution receipt without
  replay. A new duplicate invocation reaches `ReplyAlreadySettled` or an
  explicit no-active-request refusal, and cannot replace the first verdict.
- Existing Haskell `respond`, repair, blocked result, and non-review tools
  still work. Tool surface reload semantics remain unchanged.

The remaining consequential question is whether a closed sited call can carry
both type arguments and a constructible `RequestScope` answer through the
current extractor and framed resume. Prove that with a focused fixture before
freezing the helper's signature or adding the protocol constructor.
