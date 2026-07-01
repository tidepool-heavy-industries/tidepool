# tidepool-repr — Core IR types + CBOR wire format

The shared IR and serialization boundary: everything downstream (`tidepool-eval`
the oracle, `tidepool-optimize`, `tidepool-codegen` the JIT) consumes `CoreExpr`
built here. **Read the repo-root `CLAUDE.md` Key Decisions Reference FIRST** —
it defines the `CoreFrame` variants this doc's traversal code is written
against; nothing here re-lists them. This doc goes deeper on 4 of the crate's
~14 source files — the shared tree/table/id/wire-format machinery
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

**Whole-tree operations (`extract_subtree`, `replace_subtree`, `free_vars`) are
explicit-stack post-order walks (`Enter`/`Exit` work-item enum), not recursive
functions** — deliberately, so arbitrarily deep Core towers don't grow the Rust
call stack (shared child-scheduling via `for_each_child_rev`). The memo differs
by walk, same underlying idea: `extract_subtree`/`replace_subtree` use
`old_to_new: HashMap<usize,usize>` (presence marks "already emitted," which
both memoizes the walk and preserves DAG sharing — a node reachable from
multiple parents is emitted once); `free_vars` uses its own
`FxHashMap<usize,FxHashSet<VarId>>` (a per-node free-variable-set memo, not an
index remap) — don't grep for `old_to_new` inside `free_vars`, it isn't there.
If you add a new whole-tree pass here, follow the Enter/Exit pattern rather
than writing a recursive `fn walk(&self, idx)` — a deep tower will silently
overflow a naive recursive walk in a way that only shows up on real -O2 Core,
not small tests.

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

## CBOR wire format (`serial/mod.rs`)

8-byte header: 4-byte magic `TPLR` + `VERSION_MAJOR`/`VERSION_MINOR` (currently
`1.0`) as two big-endian `u16`s. **The header is OPTIONAL, not mandatory** —
`read_cbor` only checks it if the first 4 bytes match the magic; payloads
without it fall through to a legacy parse path silently, they are not
rejected. When the magic IS present, `read_cbor` rejects a version it can't
read loudly (`ReadError::UnsupportedVersion`) — specifically a `major`
mismatch, or a `minor` newer than this build supports; an older `minor` within
the same `major` is accepted (forward-compatible by design). Bump
`VERSION_MAJOR` on any breaking change to the metadata/expression CBOR shape,
and expect `haskell/`'s serializer to need a matching bump (the Haskell and
Rust sides of this format are NOT independently versioned; they're one format
with two implementations).
