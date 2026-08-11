# Compile memo: one content-addressed cache for harness turn compiles

Status: design, ahead of implementation on `root.compile-memo`.
Approved by the human (2026-08-11): *"sharing the content-addressed cache
compile path is great — deleting a parallel path is a great bonus."*

## What this reverses

`tidepool-harness/src/compile.rs` opens with a deliberate decision:

> This is deliberately NOT a fork of the runtime's caching compile: turns are
> one-shot `M a` expressions, the sidecar is small, and the extract call is the
> ~2s floor either way.

That reading was right about ONE turn and wrong about the suite. The
harness's fixed-source compiles — the boot seeds, the answerer's
`resume`/`result` turns, the outer `render`/`loop` fragments — are compiled
from IDENTICAL source, with an IDENTICAL include set, by an IDENTICAL extract
binary, once per test PROCESS, across ~200 harness test processes. The census
measured the same scenario at 50s cold vs 20s warm. The ~2s floor is not a
floor when it is paid two hundred times for the same bytes.

So: the cache-free path is deleted. Every `compile::compile_turn` /
`compile_turns` call goes through a content-addressed memo. There is no
bypass flag — one mechanism, all callers.

## Decision 1 — the cache lives in `tidepool-runtime::cache`, not at the `ExtractCmd` layer

`tidepool-extract-cmd` is the tempting home: it is the ONE place an extract
invocation is built, so keying on the complete invocation there would cover
every caller in the workspace by construction. It is nonetheless the WRONG
home today, for a reason written into its own `Cargo.toml`:

> ZERO dependencies, deliberately (`plans/post-restart/extract-manifest.md`,
> D-A): `tidepool-macro` is a proc-macro crate that must be able to depend on
> this without dragging Cranelift and the rest of the runtime graph into every
> downstream crate's BUILD graph. std only. Keep it that way.

A content-addressed memo needs blake3 (keying, checksums) and atomic
tempfile-rename (store). Putting them there pushes two hashing/IO crates into
the BUILD graph of every crate that transitively expands `haskell_eval!`.
That is a real cost paid by everyone to serve one caller, and reversing a
locked-by-comment decision is not this lane's call.

`tidepool-runtime/src/cache.rs` already owns the discipline this needs, and
`tidepool-harness` already depends on `tidepool-runtime`. So the mechanism is
generalized THERE:

- `cache.rs` grows an N-artifact, invocation-keyed layer alongside the
  existing 2-artifact eval layer;
- `compile_haskell` (eval) and `compile_turns` (harness) are its two
  consumers;
- the module becomes `pub` so the harness can reach it.

The `ExtractCmd` home stays reachable later without redesign: the new key is
computed FROM `ExtractCmd::argv()` (see Decision 2), so the key builder is
already invocation-shaped. If extract-cmd ever gains deps, the builder moves
down a crate and nothing above it changes.

## Decision 2 — keying spec

The key is namespaced (`frame(b"tidepool-invocation-artifacts-v1")` leads the
hash), so it can NEVER collide with an existing eval key. The eval key's own
bytes are untouched: no mass invalidation of `~/.cache/tidepool`.

Inputs, all length-prefix framed (the existing `frame` helper — NUL
separators alone let an embedded NUL shift bytes across a field boundary and
serve the wrong artifact):

1. **Source CONTENT**, never the path. The on-disk `<Module>.hs` lives in a
   per-invocation tempdir; its path is noise.
2. **The argv, filtered through an ALLOWLIST.** This is the anti-drift
   mechanism, and it answers hazard (a) structurally rather than by
   vigilance. `compile_turns` builds its `ExtractCmd` first and hands
   `cmd.argv()` to the key builder, which walks it:
   - `--output-dir <dir>` → dropped (per-invocation, affects nothing in the
     output bytes);
   - `--include <dir>` → dropped HERE and content-fingerprinted below;
   - `--target <name>` / `--targets <a,b>` → framed verbatim, in order;
   - the positional input, iff it equals the known input path → dropped;
   - **anything else → the invocation is UNCACHEABLE (key builder returns
     `None`, caller compiles cold).**

   Default-deny, the same membership discipline `.config/nextest.toml`'s
   `ghc-heavy` group uses. A flag added to `ExtractCmd` tomorrow and threaded
   into this call site does not silently ride along unkeyed: it makes the
   compile cold until someone consciously classifies it. The failure
   direction is a miss, never a false hit.
