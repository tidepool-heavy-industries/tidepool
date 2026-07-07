# 03 — Engine / runtime / effect / bridge / macro

Availability and diagnostics bugs in tidepool-runtime's session engine, plus
tidepool-macro/bridge/effect issues. Pairs naturally with plan 01 finding 5
(the false-StackOverflow ceiling) — both change what long-running turns do.

## ANTI-PATTERNS

- Do NOT flag locked decisions: freer-simple Leaf/Node continuation trees;
  unboxed Word# union tags.
- The `EffectMachine` fix (F5) must mirror the JIT sibling
  (`tidepool-codegen/src/effect_machine.rs:80-100` `ConTags::try_from`), not
  invent a third resolution scheme — hoist the shared constants instead.

## READ FIRST

- `tidepool-runtime/src/session/engine.rs` — `drive`, `resume`, `abort`,
  Paused registration (:780-830), the runaway-detach comments (:833-838)
- `tidepool-effect/src/dispatch.rs` — HCons peel + its own tag tests
- `tidepool-macro/src/expand.rs` — `run_tidepool_extract`, `resolve_hs_path`

---

## F1 (HIGH): Paused continuations lose the JIT `CancelHandle` — runaway resumed turns pin the engine into permanent Overloaded

**Where (verified by read):**
- `engine.rs:812-824` — Paused registration stores `{session_rx, thread, gate}`
  only; the cancel slot is discarded.
- `engine.rs:709` (`resume`) and `:759` (`abort`) — both pass a FRESH
  `Arc::new(Mutex::new(None))` as `cancel_slot` to `drive`.

**Failure:** turn times out at an effect boundary → parked as Paused → caller
resumes → program enters a PURE loop → window expires → `parked_or_in_effect`
false → `gate.request_abort` unseen (pure compute never checkpoints) →
`cancel_slot.lock()` is `None`, so the JIT cancel flag — which engine.rs's own
comment documents as the mechanism that makes a runaway "EXIT, freeing its
permit instead of pinning it forever" — is never flipped. Detached thread spins
at 100% CPU forever, holds its `OwnedSemaphorePermit` (leaked pool slot), the
reaper blocks in `join()` so `orphaned_threads` never decrements. After
`max_orphaned` occurrences, `start_turn` returns `Overloaded` for every request
until process restart.

**Fix:** add the `Arc<Mutex<Option<CancelHandle>>>` to
`ContinuationState::Paused` and thread it back into `drive()` on resume/abort.

**Verify:** test — start a turn that parks at an effect boundary, resume into a
pure loop, let the window expire; assert the turn terminates (cancel observed)
and the permit is released. Also assert `orphaned_threads` returns to 0.

## F2 (MEDIUM): `UnhandledEffect` diagnostic is dead code, and would name the wrong effect if it fired

**Where:** `engine.rs:1095` (`describe_run_error`) +
`tidepool-effect/src/dispatch.rs:275-289` +
`tidepool-codegen/src/jit_machine.rs:26-27`.

Two independent defects:
1. **Prefix mismatch:** an unhandled effect surfaces as
   `RuntimeError::Jit(JitError::Effect(_))` which Displays as
   `"effect dispatch error: Unhandled effect at tag N"`;
   `detail.strip_prefix("Unhandled effect at tag ")` never matches, so the
   effect-name annotation + "Registered effects" roster are NEVER appended
   (grep "Registered effects" — only the definition).
2. **Wrong tag if fixed naively:** `HCons::dispatch` decrements the tag per
   peeled layer, so `HNil` reports `original − stack_len` (dispatch.rs's own
   tests assert `tag: 0` for an out-of-range tag on a 2-handler stack). A
   version-skewed program emitting tag N over an N-handler stack would be
   annotated `"tag 0 (effect: <first effect>)"` — pointing at a HANDLED
   effect, in exactly the skew scenario the diagnostic exists for.

**Fix:** restore the original tag in the HCons tail branch (`.map_err` bumping
`UnhandledEffect.tag` by 1); match with `contains` — or better, classify
structurally on the eval thread while the typed error is in hand. Add a test
asserting the roster appears with the correct tag.

## F3 (MEDIUM): `run_tidepool_extract` swallows the real GHC error and can emit a false "not found"

