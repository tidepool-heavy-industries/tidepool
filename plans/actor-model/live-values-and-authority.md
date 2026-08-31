# Live values and authority

## 1. Core rule

> Values may move; authority must be granted.

Tidepool already supports opaque same-machine values through `ValueHandle`.
The actor layer should use that substrate for mailboxes and replies instead of
lowering every message through JSON. It must separately authenticate
operations on authority-bearing values.

These concerns are deliberately independent:

- **mobility** asks whether a live value can be rooted and delivered;
- **authority** asks whether the actor currently evaluating may perform an
  operation represented by that value.

Trying to label every arbitrary Haskell closure as globally “fork-safe” or
“not fork-safe” conflates them and would require fragile recursive taint
analysis. Most immutable Haskell values are safe to share. The capability
leaves they eventually invoke can check their caller.

## 2. Value classes

| Class | Examples | Mobility | Restart |
|---|---|---|---|
| ordinary Haskell value | sums, products, maps, pure closures | Same machine by live root | No, unless explicitly encoded |
| actor program value | `ActorProgram actorEffs api exit`, `ActorDefinition startup api exit`, opaque `ActorSpec startup api exit`, function-bearing record | Same machine; promotion captures an exact deployment image | No initially |
| opaque runtime value | `AgentRef api exit`, worktree handle, command runner | Copyable where its registry permits; use is caller-checked | Recover only through its owner |
| durable value | JSON stored through get/put | Anywhere the backend exposes it | Yes |
| external wire value | provider or MCP payload | Encoded at the boundary | According to that protocol |

Serialization is not the actor communication model. It is a boundary adapter.

## 3. Execution principals

Before Rust enters Haskell on behalf of an actor, it installs an execution
principal containing at least the actor identity and incarnation. Every host
operation that consumes a capability checks that principal.

The incarnation prevents an old handle from gaining access to a replacement
actor that reused a durable logical identity. The exact representation should
reuse the repository's monotonic identifier issuer and stale-writer fencing
patterns rather than invent string identifiers.

The principal is ambient runtime context, not an ordinary Haskell value. A
closure captures its lexical values but not the authority of the actor that
created it.

This yields a crisp distinction:

```haskell
receivedFunction input
-- Runs locally under the current actor's principal.

call originalOwner request
-- Routes a request; the owner handles it under the owner's principal.
```

If callers need the creator to exercise its authority, they call the creator.
Passing its function is intentionally insufficient.

## 4. Capability registry

Authority-bearing Haskell values contain unforgeable identifiers into a
Rust-owned capability registry. Their constructors are abstract behind the
owning module's public operations. Registry entries record:

- owning actor and incarnation;
- allowed callers or delegated grants;
- resource lifetime and revocation state;
- behavior when the owner forks;
- cleanup owned by Rust;
- optional failure provenance.

Actor interpreters use the same principal and grant records. The Haskell row
does not serve as a capability token, and Rust carries no reflected copy of
it. V0 fresh spawn inherits the owner's composition-owned interpreter policy;
a later explicit child-policy selector must be justified by a concrete
capability and must reuse this registry rather than create a runtime-profile
registry.

The common authorization behavior should be registered callers rather than a
blanket prohibition on copying the surrounding value. A failed operation
produces structured failure state such as unauthorized caller, revoked
capability, or resource unavailable after fork.

Protect nominal identities, not spellings. An actor may shadow the text
`Capability`; it still cannot construct the abstract
`Tidepool.Kernel.Capability`.

Not every familiar Haskell verb needs an explicit `Capability` argument. A
verb may consult a principal-scoped grant in the same registry. In both forms,
authorization belongs to the current caller rather than the closure's creator.

## 5. Fork policy

Each capability class registers one fork policy with the Rust interpreter:

| Policy | Meaning | Example |
|---|---|---|
| `OwnerOnly` | Child can share the enclosing value but is not an authorized caller | Parent-only integration authority |
| `ShareWithChild` | Child becomes an authorized caller of the same resource | Read-only repository view |
| `RebindForChild` | Runtime maps the copied handle to a child-specific resource | Isolated worktree endpoint |
| `InvalidAfterFork` | The reference is actor-linear and child use is rejected | Continuation or reply-obligation reference |

Fork policy is defined by the capability owner, not selected by arbitrary
model-authored Haskell. Haskell may request explicit delegation when policy
allows it.

No first implementation should attempt to inspect every object reachable from
a closure and reject the closure because one leaf might be restricted. The
operation on that leaf remains guarded.

### Control continuation versus continuation references

Fork clones the actor's control continuation. It does not grant children the
runtime-issued references reachable from that continuation. Pending-call
handles, joins, reply obligations, and parked-request references remain valid
in the parent and become invalid for child principals.

Rust enforces this in the actor interpreter. It does not remove bindings,
rewrite closures, or alter Haskell types. A child receives a Developer message
describing invalid references after the shared provider prefix; actual use
fails with `InvalidAfterFork`. As with every unsatisfiable effect, that abandons
a disposable workbench fragment or terminates an installed actor-program
continuation.

