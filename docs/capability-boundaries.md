# Rust capability boundaries

Use capability contracts to make a consumer understandable from its signature
and the contracts it names. The application wires services; consumers depend
on behavior. A service implementation can compose other services without making
every caller know its implementation's dependencies.

This is the accepted direction for incremental Rust migrations. The compiler
admission pilot is the first production slice, not evidence that all runtime
services already follow this pattern. Haskell effect rows and runtime resource
grants keep their existing meanings.

## Four responsibilities

| Responsibility | Representation | Owning boundary |
|---|---|---|
| What an operation promises | Behavior trait and domain input/output types | Subsystem implementing the operation |
| Which behavior a consumer can use | Named `HasX` trait with a bounded associated service | Same subsystem |
| Which implementation and shared state to use | Concrete service context with private fields and explicit wiring | Application or subsystem composition root |
| Which particular resource or transition is authorized | Issued handle, lease, permit or ticket | Resource/lifecycle owner |

A context can contain many services while a function requires only
`C: HasX + HasY`. Through generic `C`, only the capabilities in its bounds are
available; this does not restrict globals, other arguments or captured closures.
Avoid concrete-bundle accessors or `Deref` implementations that recover the whole
application and defeat that boundary. Pure local helpers need no capability trait.

Service availability does not establish current lifecycle admission, source
compatibility, provider health, completed cleanup or successful persistence.
Those facts remain explicit operation inputs and results, enforced by their
owners. Existing `ActorCapabilities` describes actor effect availability; it is
not the concrete service context described here.

## Services that compose services

There are two ordinary Rust forms. Choose according to who owns the dependencies.
Both let the outer consumer name only the behavior it needs.

### Dependencies supplied for each operation

When a service implementation should work with the caller's selected services,
index its behavior by that context. The following is a design template using
the compiler domain's input/output names; it is not the current pilot's API:

```rust
trait CompilerAdmission<C: ?Sized> {
    fn admit_work(
        &self,
        context: &C,
        owner: &CompilerCloseOwner,
    ) -> Result<CompilerWorkTicket, ResidentActorWorkbenchError>;
}

trait HasCompilerAdmission {
    type Compiler: CompilerAdmission<Self> + ?Sized;
    fn compiler_admission(&self) -> &Self::Compiler;
}

fn admit<C: HasCompilerAdmission + ?Sized>(
    context: &C,
    owner: &CompilerCloseOwner,
) -> Result<CompilerWorkTicket, ResidentActorWorkbenchError> {
    context.compiler_admission().admit_work(context, owner)
}
```

An implementation may be declared as
`impl<C: HasX + HasY + ?Sized> CompilerAdmission<C> for ResidentCompiler`.
Its body can use X and Y. The outer `admit` function still needs only
`HasCompilerAdmission`: the associated type promises that the selected service
implements the behavior for this context. Wiring a context that lacks X or Y
fails to compile. A different compiler implementation may have different private
dependencies without changing the consumer.

This extends recursively. X may itself be implemented using Z. Keep implementation
requirements on the implementation, not on the public `HasCompilerAdmission`
supertraits. A method-level `where C: HasX + HasY` on the public operation would
instead force callers to supply those bounds; that is a different contract.

The service is an ordinary object: it may contain state, an actor client, or no
fields. Passing the context avoids self-referential application structs and
lets one implementation work with multiple explicitly wired service subsets.
Do not create recursive trait proofs such as A requiring B requiring A. Model
actual runtime feedback through the existing actor/client boundary.

### Dependencies captured by the service

An independently owned service can hold a context of its own:
`ResidentCompiler<D> { dependencies: D }`, with
`impl<D: HasX + HasY> CompilerAdmission for ResidentCompiler<D>`.
Its public behavior need not accept a context argument. The accessor is then:

```rust
trait HasCompilerAdmission {
    type Compiler: CompilerAdmission + ?Sized;
    fn compiler_admission(&self) -> &Self::Compiler;
}
```

Use this form for an existing client whose state and dependencies already have
one owner. It is also the simpler form for the current admission pilot, whose
service delegates to an explicitly supplied compiler owner and needs no peer
service. Do not invent dependencies to demonstrate composition. Introduce the
context parameter when a real implementation needs operation-selected services.

These forms express different dependency lifetimes. Do not maintain parallel
entry points for the same operation merely to demonstrate both forms.

## Owned contexts and async work

Prefer a cheaply cloneable context holding `Arc`-backed services and existing
cloneable actor clients. Keep fields private and implement access traits explicitly.
Getters normally return references; spawned tasks own their context or required
service handles. This avoids propagating context lifetime parameters through
the application.

Clone at the task ownership boundary, then call services inside the `async move`
body. A borrowed service can live across an await because the task owns its
context. An async behavior used across threads must promise `Future: Send` in
its behavior signature, with the necessary service/context `Sync` bounds explicit;
an owned cloneable context alone does not prove that its operation future is Send.
Require `'static` only at the scheduling boundary that needs it. Do not require
every capability, including synchronous local ones, to have every scheduling bound.

An existing actor client can be stored directly; a zero-sized implementation
does not need another allocation. `Arc` supplies shared ownership, not a new
lock or a requirement to put a mutex around each service. Mutable state remains
with its existing actor or resource owner.