3. **Include dirs: ordered, content-fingerprinted, PATH-INDEPENDENT.** Count,
   then per dir in ORIGINAL order (search-path order decides module
   shadowing — `[A,B]` and `[B,A]` are different compilations), a recursive
   walk hashing `(path RELATIVE to the include root, blake3(contents))` for
   every `.hs`/`.hs-boot`, sorted, with the existing canonicalized-visited
   cycle guard.

   The path-independence is a deliberate DIVERGENCE from the eval key, which
   frames each include root's absolute path. It is what makes the memo
   shareable across test processes at all: the generated effects-module dir
   and the fixture dirs live under each test's own tempdir, so an
   absolute-path-keyed memo would miss on every single test. It is sound
   because the absolute location of an include dir does not reach the output
   bytes — types are stripped and Cast/Tick/Type erasure happens in the
   Haskell serializer (Key Decisions Reference), so Core carries no source
   spans; module identity comes from the path RELATIVE to the search root,
   which IS keyed.
4. **The extract binary, by CONTENT.** `binary_content_hash` (memoized in
   process and in a machine-wide sidecar) of the explicitly-supplied binary,
   plus the content hash of every wrapper `exec` target — the existing
   machinery, unchanged, so an extract rebuild forces a miss. Content only,
   no path: the same binary bytes at two install locations IS the same
   compiler, and pinning the path would again defeat sharing.

### Hazard (b) — session-scope compiles are OUT of v1, by construction

`--session-bind` / `--inject-val` / `--session-root` compiles read per-session
MUTABLE directories whose content is not covered by anything above. They are
excluded, and the exclusion is not a comment: those flags are not on the
allowlist, so such an invocation keys to `None` and compiles cold. They also
do not occur on this path — `compile_turns` sets exactly
`input`/`output_dir`/`targets`/`includes` — and the session lane
(`tidepool_runtime::session::turn`) is untouched by this change. The
test-suite win lives entirely in the fixed-source boot/answerer/render
compiles. Widening to session scope is a MEASURE-FIRST follow-up, not a
freebie.

### Hazard (c) — a hit is observationally identical

The memo stores the FULL artifact set the caller reads, not just the Core:

- `meta.cbor` (the shared merged `DataConTable` + warnings),
- `<target>.cbor` for every requested target,
- the asks sidecar — `asks.json` for one target, `<target>.asks.json` for
  more than one, keyed on the same `targets.len() > 1` test the Haskell side
  uses to decide which shape to WRITE.

An artifact may be recorded as ABSENT (distinct from empty): an extract
predating the asks pass writes no `asks.json`, and `parse_asks(None)` yields
an empty sidecar — a hit must reproduce that, not fabricate `[]`.

Structurally, the hit path and the miss path both produce
`(meta_bytes, Vec<RawTargetOutput>)` and then call ONE shared tail that
deserializes, registers var names / poisoned externals, parses asks, and logs
`"compiled turn"`. Observational identity is therefore a property of the code
shape, not of two branches kept in sync by hand.

The one deliberate difference: a hit records no `extract_spawn` timing stage
and forwards no `extract.*` phases, because no process was spawned. That is
the measurement working, not a divergence — the `cbor_read` /
`cbor_deserialize` / `asks_parse` stages are still recorded, over the memo's
bytes.

