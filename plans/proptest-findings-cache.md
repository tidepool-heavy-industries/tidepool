# S5 Cache-Layer Findings — `tidepool-runtime/src/cache.rs`

Suite: `tidepool-runtime/tests/proptest_cache_layer.rs`
(`cargo test -p tidepool-runtime --test proptest_cache_layer` — 18 green, 8 `#[ignore]`d bug-twins)

**Method.** `cache_key`/`cache_load`/`cache_store` are `pub(crate)` in a private
module, so the suite drives the cache behaviorally through public
`compile_haskell` with `XDG_CACHE_HOME` pointed at a per-test tempdir and
`TIDEPOOL_EXTRACT` pointed at a stub shell script that copies fabricated CBOR
fixtures and bumps an invocation counter. **Counter delta = HIT/MISS oracle**,
which makes key equality observable without calling `cache_key`. Real GHC /
tidepool-extract is never invoked; `~/.cache/tidepool` is never touched.

Convention: green tests assert *current* (including buggy) behavior; each bug
has an `#[ignore = "BUG: ..."]` twin asserting correct behavior. `--ignored
--skip symlink_cycle` fails all 8 twins — every finding is double-confirmed.

## THE STALENESS VERDICT (headline question)

**The extract binary IS fingerprinted into the cache key** —
`extract_binary_fingerprint` (cache.rs:34,42) hashes the resolved binary's
(path, size, mtime), and chases shell-wrapper `exec` lines to fingerprint the
delegate binary too. The real `~/.cargo/bin/tidepool-extract` wrapper uses an
unquoted absolute `exec /home/.../tidepool-extract-bin "$@"`, which the parser
follows correctly (verified: `staleness_unquoted_wrapper_target_followed`).
So the #313-era "rm -rf ~/.cache/tidepool after every rebuild" footgun is
**fixed for the standard flow** — a rebuild changes size/mtime → new key.

**BUT three stale-cache routes remain open** (F3, F4, F5 below). The
CLAUDE.md advice to clear the cache after binary updates is still warranted
exactly when: the binary lands with preserved size+mtime (nix store normalizes
mtimes to epoch+1; `cp -p`; `rsync -t`), the wrapper quotes its exec target,
or dependency `.hs` files are symlinks.

## Bug table

| ID | Class | Severity | What | Green repro / ignored twin |
|----|-------|----------|------|-----------------------------|
| F1a | B5 key collision | Medium | `cache_key` hashes `source \0 target \0 ...` with **no length framing** — a NUL in the source shifts bytes across the field boundary: `key("a\0b","c") == key("a","b\0c")`. The second pair is served the first pair's artifact without any compile. | `key_collision_nul_source_target_boundary` / `key_should_separate_source_from_target` |
| F1b | B5 key collision | Medium | Same construction across the target/include boundary: include roots are hashed (path bytes + `\0`) even when nonexistent, so `key(s,"t",["/p"]) == key(s,"t\0/p",[])`. | `key_collision_target_vs_include_root` / `key_should_separate_target_from_include_roots` |
| F2 | B5 wrong artifact | **High** | `cache_key` **sorts** include roots, but lib.rs:130 passes `--include` flags in the ORIGINAL order, and GHC search-path order decides module shadowing. `[A,B]` and `[B,A]` share one key → compiling with `[B,A]` is served the `[A,B]` artifact: wrong module resolution served as a valid hit. | `key_insensitive_to_include_order_serves_other_orders_artifact` / `key_should_be_sensitive_to_include_order` |
| F3a | B5 staleness | **High** (nix realism) | Binary fingerprint is (path, size, mtime) — **content is never hashed**. A same-length binary swap with preserved mtime keeps the key → stale Core served from the old toolchain. Nix store sets ALL mtimes to epoch+1, so for nix-deployed binaries only *size* distinguishes versions. This is the #313 footgun shape surviving the fix. | `staleness_binary_content_swap_same_size_mtime_served_stale` / `key_should_change_when_binary_content_changes` |
| F3b | B5 staleness | Medium | Same gap in `fingerprint_dir` for include-dir `.hs` files: same-size+mtime edits are invisible. | `staleness_hs_content_swap_same_size_mtime_served_stale` / `key_should_change_when_hs_content_changes` |
| F4 | B5 staleness | Medium-High | `fingerprint_dir` uses `DirEntry::metadata()` = **lstat** — a symlinked `.hs` is fingerprinted by the *link's* size/mtime. ANY edit to the real file (new size AND mtime) leaves the key unchanged → stale Core. Bites symlink-farm / nix setups. | `staleness_symlinked_hs_target_edit_served_stale` / `key_should_change_when_symlinked_hs_target_changes` |
| F5 | B5 staleness | Medium (latent) | `extract_exec_target` only accepts a bare token starting with `/`. A **quoted** exec target (`exec "/path" "$@"` — the shellcheck-recommended form), `$HOME`-relative targets, `env`-prefixed lines, or any line containing `=` are silently not followed → delegate-binary upgrades behind such wrappers serve stale Core. The real wrapper is unquoted today — one quoting cleanup away from silent staleness. | `staleness_quoted_wrapper_target_not_fingerprinted` / `key_should_follow_quoted_wrapper_targets` |
| F6 | B1 corrupt-served-as-valid | Medium impact, low likelihood | **No integrity check**: the `.ok` sentinel guards completeness only (its content is ignored — `sentinel_content_is_ignored`). A single bit-flip in a value byte of cached `.cbor` still decodes as a *valid CoreExpr for a different program* and is served as a hit with no recompile. Also applies to `meta.cbor` — flipping the `has_io` bool silently toggles `IOTypeDetected` enforcement. | `corruption_bitflip_served_as_valid_different_program` / `corrupted_payload_should_be_rejected_or_recompiled` |
| F7 | B2 crash | **REFUTED** | Symlink cycle in an include dir (`inc/loop -> inc`) does NOT cause unbounded `fingerprint_dir` recursion: each level adds a symlink component, kernel path resolution fails with ELOOP after ~40 traversals (PATH_MAX bounds physical nesting), and the walker unwinds gracefully. Accidental safety net, pinned as regression guard. | `symlink_cycle_in_include_dir_terminates_gracefully` (green) |
| F8 | Testability | Low | Cache primitives are `pub(crate)` in a private module with no injection point except env vars (`XDG_CACHE_HOME`, `TIDEPOOL_EXTRACT` — both honored, which is what makes this suite possible). Consequence: load-after-store cannot be tested on *arbitrary byte* payloads (lib.rs only stores after successful deserialize), so the identity property runs over random *valid* trees (`arb_core_expr`) instead. | (documented; no test possible) |

