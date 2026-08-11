# extract-manifest lane — the extract↔Rust channel constellation

Approved direction (Inanna, 2026-08-10): "unify these channels". One
structured manifest out of the extractor, one `ExtractCmd` builder into it.

Bug history this lane inherits: `codex-review-2026-08-08.md` items 18-20.

## Scaffold decisions (root of this lane, made before forking)

These are the cross-lane choices the children must NOT re-derive.

### D-A. `ExtractCmd` lives in a new std-only leaf crate, not `tidepool-runtime`

`tidepool-macro` is a proc-macro crate whose only dependencies are
`syn`/`quote`/`proc-macro2`/`serde_json`. It has NO dependency on
`tidepool-runtime` and must not grow one — that would drag Cranelift and the
whole runtime graph into every downstream crate's *build* graph. But
`expand.rs:668` is one of the seven spawn sites and the done-criterion is
"zero open-coded `Command::new(extract)` sites".

So: a new workspace crate **`tidepool-extract-cmd`**, `std`-only (no
workspace deps at all). It owns:

- extract-binary resolution, with `expand.rs`'s STRICT policy as the default
  (`$TIDEPOOL_EXTRACT` set-but-unreadable is a hard error, never a silent
  fall-through to `PATH`; unset falls back to bare `tidepool-extract`),
- argument construction for every mode (`--target`/`--targets`/`--turn`/
  `--classify`/`--session-*`/`--include`/`--output-dir`/…),
- the spawn itself + the process-global spawn counter (moved here from
  `tidepool-harness/src/compile.rs`, which re-exports it so its existing
  public surface is unchanged),
- an explicit **non-zero-exit classification policy enum** — the default is
  "parse the stdout diagnostics report; unparseable ⇒ version skew", and
  `classify_block`'s deliberate exception ("this lane has no user-error mode,
  every non-zero exit is infra/stale-extract") is a NAMED variant, not a
  copy-pasted comment.

It deliberately does NOT parse diagnostics or CBOR (that needs `serde_json`
/`ciborium` and belongs upstream): it returns the raw `std::process::Output`
plus the classification verdict, and each caller maps that onto its own error
type (`CompileError`, `SessionError`, `String`).

`expand.rs`'s nix-run fallback stays a property of that call site — it is
expressed as a fallback hook on the builder, not duplicated arg construction.

### D-B. The spawn counter is wrong by construction today — fix is structural

`tidepool-harness/src/compile.rs`'s `EXTRACT_SPAWNS` is the extract-wave
plan's done-criterion, but the harness ALSO reaches `tidepool-runtime`'s
`run_turn` (`session/turn.rs`), which spawns extract invisibly to the counter.
Moving the counter INTO `ExtractCmd`'s spawn path makes every spawn counted by
construction. The acceptance test must assert the property, not the number: no
`Command::new` naming the extract binary survives outside the builder.

### D-C. Sentinel payload encoding — slot in the middle bits, kind stays low

Today an unresolved external emits the SHARED node `0x45 << 56 | 4`, erasing
which symbol it replaced. New layout, chosen so kinds 0/2/3 are byte-identical
and readers that mask the low byte keep working:

    0x45 << 56  |  (slot << 8)  |  kind

- `kind` stays in the LOW byte (0 = generic, 2 = error, 3 = undefined,
  4 = unresolved-external poison). Every existing reader that compares the
  whole word for kinds 0/2/3 keeps matching, because those emit `slot = 0`.
- `slot` is a per-module monotonic counter (the same shape the runLLMTurn
  site ids already use), 48 bits, assigned per distinct poisoned external.
  `slot = 0` means "no slot recorded" (legacy / non-poison sentinels).

`meta.cbor`'s warnings map gains an OPTIONAL `poisoned` key:
`[[slot, qualified-name], …]`, omitted entirely when empty.

### D-D. The wire bump is a MINOR bump and must NOT require fixture regen

`tidepool-repr/CLAUDE.md`: the reader rejects a NEWER minor but accepts an
OLDER minor within the same major. `CborEncode.hs` notes the reader "rejects
unknown keys, so new keys require a version bump on both sides."

Therefore: bump `2.0` → `2.1` on BOTH sides in one commit, teach the reader
the optional `poisoned` key (absent ⇒ empty, which is exactly what every
committed 2.0 fixture says). Committed `haskell/test/{suite_cbor,corpus_cbor}`
fixtures stay valid and are NOT regenerated.

**STOP CONDITION.** If anything forces regeneration of
`haskell/test/{suite_cbor,corpus_cbor}` — that is a SEQUENCED ROOT-LEVEL
ACTION (codex review item 16, box-wide rule), never a lane's call. Land
everything up to that point, then stop and report. `tidepool-repr`'s own
`tests/golden_wire_contract.rs` corpus is a different, in-crate artifact; if
the minor bump moves its bytes, updating it in-crate is in scope — say so
explicitly in the receipts.

## Wave decomposition

Wave 1 (parallel, disjoint files):

| Lane | Owns | Notes |
|---|---|---|
| `warmups` | `haskell/src/Tidepool/{Binders,DiagJson}.hs`, `haskell/app/Main.hs` | 3 byte-identical consolidations, 3 commits |
| `extractcmd` | new `tidepool-extract-cmd/`, `tidepool-runtime/`, `tidepool-harness/src/compile.rs`, `tidepool-macro/src/expand.rs` | D-A + D-B; zero Haskell |
| `sentinels` | `haskell/src/Tidepool/Translate.hs`, `tidepool-repr/`, `tidepool-codegen/`, `tidepool-eval/` | D-C + D-D |
| `manifest-doc` | `plans/` only | design the end-state schema |

Wave 2 (after `warmups` folds — same file):

| Lane | Owns | Notes |
|---|---|---|
| `writepaths` | `haskell/app/Main.hs` | unify the three CBOR+meta write paths |
