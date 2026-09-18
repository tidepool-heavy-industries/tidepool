# What was checked before dogfooding, and what was not

2026-09-17. Written so that anything which breaks in a run can be matched
against what was actually verified rather than what was assumed.

## Landed in this tree (`tidepool-jev`, `feat/jev-effect`)

Two commits, both from defects found by driving a resident workbench all day
rather than from a plan.

**`8ea5fdf15` the notebook stops fighting the person using it.** A cell ending
in `pure x` now runs instead of being refused with an ambiguous `Applicative`.
A declaration can be redeclared, which is the whole point of a workbench and
was previously a hard error that also made a partially-failed cell collide with
its own retry. The workbench preamble imports `Nil`, without which every Jev
packet copied from a skill example failed on its last line.

**`d9a6f205b` an agent can find the modules its own workspace authors.** A
dotted capitalized `lookup` query routes to the module browse that already
existed with no callers, and `doc` lists the workspace's own modules beside the
built-in topics.

### Focused checks, all passing

| group | what it covers | result |
|---|---|---|
| `session::turn` and `session::workbench` | the cell templates and the preflight, including both retry paths | 71 passed |
| `declarations_accumulate_and_types_coexist`, `function_redeclaration_shadows_and_a_single_declaration_stays_visible` | declarations across cells, and shadowing; the second drives real GHC | 2 passed |
| `dotted_capitalized_query_is_a_module_lookup_and_other_names_are_unchanged`, `topics_and_unknown_topic_error_name_configured_workspace_modules` | discovery routing and the topic list | 2 passed |

Beyond the tests, every one of these ran continuously for a day: roughly forty
cells were submitted through `shoal proxy` against a live session, including
every cell now in `plans/jev-lab/breadth/`. That is the real evidence for the
notebook fixes.

**Binary**: `target/debug/shoal` is current against the committed tree. No
source file under `tidepool-actor/src`, `tidepool-runtime/src` or `tidepool/src`
is newer than it.

### Unverified, and specifically so

- **The discovery change is incomplete and its commit says so.** It routes the
  query and lists the topics. It was not carried through to worked-invocation
  discovery, which was the actual point: an agent should be able to find not
  just that `Project.Investigate` exists but how to call it. The agent doing
  this work was stopped partway.
- **No broad battery was run.** No `just verify`, no full suite for either
  crate. The 75 tests above are the named tests these changes can break, chosen
  deliberately; anything outside them is unchecked.
- **Nothing was checked on the prepared route beyond live use.** `shoal check`
  typechecks and extracts but does not run prepared codegen, so the only proof
  the workbench compiles is that actors started and cells ran, which they did
  all day.
- The `Nil` import fix takes effect on a new session launch only.

## On a branch, reviewed, not merged (`work/flake-haskell-sources`)

Four commits in `~/dev/tidepool/.claude/worktrees/flake-haskell-sources`. A
project pins an external repository through its own flake, names the Haskell
source directories inside it, and imports those modules.

**Review: the design is right and I would take it.** It found that no new
mechanism was needed. A fetched flake input becomes an ordinary source root,
after which the existing workspace loader captures and content-hashes it, the
toolchain cache keys it automatically because captured source becomes an
`--include` root, and every actor already shares that include list. It checked
the process mount boundary rather than assuming, and found that boundary wraps
the interactive coding-agent subprocess while Haskell compilation happens in the
host process, so actor access was never a mount question. That is the kind of
check that usually gets skipped.

Its own reported tests: a capture-and-rekey test, and an end-to-end test where a
flake-pinned module answers a prepared cell, returning 42. The second costs
about 230 seconds uncached.

**One review finding it did not raise.** The workspace itself is handed to `nix`
as a bare directory, deliberately, because a `path:` URL would copy build trees
into the store. But an override is passed as `--override-input <name>
path:<directory>`, which is exactly that copy, for the overridden input. For a
small sibling checkout this is fine and for a large one with build artifacts it
will be slow. Worth a note in the documentation or a `git+file://` form that
respects ignore rules.

**Also flagged by that worker, and true**: `capture_tree` rejects symlinks
outright, so a pinned repository containing a symlink under a named source
directory fails. And `tidepool/src/actor_host/resource_tests.rs:139` fails
`cargo fmt --check` as committed at `1864e7c6a`, which predates all of this
work.

## Still running (`standby/opus`)

The `Reflect` effect, letting an actor read its own recent conversation turns.
Two commits so far, a reader and a test that it answers the executing actor or
nobody, with more uncommitted. Not reviewed, not tested by me.

## Merge hazards when these three come together

- **`tidepool-actor/src/prompt_catalog.rs`** is edited by the discovery commit
  here and by the in-progress Reflect work, which is adding a `reflect` doc
  topic. Same file, same list, compatible intent, certain textual conflict.
- **`tidepool/src/actor_host.rs`** is edited by the notebook commit here (the
  preamble import) and by Reflect (about forty lines). Different regions.
- Both branches are cut from `1864e7c6a`, which is an ancestor of this tree, so
  the merges are ordinary.
