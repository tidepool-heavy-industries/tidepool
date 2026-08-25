# tidepool-repr — Core IR types + CBOR wire format

**Charter.** Belongs: `CoreExpr`/`CoreFrame` IR types, `DataConTable`,
free-variable analysis, and the CBOR wire format — the shared representation
every downstream crate consumes. Does NOT belong: evaluation (`tidepool-eval`),
JIT compilation or heap byte layout (`tidepool-codegen`), Cast/Tick/Type
erasure (stays in the Haskell serializer, never ported to Rust).

The shared IR and serialization boundary: everything downstream (`tidepool-eval`
the oracle, `tidepool-optimize`, `tidepool-codegen` the JIT) consumes `CoreExpr`
built here. **Read the repo-root `CLAUDE.md` Key Decisions Reference FIRST** —
it defines the `CoreFrame` variants this doc's traversal code is written
against; nothing here re-lists them. This doc goes deeper on 4 of the crate's
~17 source files — the shared tree/table/id/wire-format machinery
(`tree.rs`, `datacon_table.rs`, `session_ids.rs`, `serial/`). It does NOT cover
`frame.rs` (the `CoreFrame`/`VarId`/`Literal` type defs themselves — that's
root's territory), `types.rs` (`PrimOpKind`/`define_primops!`), or
`builder.rs`/`normalize.rs`/`subst.rs`/`varid_check.rs` (expression
construction/normalization helpers) — read those directly if you need them.

## `RecursiveTree` — self-rolled flat-vector scheme, not an external hylo crate

`CoreExpr = RecursiveTree<CoreFrame<usize>>` (`tree.rs`) is a `Vec<CoreFrame<usize>>`
where child positions are indices into the same vector, not pointers or an
external recursion-schemes library. Root is conventionally the **last** node.
`MapLayer` is the one-layer functor map (`CoreFrame<A> -> CoreFrame<B>` given
`A -> B`) every traversal is built from.

**Whole-tree operations (`extract_subtree`, `replace_subtree`) are
explicit-stack post-order walks (`Enter`/`Exit` work-item enum), not recursive
functions** — deliberately, so arbitrarily deep Core towers don't grow the Rust
call stack (shared child-scheduling via `for_each_child_rev`), memoized via
`old_to_new: HashMap<usize,usize>` (presence marks "already emitted," which
both memoizes the walk and preserves DAG sharing — a node reachable from
multiple parents is emitted once). If you add a new whole-tree pass that isn't
naturally forward-ordered, follow this Enter/Exit pattern rather than writing
a recursive `fn walk(&self, idx)` — a deep tower will silently overflow a
naive recursive walk in a way that only shows up on real -O2 Core, not small
tests.

**`free_vars.rs`'s `FreeVarsIndex` is the one free-variable engine** (moved
here from `tidepool-codegen/src/emit/free_vars_index.rs` on consolidation —
codegen's copy was deleted). It's a single **forward** pass over
`0..nodes.len()` (not Enter/Exit) computing every node's free-variable set
once, `Rc`-shared; `free_vars(tree)` is just
`FreeVarsIndex::compute(tree).free_vars_at(root)`. The forward order alone is
stack-safe (no recursion, no explicit stack) because `RecursiveTree`'s
invariant guarantees every child index is less than its parent's. Reach for
`FreeVarsIndex` directly (not `free_vars`) when you need free variables at
more than one node of the same tree — one `compute` amortizes across every
query.

## `DataConTable` — use `insert_checked`, not `insert`, for any new ingestion path

`insert` silently overwrites on id collision (kept for tests/simple construction).
**`insert_checked` is the load-bearing guard** — it's the fix for the class of
bug that evicted freer-simple's `Union` from the table (two distinct
constructors hashing to the same 56-bit `stableVarId`): it compares
module-qualified identity (falling back to unqualified name) and only errors on
a genuinely different constructor at the same id; the same constructor
re-encountered from multiple metadata sources is a silent no-op. See
`haskell/CLAUDE.md`'s `TIDEPOOL_VARID_AUDIT` knob for diagnosing a collision
from the Haskell side — this crate is where it's actually caught.

**Same-name, different-type constructors** (`Bin`/`Tip` from `Data.Map` vs
`Data.Set`) are disambiguated by `get_companion` using sibling groups built by
`populate_siblings_from_expr` (constructors that co-occur as `Case` alternatives
are type-siblings) — falls back to `get_by_name_arity` if no sibling info was
populated for that expression yet. `get_by_name` deliberately returns `None` on
ambiguity rather than guessing; use `get_by_qualified_name` when you have it.

## Session identifiers (`session_ids.rs`) — the Rust-side home for `tidepool-repl`'s stable ids