`call` remains single-result; fork never changes reply cardinality implicitly.

## 6. Launch grants

Fresh spawn needs one explicit way to authorize a child for a particular
resource. The runtime must not recursively inspect the startup value for
capability leaves, and static interpreter policy cannot name per-instance
resources such as one worktree.

The initial membrane is an opaque launch-grant recipe attached immutably to an
actor specification:

```haskell
data LaunchGrant

withLaunchGrant
  :: LaunchGrant
  -> ActorSpec startup api exit
  -> ActorSpec startup api exit
```

Capability-owning modules create these recipes through their own small
vocabulary—for example, a worktree module may expose a function that requests
its registered child policy for one handle. The model does not choose an
arbitrary `Share`/`Rebind` enum, forge a registry identifier, or edit a generic
grant record.

At `startActor`, Rust checks the current principal and the resource's
registered policy under the child interpreter. It then redeems every
recipe atomically for the newly allocated child before startup. Any refusal
rolls back the unpublished child and all grants already derived for it.
Copying a recipe or decorated specification transfers no authority: every
redemption is checked again, and the resource owner decides whether a recipe
is reusable, rebinds a fresh resource, or has expired.

This is launch metadata, not part of the program image. General post-start
`delegate`/`revoke` operations are deferred until a real protocol needs them.
Actor retirement still revokes grants it owns, and revocation is always
checked at use by already-copied handles and closures.

## 7. Mailbox root ownership

A live value sent through a mailbox needs an ownership protocol above raw
`ValueHandle`:

1. The sender evaluates enough to produce the request value and reply
   obligation required by the protocol.
2. Rust roots the value in a message envelope owned by the destination
   mailbox before acknowledging the send.
3. Dequeue transfers root ownership from the mailbox to the target actor's
   runtime resource scope or active call.
4. A reply is rooted before the callee's request scope can retire.
5. Cancellation, actor death, refused delivery, and dropped replies release
   their roots exactly once.
6. Delivery never depends on bridging a closure into Rust's tolerant `Value`
   representation.

This should extend the existing handle/root ledger rather than create a
parallel GC-root registry. Root counts and ownership transitions must remain
observable in tests.

`cast` has no reply root. `call` has a reply obligation that is runtime-owned
and linear even if the request value is freely shareable. Rust retains delivery
refusal and settlement failure as structured lifecycle state. The ordinary
Haskell operation neither fabricates a domain value nor exposes that machinery
as part of the actor protocol.

### Successful exit values

Successful actor exits do not use the mailbox root ledger. The opaque
`AgentRef api exit` carries a shared, managed Haskell single-assignment cell.
The actor entry wrapper fills it before reporting completion to Rust, so every
copy of the reference reaches the same typed value through the ordinary
Haskell heap. Rust stores the terminal lifecycle fact but never a
`RootedValueRef` for the successful payload. This keeps `RootCustody` as a
temporary transport token and makes exit lifetime exactly ordinary value
reachability.

## 8. Effectful function portability

- Pure closures move freely.
- Row-polymorphic functions with `Member` constraints move and instantiate
  against the receiving actor's stack.
- A function specialized to a concrete stack may move as a value, but GHC only
  permits applying it where that concrete row unifies.
- A forked continuation runs through the child's new interpreter instance
  under the child's principal and grants.

Rust never interprets union tags as handler positions and does not maintain a
second effect-row ABI. The actor interpreter authorizes nominal request
constructors at use time; Haskell row compatibility remains Haskell's job.

## 9. Same-machine boundary

Arbitrary live values only move between actors attached to the same machine
session. Crossing a machine or process boundary requires an explicit codec;
JSON is the initial general codec.

The actor API must expose this failure honestly. It must not silently render a
closure, replace it with a sentinel, or pretend a cross-machine call has the
same contract as an in-machine call. A later typed codec registry can widen the
boundary without changing local semantics.

## 10. Contract tests

The authority layer is not complete until tests demonstrate all of these:

1. A pure closure crosses actors, survives producer retirement, and runs.
2. A closure containing an owner-only capability crosses but fails with the
   receiver named as the unauthorized caller.
3. A registered launch grant makes the same invocation succeed for the child,
   while owner retirement makes an already-copied handle fail again.
4. A fork inherits only capabilities whose class registers or rebinds it.
5. A local closure call uses caller authority; an actor `call` uses callee
   authority.
6. A cancelled call, dead actor, and abandoned mailbox release message and
   reply roots exactly once.
7. A live-value call across a machine boundary is rejected before execution.
8. An old incarnation cannot use a grant issued to its predecessor.
9. A transferred snapshot remains valid after its source actor retires.
10. A forked continuation dispatches nominal requests through the child's
    interpreter and is subject to the child's principal and grants.
11. Pre-fork continuation references remain valid in the parent and fail in
    children without any Haskell binding rewrite.