## Perturbation sensitivity table

| Perturbation | Key changes? | Intentional? |
|---|---|---|
| Source: any single-char edit, incl. whitespace/comments | YES | Yes — byte-exact by design (blake3 over raw bytes); conservative over-invalidation. `prop_key_stable_and_sensitive`, 48 cases |
| Identical inputs, repeated calls | NO (stable) | Yes. Same prop + every HIT assertion in the suite |
| Target name | YES | Yes |
| Include list: add/remove dir (even empty) | YES | Yes |
| Include list: duplicate a dir (`[A,A]` vs `[A]`) | YES | Accidental but harmless (spurious miss only) |
| Include list: ORDER (`[A,B]` vs `[B,A]`) | **NO** | **BUG F2** — order is semantically meaningful to GHC |
| Non-`.hs` file added/changed in include dir | NO | Yes — GHC only reads `.hs`/`.hs-boot` from the search path |
| `.hs` edit changing size or mtime | YES | Yes |
| `.hs` edit preserving size+mtime | NO | **BUG F3b** |
| Symlinked `.hs`: target edited (any size/mtime) | NO | **BUG F4** |
| Extractor binary: size or mtime change | YES | Yes — the #313-class fix; works |
| Extractor binary: content swap, same size+mtime | NO | **BUG F3a** |
| Delegate behind unquoted-absolute wrapper exec | YES | Yes — matches the real wrapper |
| Delegate behind quoted/`$VAR`/`=`-bearing wrapper exec | NO | **BUG F5** |
| NUL-shifted field boundaries (source↔target, target↔include) | NO (collision) | **BUG F1** |

## Corruption matrix (property group 4)

`{.cbor, .meta.cbor, .ok} × {delete, truncate-0, truncate-half, flip@{0,4,8,last}, garbage}`
— `corruption_matrix_no_panic_no_silent_divergence`: **no panic anywhere**;
every detectable corruption falls through to recompile (lib.rs:101-109 treats
deserialize failure as a miss). Header flips are caught (magic flip → legacy
decode path → structure error; version flip → `UnsupportedVersion`); frame-tag
and root-index flips are caught by `read_cbor` validation. Partial-write /
interrupted-store states (`partial_write_states_are_misses`): the
remove-sentinel-first → persist → persist → sentinel-last protocol makes every
crash prefix a clean miss, and sentinel-present-but-payload-missing states are
read-failure misses. The one escape is F6 (value-byte flips that keep the CBOR
valid).

Load-after-store identity: `prop_load_after_store_identity` (32 random
`arb_core_expr` trees) + `load_after_store_identity_huge_payload` (~1.5 MB
non-UTF8 `LitString`): HIT payloads decode identically to what was stored.

## Design notes (not filed as bugs)

- **Concurrent store interleaving** can in principle leave `.cbor` and
  `.meta.cbor` from different store generations under one valid sentinel
  (per-file atomic rename, no pair-level transaction). Benign today: same key
  ⇒ same compiler output ⇒ identical payloads.
- **`cache_dir` accepts a relative `XDG_CACHE_HOME`** (XDG spec says ignore
  non-absolute paths) — cache would land relative to CWD.
- **Resolver divergence**: `cache_key` resolves the binary via `which::which`,
  execution uses `Command::new` (execvp). Edge cases (PATH changes between
  call sites, non-executable files) can fingerprint one binary and run another.
- **No eviction**: entries accumulate unboundedly (out of scope per spec).

## Fix directions (NOT implemented, per boundary)

- **F1**: length-frame each field (`hasher.update(&len.to_le_bytes())` before
  each component) instead of `\0` separators.
- **F2**: drop the `sort()` — hash include roots in the order they are passed
  to the extractor; order IS an input.
- **F3/F4/F5**: hash binary/file *content* (blake3 is already on hand; an
  (dev,inode,size,mtime)-keyed hash memo keeps it cheap), stat through
  symlinks (`fs::metadata` instead of `DirEntry::metadata`), and/or embed a
  `tidepool-extract --version`/build-hash string in the key instead of parsing
  wrapper scripts.
- **F6**: write blake3(expr_bytes) + blake3(meta_bytes) into the `.ok`
  sentinel (currently empty and ignored) and verify on load.

## Seeds / regressions

Both proptest properties pass, so no `.proptest-regressions` file was
generated. Every confirmed bug is deterministic (collision/staleness
constructions), so the "seeds" are the explicit constructions in the green
repro tests listed above.