**Where:** `tidepool-macro/src/expand.rs:541-546` — the direct PATH
invocation's stderr is discarded on failure (`Ok(_) | Err(_) => { /* fall back
to nix */ }`).

**Failure:** `haskell_inline!`/`haskell_eval!` with a Haskell type error,
extract on PATH, no flake.nix (or no nix): emitted `compile_error!` is
*"tidepool-extract not found on PATH and no flake.nix in any parent
directory"* — both claims false, the actual GHC diagnostic gone. With nix
available, the same error is only reproduced after a redundant slow `nix run`.

**Fix:** fall back to nix only when the spawn fails with
`ErrorKind::NotFound`; when the binary RAN and failed, return its stderr
directly.

## F4 (MEDIUM-LOW): stale `.cbor` bindings served silently after a binding rename

**Where:** `expand.rs:96-107` (`resolve_hs_path` binding lookup); extractor
(`haskell/app/Main.hs:132`) only does `createDirectoryIfMissing` — nothing
removes stale outputs from `target/tidepool-cbor/<Module>/`.

**Failure:** rename binding `foo` → `bar` in the `.hs`: extraction writes
`bar.cbor`, `foo.cbor` survives, and `haskell_eval!("X.hs::foo")` compiles
fine while embedding the OLD Core — silent stale-code execution. The
unsuffixed form instead errors "Multiple bindings found" for a genuinely
single-binding module.

**Fix:** clear the per-module output dir before invoking the extractor (it is
fully regenerated each expansion), or key the dir by source hash.

## F5 (LOW-MED, confirmed stale-shadow smell): interpreter `EffectMachine` resolves freer constructors by ambiguous bare name; JIT sibling already fixed

**Where:** `tidepool-effect/src/machine.rs:22-37` vs
`tidepool-codegen/src/effect_machine.rs:80-100`.

`ConTags::try_from` (JIT) resolves `Val/E/Union/Leaf/Node` via
`get_by_qualified_name("Data.FTCQueue.Node")` etc. precisely because
`get_by_name` returns `None` on ambiguity; `EffectMachine::new` (oracle) still
uses bare `get_by_name`.

**Failure:** any effectful program defining/importing a `Node`/`Leaf`/`Val`
constructor (`data Tree = Node Tree Tree | Leaf Int`) makes the ORACLE fail
`MissingConstructor { name: "Node" }` — misdescribing ambiguity as absence —
while the JIT runs fine, so differential harnesses diverge on exactly the
collision-shaped programs. Oracle/test-only (no production call sites) caps
severity.

**Fix:** mirror qualified-first resolution; hoist the five toolchain-pinned
qualified names into tidepool-effect and have codegen reuse them.
Cross-ref: plan 05 F2 covers the PRODUCTION-path variant of this in
`normalize.rs` — fix together.

## F6 (LOW): `FromCore for ()` accepts any nullary constructor

**Where:** `tidepool-bridge/src/impls.rs:136-143`. `to_value` insists on the
`"()"` constructor specifically (its own comment: a wrong nullary con
"silently corrupts downstream decode"), but `from_value` accepts `Nothing`,
`False`, `[]`, or any user nullary con as `()` — masking upstream encoding
bugs. Related: tuple `FromCore` (:552-570) reports `TypeMismatch "(,)"` when
the real problem is `"(,)"` missing from the table (should be
`UnknownDataConName`).

**Fix:** check the con id against `"()"` mirroring `to_value`; split the
missing-constructor case out of the tuple type-mismatch arm.

## F7 (LOW): derived enum `FromCore` fails fast on EARLIER variants' missing constructors

**Where:** `tidepool-bridge-derive/src/codegen.rs:176-189`. Each variant's
DataCon lookup ends in `?`, so decoding a valid value of variant K errors
`UnknownDataConNameArity` for an earlier variant J whenever J's constructor is
absent from this compilation's table. Decode success depends on Rust variant
order. Currently masked (tables carry whole types) but unenforced.

**Fix:** treat a failed lookup for a non-matching variant as "skip"; error
only if no variant matched.

## Cache residual edges (cache.rs verified CLEAN overall — these are the two bounded leftovers)

- A wrapper script referencing its delegate via a variable/relative path
  (`exec "$DIR/bin"`) isn't followed by the content-hash → stale Core after a
  delegate-only upgrade. (Direct-path wrappers ARE followed.)
- `fingerprint_dir` follows directory symlinks with no cycle guard → a cyclic
  symlink under an include dir hangs key computation.
- Doc drift: `cache.rs:261-262` doc says "sizes, and modification times"; the
  code hashes contents.

## Smaller items

- `resume()`/`abort()` of a Paused continuation re-drive with
  `config.default_timeout_secs`, silently discarding the turn's original
  caller-clamped window — carry `timeout_secs` in the continuation or document
  the reset (fold into F1's struct change).
- `tidepool-effect/src/machine.rs:118` — comment claims "deep_force the
  request so FromCore never sees ThunkRef" above a plain `clone()` (the force
  happened at :95). Reword to state the invariant.
- `strip_module_header` (`expand.rs:471-511`) breaks on multi-line module
  headers (`module Foo\n ( x )\n where` leaks `( x ) where` into the inlined
  body); skip-until-`where` guard closes it.

## Verified clean — do NOT re-audit

`tidepool-runtime/src/cache.rs` — the F1–F6 proptest campaign closed framing,
include-dir ordering, content-vs-mtime, symlinked-`.hs` lstat, quoted wrapper
targets, corrupted-CBOR serving; sentinel-written-last + blake3 means races
degrade to MISS never wrong-serve; key covers source/target/include-dirs/
extract-binary+wrapper hashes/session `(id, gen)` salt. HList dispatch routing
correct for in-range tags. Bridge scalar/container impls round-trip
(proptest-covered); PhantomData symmetric. `json.rs` is a thin delegate to the
single shared `tidepool_eval::json` builder. `PauseGate` state machine sound.
Session lib (`session/mod.rs`): atomic module writes, validation-failure
rollback, cache salt isolation. `tidepool-bignum` chunked pow-2 scaling cannot
double-round.

## DONE CRITERIA

- [ ] F1 fixed with the park→resume→pure-runaway test; permit accounting
      asserted
- [ ] F2 fixed with a roster+correct-tag test
- [ ] F3/F4 fixed (macro error fidelity + stale-cbor cleanup)
- [ ] F5 fixed jointly with plan 05 F2 (shared qualified-name constants)
- [ ] F6/F7 fixed with round-trip asymmetry tests
- [ ] Cache edges + smaller items fixed or filed
- [ ] `scripts/battery.sh` green (runtime crates are GHC-heavy:
      `--ignore-default-filter`)