Newtypes only — never bare `u64`/`String` — so invariants live on the type:
`Generation` (monotonic, never reused), `SessionId`, `SessionModule` (the ONE
place on the **Rust side** `"Tidepool.Session.{Val|Lib}.G<g>"` is constructed —
render through `.module_name()`, don't hand-format the string elsewhere), and
`SessionVarId`. **This format is also built independently on the Haskell side**
(`sessionModuleString`, `haskell/src/Tidepool/Session.hs`) — the two must stay
byte-identical by hand; there's no shared formatter across the language
boundary, so a format change here needs a matching change there.

**`SessionVarId`'s hash is minted exactly once, in the Haskell extract**
(`Translate.stableVarId`, `0xFE<<56 | fingerprintString("<module>:<occ>")`).
Rust only stores and re-seeds it into `ExternalEnv` on later reference turns —
it never recomputes the fingerprint. This is deliberate: there is no
cross-language hashing algorithm to keep in sync, by construction.

## CBOR wire format (`serial/mod.rs`) — ONE current format, no tolerance

8-byte header: 4-byte magic `TPLR` + `VERSION_MAJOR`/`VERSION_MINOR` (currently
`3.0`) as two big-endian `u16`s. **The header is MANDATORY** — a payload
without it is rejected loudly (`ReadError::MissingHeader`); stale fixtures or
caches get regenerated, never tolerated. Version rejection is also loud
(`ReadError::UnsupportedVersion`): a `major` mismatch, or a `minor` newer than
this build supports; an older `minor` within the same `major` is accepted.
Metadata has exactly one accepted shape: `[entries_array, warnings_map]` with
strictly 9-element entries — the shape `Tidepool.CborEncode.encodeMetadata`
emits (the 9th being rendered field types, in field order — added in `3.0`
alongside the 8th element's parent-type-name from an earlier bump). The Rust
writer mirrors the Haskell encoder byte-for-byte (always-9
entries in ascending-`DataConId` order, same warnings-key emission rules);
`tidepool-repr/tests/golden_wire_contract.rs` pins the byte identity for both
trees and metadata over the committed corpus. Bump `VERSION_MAJOR` on any
breaking shape change, bump `haskell/`'s serializer in the same commit — the
version bytes are hardcoded in TWO places with no shared formatter:
`VERSION_MAJOR`/`VERSION_MINOR` here AND `Tidepool.CborEncode.tplrHeader`'s
byte literal (the `3.0` bump missed the latter on first pass) — and
regenerate the fixture corpora (`haskell/regen-corpus.sh` + the extract
invocations in `haskell/CLAUDE.md`) — the Haskell and Rust sides are one
format with two implementations, and the regenerated corpus diff is the
review artifact.

**A purely ADDITIVE optional warnings key is a MINOR bump, and must not need
fixture regeneration.** `2.1` (the `poisoned` key: sentinel slot → the
qualified name of the unresolved external each `0x45`-kind-4 node replaced) is
the worked example. The rules that make it safe: the reader accepts an older
minor within the same major, an absent optional key decodes to empty, and the
writer omits it when empty — so every committed `2.0` payload still reads,
re-encodes byte-identically (`golden_wire_contract` compares payloads, not
headers), and is NOT regenerated. Bump both sides in one commit, and pin the
older-minor read (`read_metadata_accepts_previous_minor_without_poisoned_key`).
A key that is not optional, or any change to an existing key's shape, is a
MAJOR bump with the full regeneration dance above.

## Version ladder (`version_ladder.rs`) — the one migration mechanism for durable, non-reproducible persistence artifacts

`serial/`'s CBOR versioning above answers the wire format for `.cbor`
fixtures — build artifacts that a breaking bump can just regenerate.
`version_ladder.rs` answers the different case: durable artifacts that are
NOT reproducible (the harness log, worktree/handlers journals, the
selfharness transcript, a `Checkpoint`, a harness's own `State` blob) — the
durable record of one real operator's run, which a breaking change must
migrate forward, never regenerate.

**Per-kind, not per-repo.** Each artifact kind owns its own `floor`/
`current`/migration table, mirroring how `serial::VERSION_MAJOR`/
`VERSION_MINOR` scope to one wire format rather than the whole repo. This
module supplies only the fold (`migrate_to_current`); each consumer supplies
its own bounds, its own `Migration` functions, and its own typed,
path-carrying rejection on top of [`LadderError`] (a
`PersistenceError::BelowFloor`, a `WorktreeError::JournalFutureVersion`, …).

**The unstamped-file convention.** [`found_version`] reads a payload with no
top-level `"version"` key as `0`, never as the kind's current version — so an
artifact written before this scheme existed keeps loading: a consumer's
`floor` starts at `0`, and its migration table's first entry is the `0 -> 1`
step that turns the unstamped shape into an explicitly-versioned one.

**The fold.** [`migrate_to_current`] walks `value` forward from its stamped
`found` version to `current` through a `&[Migration]` table indexed from
`floor` (`migrations[0]` is the `floor -> floor + 1` step, and so on) —
`N` versions cost `N - 1` small, independently-testable steps, never one
from-any-version-to-current function. Rejects loud and typed outside
`[floor, current]` (`LadderError::BelowFloor`/`UnsupportedVersion`) — never a
silent reset; a below-floor artifact must be archived/deleted and restarted
fresh, or read with an older build that still carries the migration path.
