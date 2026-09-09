# Typed file tools for resident Haskell

## Motivation

Agents should not routinely implement file edits with Python string slicing,
repeated `sed` calls, or shell heredocs. Those scripts obscure intent, discard
the identity of the source that was inspected, and make ambiguous matches or
partial edits difficult to assess.

Provide useful file-reading and editing operations as ordinary typed Haskell
ingredients, backed by the existing native file/patch machinery. This is a
proposed capability, not an implemented API. Until it exists, prefer the current
patch tool over ad-hoc source-rewriting scripts when it fits the operation.

## Desired experience

Read a file or relevant region once, retain its identity/version, describe the
change, preview it, then apply it against the version that was actually read.
Compose independent operations with ordinary Haskell rather than generating a
new shell script for every batch.

Illustrative API shape only:

```haskell
source <- readSource path
let edit = replaceUnique oldDefinition newDefinition
proposal <- prepareEdit source edit
inspectFull (editDiff proposal)
```

Preview is optional. A caller that needs it can apply the retained proposal:

```haskell
receipt <- applyEdit proposal
inspectFull receipt
```

The [command workbench design](next/haskell-command-workbench.md) adds the desired
`edit path $ do ...` convenience: sequential in-memory operations with one
all-or-none single-file application, without a mandatory preview/approval turn.
Both surfaces should share the same mutation owner and preconditions.

Collections use the same operations, for example `traverse prepareReplacement
inputs`, where the parent defines `prepareReplacement` for the task. A bespoke
bulk-edit command should not be necessary merely to iterate over files.

## First useful primitives

- Read a whole file or selected region with explicit truncation metadata and a
  retained source version. Keep paths and ranges structured.
- Search with structured matches, file identity and ranges, not text that the
  caller must parse from terminal output. Reuse existing search implementations.
- Replace an exact unique match; insert at a verified anchor; apply a patch;
  create a file only if absent. Zero or multiple matches are typed failures,
  not invitations to guess or silently edit the first occurrence.
- Preview a diff without mutation. Applying the proposal must check that its
  preconditions still hold; never silently apply to a newer file version.
- Return receipts distinguishing applied changes, conflicts, no-op results and
  failures. Expose which work completed when a batch fails partway through.

Default to exact operations with honest failure rather than fuzzy matching.
Syntax-aware selection can follow where it materially improves a real task:
replace a named declaration, adjust imports, or edit a configuration key. Reuse
the owning parser/language tooling instead of writing another partial parser.
Report unsupported syntax explicitly; do not pretend regex selection is an AST.

## Ownership and safety contracts

- Haskell describes and composes edits. Existing Rust/native owners enforce
  workspace authority, path handling and mutation; do not add a second file
  service, path resolver or durable edit journal.
- A retained source value is evidence of what was read, not authority to write.
  Applying an exported edit from a small worker must respect that worker's grant
  or an explicitly authorized parent-application boundary.
- Enforce stale-source checks at the mutation owner, with a defined concurrency
  boundary. A caller-side check followed by an unrelated write is insufficient.
- Define file creation, symlink, encoding, newline and permissions behavior.
  Preserve untouched bytes and do not normalize an entire file incidentally.
- Single-file atomicity and multi-file transactionality are different promises.
  `traverse`/`sequence` do not provide rollback. Do not label a partially applied
  batch successful; retain per-file outcomes for review and recovery.
- Same-worktree agents need coordinated write ownership. Version checks detect
  stale assumptions but do not replace the parent's task/file assignment policy.
- Text edits are not proof of semantic correctness. Compile or test relevant
  consumers after applying a change, using the owning repository guidance.

## Fit with programmable small agents

See [small typed agents](small-agents.md). A task-specific `.hs` tool wrapper can
expose a narrower operation such as proposing a change to one declaration or
returning a patch for parent review. The worker need not receive arbitrary shell
execution or write access to the entire parent worktree.

Keep a stable tool interface across repeated tasks; varying paths and typed
inputs belong in task data rather than regenerated tool descriptions. These
operations should also be useful directly to full-context resident actors.

## First implementation slice and verification

1. Inventory current read/search/patch tools and file effects, then identify the
   owning entry points and missing contracts. Use `AGENTS.md`, live signatures
   and production consumers; do not assume that every sketch above needs a new
   backend operation.
2. Implement one vertical read → unique replacement → preview → checked apply
   path with typed outcomes. Connect it to a real resident Haskell consumer.
3. Test successful editing, no/ambiguous match, stale source, concurrent mutation,
   unchanged content, file creation conflicts, permission rejection, and the
   failure/partial-completion behavior actually promised by the implementation.
4. Exercise the exact model-facing examples through the resident workbench.
   Demonstrate replacing a real ad-hoc editing script with a shorter composed
   operation; avoid an abstraction used only by its own new tests.

Open decisions: source-version representation, existing patch-owner integration,
structured search surface, size/truncation limits, and which syntax-aware edit
first justifies language-specific support. Do not start with a universal editor
DSL or a cross-language AST framework.