Cloning the context must not duplicate exclusive transition rights. Move affine
tickets and publication permits separately into the operation. Expose observer
and controller handles separately when observing must not imply cancellation.
Preserve their real drop behavior: dropping a waiter, requesting interruption,
observing completion and confirming cleanup are distinct events.

A full context clone can retain unrelated services longer than intended. Use a
small owned context projection where a task's lifetime must be independent of
the application. Do not manufacture a new projection type for every function.
Avoid strong-reference cycles between an application and services that retain it;
borrowed getters and explicit dependency contexts remove the need for that shape.

## Contracts that support local reasoning

A behavior trait should document enough for a caller to use it without reading
the implementation:

- Which identity and owner scope its inputs and returned handles refer to.
- What happens before success is returned, including publication and durability.
- Whether the operation blocks, starts work or only observes existing work.
- Cancellation, retry and drop behavior, including independently continuing work.
- Failure distinctions, uncertainty, retained evidence and remaining obligations.

Put guarantees at the owner that can enforce them. A getter is usually cheap and
infallible; operations perform dynamic admission. An optional service or fallible
resolution should be explicit in its contract rather than hidden global lookup.
Do not default a missing dependency into existence. Constructors at composition
roots choose implementations; consumers receive their context.

Keep behavioral and access traits near the domain, with the smallest useful
visibility. Public extensibility is a separate decision from internal modularity.
Externally implementing a trait must not provide constructors for owner-issued
proofs or bypass admission. Returned handles keep their private invariants.

## Shared machinery

The initial shared mechanism is the bounded associated-type access pattern and
explicit wiring. It needs no registry, resolver, runtime reflection or dependency
injection container. A single implementation is sufficient to justify a meaningful
behavior contract.

After a second production capability demonstrates repeated declaration/wiring
work, consider a small declarative macro for named accessors and explicit field
wiring. It must preserve ordinary Rust diagnostics, support context-indexed
behavior where needed, and generate no policy, `Default`, authorization or hidden
clones. Domain behavior, errors, constructors and lifecycle laws remain authored
by their owners. Inspect the expanded result before accepting the macro.

A generic `Has<Key>` with an unconstrained associated service does not imply that
the service implements a particular behavior. A named adapter can add that proof,
but also adds a layer. Introduce a generic key system only for a real generic
operation over keys, rather than to recreate the named contract indirectly.

Prefer explicit impls for concrete context types. Blanket derivation from unrelated
capabilities can overlap when a context has both. Avoid blanket `Deref` forwarding
that reserves implementations future context types need. Existing Frunk effect
dispatch does not require exposing HList selection indices to service consumers.

Static dispatch is the default, not an obligation. A closed service can use
`Arc<dyn Behavior>` through the same accessor. A context-indexed service can be
erased for a fixed context, but its `HasX` trait using `Behavior<Self>` is not
itself dyn-compatible. Async methods may need an explicit erased future at a
chosen dynamic boundary. Do not add boxing to all callers to support an unused
erasure case.

## Migration and verification

Migrate one complete consumer path: identify its actual dependencies and owner,
define the behavior contract, wire the service, move the consumer, and remove
the superseded access path. Preserve current scheduling, cancellation and reuse
semantics unless the change explicitly repairs one of them. Do not redesign
persistence, source identity and capability access in one parcel.

Compile real consumers and their tests early. Run behavior checks through the
actual service and issuance path, including refusal, partial progress, drop and
shutdown where relevant. Test shared-producer versus waiter cancellation when
the service reuses work. Use independent generated histories for stateful
contracts; a mock returning its input does not establish the production invariant.

Type-level controls should demonstrate missing capabilities, invalid wiring and
non-cloneable permits at the actual boundary. Language-only probes establish that
a pattern is expressible, not that the application is correct. Do not expose
private authority constructors merely to make an external compile-fail fixture
convenient. Record any unavailable negative-test boundary explicitly.

Review both the local contract and joined production behavior before integration.
Traits alone do not establish faster compilation or runtime execution. Keep large
algorithms behind narrow interfaces where unnecessary generic specialization
would multiply code generation; measure before choosing dynamic dispatch.

## Language references

The context-indexed and captured-context forms, owned `Send + 'static` async
composition, and intended invalid constructions were checked with the repository's
pinned Rust 1.93 compiler during design. That is language-design evidence, not
release qualification.

- [Associated types and bounds](https://doc.rust-lang.org/reference/items/associated-items.html#associated-types)
- [Trait implementation coherence](https://doc.rust-lang.org/reference/items/implementations.html#trait-implementation-coherence)
- [Dyn compatibility](https://doc.rust-lang.org/reference/items/traits.html#dyn-compatibility)
- [Arc ownership and thread safety](https://doc.rust-lang.org/std/sync/struct.Arc.html#thread-safety)
- [Impl-side dependencies in Context-Generic Programming](https://patterns.contextgeneric.dev/impl-side-dependencies.html)

The CGP separation is useful design precedent; this migration does not add a CGP
dependency. Shared library adoption requires a demonstrated production benefit.