Only a compile that both SUCCEEDED and DESERIALIZED is stored — the same
discipline `compile_haskell` keeps ("only store in cache if deserialization
succeeded"), so a malformed artifact set can never be memoized into a
permanently-failing entry. A failed extract is never stored at all.

### Store format

`{key}.a{i}` per present artifact, plus a `{key}.ok` sentinel written LAST
carrying a manifest: the artifact count, then per artifact its logical NAME,
a presence byte, and blake3 of its bytes. `load` reads the sentinel first,
requires the manifest's names to equal the caller's expected names in order,
then verifies each file's blake3 against the manifest. A crash mid-store, a
slot-set mismatch, or a bit-flip that still decodes as plausible CBOR all
fall through to a MISS. Files are persisted by atomic rename; the sentinel is
removed first and rewritten last.

The variable-length manifest cannot be confused with the legacy eval
sentinel, which `cache_load` requires to be exactly 64 bytes — and the
namespaced key means the two never name the same file anyway.

## Decision 3 — memo sharing across test processes

`paths.rs` grows `compile_cache_dir()`: `$TIDEPOOL_COMPILE_CACHE_DIR` if set,
else `cache_dir()` — **the default layout is unchanged**, so no user's cache
moves. `cache.rs` resolves its directory through it, which shares the
`binfp-*` binary-fingerprint sidecars too (a per-test-isolated cache re-hashes
the ~79MB extract binary in every one of ~200 processes).

`tidepool-harness/tests/support/mod.rs`'s `isolate_cache()` splits its two
jobs:

- **mutable session state stays per-test isolated** — `XDG_CACHE_HOME` still
  points at a fresh `TempDir`, so checkpoints, transcripts, `log.jsonl`, the
  KV path, the generated effects module and the materialized stdlib remain
  private to the test;
- **the compile memo is SHARED** — `TIDEPOOL_COMPILE_CACHE_DIR` points at a
  stable directory derived from the AMBIENT cache home read before the
  isolation (so `XDG_CACHE_HOME=$PWD/.cache scripts/battery.sh …` puts it
  under `$PWD/.cache`), honoring an explicit `$TIDEPOOL_COMPILE_CACHE_DIR`
  when the caller set one.

**Why sharing is safe:** the memo is content-addressed. Two tests reach the
same entry only when the source content, the allowlisted argv, the include
CONTENT, and the extract binary content are all identical — in which case
they are the same compilation and are entitled to the same bytes. A test that
writes a different fixture gets different include content and therefore a
different key; there is no path, pid, or ordering input by which one test can
observe another's state. Concurrency is safe for the reason
`.config/nextest.toml` already records for the eval cache: atomic rename per
file, sentinel written last, checked first — a reader racing a writer sees a
clean miss, never a torn file. Two writers racing on one key write identical
bytes.

The function KEEPS its name. A rename would touch four test files a sibling
lane (`test-diet`) is editing concurrently — the behavior is what splits, and
the call sites stay put.

## Adversarial tests (the keying spec is the thing that can silently poison everything)

In `cache.rs`'s unit tests — pure Rust, no GHC, run via
`--ignore-default-filter -p tidepool-runtime --lib`:

- two invocations differing ONLY in a target name, ONLY in target ORDER, ONLY
  in a byte of one include file, ONLY in include ORDER, ONLY in source
  content, ONLY in extract binary content → four distinct keys, all mutual
  misses;
- the SAME content with include dirs at DIFFERENT absolute paths → the SAME
  key (the sharing property, asserted rather than assumed);
- an argv carrying a non-allowlisted flag (`--session-bind`, `--turn`) →
  `None`, i.e. uncacheable;
- artifact roundtrip: present + ABSENT artifacts survive; a missing sentinel,
  a wrong expected-name set, and a corrupted artifact each read as a miss.

## Not in scope

- The session lane's compiles (hazard b, above).
- Moving the key builder into `tidepool-extract-cmd` (Decision 1).
- Sharing the materialized stdlib / generated effects dirs across test
  processes. Same content-addressed safety argument would apply, but their
  writers' atomicity has not been audited here and the measurement does not
  yet demand it.
