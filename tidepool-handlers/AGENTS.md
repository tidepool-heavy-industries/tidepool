# Concrete effect interpreters

This crate owns Rust implementations of effect requests and effect-stack
assembly. Effect/verb definitions originate in `tidepool-protocol` and
`tidepool-mcp`; worktree primitives and coding backends remain in their owning
crates.

- Keep one module per effect and one handwritten method per generated request
  constructor. Dispatch is nominal; union position is not a handler slot.
- Add operations at the single-source schema/definition first, then implement
  the generated request here. Never hand-copy a parallel wire contract.
- Choose `respond`, `respond_list`, or a typed per-verb failure from the result
  shape. Ordinary domain refusal is data; corruption remains `EffectError`.
- Stack/profile assembly is the capability boundary. Definitions convey no
  resource authority; handler configuration and runtime-issued handles do.
- Filesystem read/write share the canonicalized rooted backend. `Exec` is an
  unrestricted host process whose initial CWD is constrained; do not claim it
  is a filesystem sandbox.
- Subagent blocking, stepped, and async forms are combinators over one
  `CycleSaga`. Do not create another delegation implementation or let Rust
  re-enter the JIT to run a Haskell tool handler.
- Handler-owned child tables and processes must be bounded and reaped on drop.
  Cancellation settles exactly once and retains worktree evidence.
