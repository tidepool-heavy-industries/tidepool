# The extract manifest — one structured document out of `tidepool-extract`

**Lane:** `manifest-doc` (design only; `plans/` is the whole write surface).
**Consumes:** `plans/post-restart/extract-manifest.md` decisions **D-A**–**D-D**, verbatim, without re-deriving them.
**Implements later:** the wave decomposition in §7 is what a future lane executes.

The extractor talks to Rust through nine channels that accreted one at a
time. This doc inventories every one of them with its named Rust consumer
(§1), specifies the single document that replaces them field by field (§3),
picks a serialization format and argues it (§2), answers codex review **R2.6**
as the worked proof the design earns its keep (§4), states what deliberately
does *not* move (§5), gives the no-flag-day migration story (§6), and
decomposes the implementation into ordered waves with acceptance criteria
(§7).

Every claim about current behaviour below cites `file:line` as of this
branch's tip.

---

## 0. The one-sentence design

> **stdout becomes one JSON manifest that is a strict superset of today's
> diagnostics report; the large CBOR payloads stay files that the manifest
> *names*; every other channel is absorbed as a manifest field.**

Two consequences worth stating up front, because they are what make the
migration cheap:

1. **The manifest is not a new channel.** It *is* channel 1. Every one of the
   eight spawn sites already captures stdout and hands it to
   `diag::parse_diag_report` (`tidepool-runtime/src/diag.rs:55`). No new flag,
   no new file to look for, no new failure mode for "the manifest wasn't
   written" — and critically, stdout is the only channel that survives a
   failure before `createDirectoryIfMissing` ever runs (`haskell/app/Main.hs:276`,
   inside the `try` at `haskell/app/Main.hs:255`).
2. **The split is control plane vs. program plane.** The manifest describes
   *the extraction*: which mode ran, which files were written, which template
   was spliced, what the coordinate transform is, what went wrong.
   `meta.cbor` keeps describing *the program*: the `DataConTable` and the
   IR-level facts a JIT needs (`has_io`, `captured_type`, `var_names`,
   `warnings`, and D-C's new `poisoned`). Drawing that line and holding it is
   worth more than absorbing everything — see §5.

---

## 1. Current channel inventory

Nine channels, exhaustively, with the Rust consumer of each. **An item with no
named consumer is itself a finding** and is flagged `NO CONSUMER`.

### 1.0 The eight spawn sites (who talks to the extractor at all)

| # | Site | Mode | Channels it reads |
|---|---|---|---|
| S1 | `tidepool-runtime/src/lib.rs:185` (`compile_haskell`) | one-shot `--target` | 1, 2, 3 |
| S2 | `tidepool-runtime/src/session/mod.rs:537` (decl validation) | one-shot `--target result` | **1 only** — status + diagnostics; every emitted file is discarded |
| S3 | `tidepool-runtime/src/session/turn.rs:462` (`run_turn`) | `--turn` | 1, 2, 3, 7, 8 |
| S4 | `tidepool-runtime/src/session/turn.rs:811` (`classify_block`) | `--classify` | 1, 6, 8 |
| S5 | `tidepool-runtime/src/session/turn.rs:961` (`compile_session_turn`) | `--session-*` | 1, 2, 3, 4, 5, 8 |
| S6 | `tidepool-harness/src/compile.rs:217` (`compile_turns`) | `--targets` | 1, 2, 3, 4, 8 |
| S7 | `tidepool-macro/src/expand.rs:681` (`haskell_eval!`) | one-shot `--target` | 1, 2, 3 |
| S8 | `haskell/regen-corpus.sh:35` + `haskell/CLAUDE.md:83` | `--all-closed --target-module-only` | **none — not a Rust caller** |

D-A folds S1/S3/S4/S5/S6/S7 into `tidepool-extract-cmd`; S2 is the seventh
spawn site D-A's "zero open-coded `Command::new(extract)` sites" criterion
covers. S8 is a shell caller and stays one.

### Channel 1 — diagnostics JSON on stdout

- **Writer:** `Tidepool.DiagJson.renderDiagsJson` (`haskell/src/Tidepool/DiagJson.hs:102-104`),
  shape `{"version":1,"diagnostics":[{"span":{…}|null,"severity":…,"message":…}]}`.
  Every dispatch arm prints exactly one (`haskell/app/Main.hs:75, 170, 410, 417, 800, 808, 925, 930`).
- **Consumer:** `tidepool_runtime::diag::parse_diag_report`
  (`tidepool-runtime/src/diag.rs:55`), at S1 (`lib.rs:205`), S2
  (`session/mod.rs:580`), S3 (`turn.rs:500`), S4 (`turn.rs:841`), S5
  (`turn.rs:1005`). Rendered by `render_diagnostics`
  (`tidepool-runtime/src/diag.rs:142`) from `tidepool-mcp/src/eval_prep.rs:693`,
  `tidepool-repl/src/session.rs:2331`, `tidepool-runtime/src/session/mod.rs:594`,
  `tidepool-harness/src/harness.rs:399`.
- **Version policy:** exact-match `SUPPORTED_VERSION = 1`
  (`tidepool-runtime/src/diag.rs:48, 71`), checked *before* the strict shape
  parse so a skew reports as a skew (`diag.rs:68-78`).
- S6 (`compile.rs:245-254`) and S7 (`expand.rs:1094`) deliberately do **not**
  use the strict parser — S6 dumps raw stdout+stderr into
  `CompileError::Extract`; S7 is documented as the one site allowed a graceful
  fallback to raw stderr (`expand.rs:1090-1093`).

### Channel 2 — per-binding CBOR trees, percent-encoded filenames

- **Writer:** `encodeTree` (`haskell/src/Tidepool/CborEncode.hs:22`) written at
  `haskell/app/Main.hs:329` (`--all-closed`), `:386` (per-binding), `:613`
  (`writeClosedTargets`, the unified single/multi-target path). Filename from
  `cborFileName` (`haskell/app/Main.hs:467-472`), which percent-encodes `/`
  and `%` because a derived `/=` binder yields `$c/=_u…` and kills the write.
- **Consumers:** S1 `lib.rs:213` (`format!("{}.cbor", target)`), S3
  `turn.rs:555` (`"result.cbor"`), S5 `turn.rs:1012` (`"result.cbor"`), S6
  `compile.rs:271` (`format!("{target}.cbor")`), S7 `expand.rs` via
  `publish_extract_dir` (`expand.rs:1072`) then a directory scan.
- **Latent bug the manifest deletes:** `Main.hs:463-466` says in so many
  words — *"Readers that look files up BY NAME … only ever use caller-chosen
  target names today; if a target containing `/` ever appears there, the Rust
  side must apply this same encoding."* Four Rust sites reconstruct a filename
  the extractor already knows.

### Channel 3 — the merged `meta.cbor`

- **Writer:** `encodeMetadata` (`haskell/src/Tidepool/CborEncode.hs:105-127`):
  `[entries_array, warnings_map]`, strictly 8-element positional entries
  (`CborEncode.hs:144-154`), TPLR header 2.0 (`CborEncode.hs:19`). Written at
  `Main.hs:365`, `:402`, `:621`.
- **Consumer:** `tidepool_repr::serial::read::read_metadata`
  (`tidepool-repr/src/serial/read.rs:113`) → `(DataConTable, MetaWarnings)`
  (`read.rs:78-106`). Called from S1 `lib.rs:227`, S3 `turn.rs:571`, S5
  `turn.rs:1029`, S6 `compile.rs` deserialize block, S7 `expand.rs:500`.
- The warnings map is strict: unknown keys are rejected
  (`CborEncode.hs:111-113`, `read.rs:260-330`), which is exactly why D-D
  requires the 2.0→2.1 bump on both sides in one commit.

### Channel 4 — `asks.json` and `<target>.asks.json`

- **Writer:** `renderAsksJson` (`haskell/app/Main.hs:1108-1110`) over
  `renderAskJson` (`haskell/src/Tidepool/Binders.hs:460-461`), shape
  `[{"site":<u32>,"type":"<rendered>"}]`. **Two shapes, chosen by
  `length targets > 1`**: the flat `asks.json` when single
  (`Main.hs:628-631`), `<outFileBase>.asks.json` per target when multi
  (`Main.hs:615-618`). Rationale at `Main.hs:512-520`.
- **Consumers:** S5 `read_asks_sidecar` (`turn.rs:1043`, `:1059`); S6
  `compile.rs:263-268`, which reimplements the *same* `targets.len() > 1`
  test on the Rust side to pick the shape.
- **FINDING — written but never read on the `--turn` path.**
  `run_turn` reaches `writeClosedTargets` via
  `writeWholeModuleClosed` (`Main.hs:900` → `:722`), so `asks.json` is
  written on every turn; but S3 takes its asks from the `TurnOut` CBOR
  instead (`turn.rs:519, 522, 533, 536`, and the explicit note at
  `turn.rs:549` — *"asks comes from the already-decoded wire variant, not a
  sidecar"*). One fact, two encodings, one of them dead per mode.
- **FINDING — a parallel inference.** The single/multi shape rule is a
  behaviour BOTH sides compute independently from the same input. Neither
  side states it; both derive it.

### Channel 5 — `bound_binders.json` (`--emit-bound-binders`)

- **Writer:** `renderBoundBindersJson` (`Main.hs:1099-1101`) over
  `renderBoundBinderJson` (`Binders.hs:449-456`), shape
  `{"binders":[{"name","varId","module","tier","typeDisplay"}]}`. `varId` is a
  **decimal string**, deliberately — an f64 would lose precision
  (`Binders.hs:447-448`). Written at `Main.hs:1080`.
- **Consumer:** S5 only — `turn.rs:984` passes the flag, `turn.rs:1035-1041`
  reads it through `parse_bound_binders`.
- **FINDING — duplicated shape.** The identical record is *also* carried
  inside the `TurnOut` CBOR (`encodeBoundBinder`, `CborEncode.hs:202-209`) and
  decoded a second time by S3. Two encoders, two decoders, one record.

### Channel 6 — `classify.json` (`--classify-out`)

- **Writer:** `renderVerdictsJson` (`Binders.hs:366-372`), shape
  `{"verdicts":[{"kind":…,"binders":[…]}]}`. Written at `Main.hs:945`.
- **Consumer:** S4 only — `turn.rs:815` passes the flag, `turn.rs:858` reads
  the file, `parse_classify_json` (`turn.rs:864`) parses it and hard-fails on
  a count mismatch.

### Channel 7 — `turn.cbor` (`--turn-out`) and `--json-output`

- **Writer:** `encodeTurnOut` (`CborEncode.hs:164-182`) — a tagged 2-element
  list, `"Decl"`/`"Bind"`/`"Expr"`, deliberately *outside* the TPLR format
  (`CborEncode.hs:156-161`: "no TPLR header, no version coupling"). Written at
  `Main.hs:912-914`. (As of this branch's tip the optional JSON rendering,
  `renderTurnOutJson`, is gone — see the FINDING below.)
- **Consumer of `turn.cbor`:** S3 only — `decode_turn_out` (`turn.rs:511`,
  decoders at `turn.rs:733-782`).
- **FINDING — `--json-output` has NO CONSUMER.** No Rust call site passed the
  flag (grep over the workspace at the time of this finding: the only hits
  were `Main.hs:241`, `:915-919`, `Binders.hs:406`, and
  `plans/one-spawn-turn-protocol.md`). It was a hand-maintained second
  serializer of a wire type, kept in sync by hand, that nothing read.
  `plans/one-spawn-turn-protocol.md:679` already said as much: *"`--json-output`
  proves nothing."* **DELETED** (`deadchannels` lane, 2026-08-10) — see §7.0's
  fate table. `argJsonOutput`, its parse arm, its usage-banner mention, the
  `runTurnMode` write site, `renderTurnOutJson`, and the two helpers that
  turned out to serve nothing else (`renderItem`, `jsonStringList`) are gone;
  `renderBoundBinderJson` and `renderAskJson` stayed — channels 5 and 4 still
  read them. `turn.cbor`/`encodeTurnOut`/`decode_turn_out` are untouched and
  stay live until M3 absorbs them into `manifest.turn`.
- **RELATED FINDING — `--harness-profile` has NO Rust consumer** either
  (`Main.hs:246`, `:69-71`, `:130-139`); nothing in the workspace passes it.
  Not one of the nine channels (it is an input flag that changes what gets
  compiled, not an output the manifest would describe), but §7.0's fate table
  gives it a named wave anyway so it does not linger as an unresolved finding.

### Channel 8 — timing lines on stderr

- **Writer:** `Tidepool.Timing.emitPhase` (`haskell/src/Tidepool/Timing.hs:78+`),
  grammar `tidepool-timing phase=<name> ms=<int>`, gated on `TIDEPOOL_TIMING=1`
  (`Timing.hs:44-45`). Phase vocabulary is frozen in
  `tidepool-harness/src/timing.rs:136-197`.
- **Consumers — TWO hand-rolled forwarders parsing one grammar:**
  1. `tidepool_runtime::session::turn::forward_extract_timing`
     (`turn.rs:405-424`), called at S3 `turn.rs:493` with the literal prefix
     `"extract"` and at S4 `turn.rs:822` with `"classify"`.
  2. `tidepool_harness::timing::ExtractTiming::parse`
     (`tidepool-harness/src/timing.rs:303-323`), used at S6
     `compile.rs:242-243`.
- **FINDING — the lane prefix is caller knowledge, not extractor knowledge.**
  `timing.rs:29-41` is emphatic that `extract.*` and `classify.*` must never
  merge; today the distinction survives only because two call sites pass
  different string literals. The extractor knows its own mode and cannot say
  so.
- **FINDING — the two parsers are byte-for-byte the same algorithm** in two
  crates (`turn.rs:406-423` vs `timing.rs:305-321`).

### Channel 9 — loud human stderr that is also machine-read

| Line | Site | Consumer |
|---|---|---|
| `[extract] POISONED <n> unresolved external(s) … <names>` | `haskell/src/Tidepool/Translate.hs:800-802` | **A TEST ONLY** — `tidepool-runtime/tests/extract_poison_diagnostic.rs:69` asserts `stderr.contains("POISONED")`, `:134` asserts it does not. No production reader. |
| `  SKIPPED (<name>): unresolved external(s): …` | `haskell/app/Main.hs:322` | **NO CONSUMER** |
| `  SKIPPED (<name>): <exception>` | `haskell/app/Main.hs:340` | **NO CONSUMER** |
| `D1 CHECK B … DIAGNOSTIC for binder <n>: <k> DataCon(s) …` | `haskell/app/Main.hs:703-711` | **NO CONSUMER** — explicitly informational (`Main.hs:663-678`), yet the same haddock says a CHECK B hit "is still a REAL FINDING to read and … escalate". Escalation is eyeball-dependent because there is no field to assert on. |
| `  Wrote: <file> (<n> nodes, <b> bytes)` ×8 | `Main.hs:330, 366, 387, 403, 614, 618, 622, 631, 914, 918, 946` | **NO CONSUMER** |
| `  Wrote session iface: <mod> (<name> :: <ty>, <tier>, varId <n>)` | `Main.hs:1064-1065` | **NO CONSUMER** |
| `Processing: <path>` / `Processing (session\|turn): <path>` | `Main.hs:254, 765, 834` | **NO CONSUMER** |
| `  Top-level bindings: <n>` | `Main.hs:267, 783` | **NO CONSUMER** |
| Raw stderr echoed to the operator's terminal | S1 `lib.rs:196-199`, S5 `turn.rs:996-999` | Human only |

**Inventory summary.** Of nine channels: two (2, 3) carry bulk IR and are
correctly files; three (4, 5, 6) are small JSON sidecars each with exactly one
reader; one (7) has a live CBOR half and a dead JSON half; one (8) is a
line-oriented stderr protocol with two duplicate parsers; one (9) is
structured information rendered as prose with, in production, zero readers.

---

## 2. Format: JSON, not CBOR

**Decision: the manifest is JSON. The tree and metadata payloads stay CBOR.**
The stated rule going forward: **CBOR for IR payloads, JSON for the control
plane.** That rule is already ~80% true today; this makes it total and
statable.

Arguments for JSON, strongest first:

1. **The manifest's most important job is to be readable when the reader and
   the writer disagree.** A version-skew message is the highest-value thing it
   emits, and skew is exactly when a strict binary decoder gives you nothing.
   `parse_diag_report` already depends on this: it parses to a loose
   `serde_json::Value`, checks `version`, and only then does the strict shape
   parse (`tidepool-runtime/src/diag.rs:66-79`), with the explicit comment
   *"a future version with a different field shape must still report the
   accurate version-skew message, not a generic 'did not parse' guess."* That
   parse-loose-then-strict move is idiomatic in `serde_json` and awkward on
   `ciborium`'s typed path. A CBOR manifest whose header the reader rejects is
   opaque precisely when a human is debugging a stale deploy.
2. **Additive evolution is free in serde_json and expensive in the CBOR
   discipline.** `DiagReport` carries no `#[serde(deny_unknown_fields)]`
   (`tidepool-runtime/src/diag.rs:39-43`), so an *old* reader silently ignores
   a *new* key — this is the entire no-flag-day story in §6. The CBOR metadata
   reader is deliberately the opposite: it rejects unknown keys
   (`CborEncode.hs:111-113`), which is why every new key there costs a
   two-sided version bump (D-D). Two formats, two evolution policies, matched
   to two different jobs.
3. **Every consumer already has `serde_json` on the hot path**, and the
   extractor already hand-rolls JSON with no `aeson` dependency
   (`DiagJson.hs:3-4`, and the four hand-rolled renderers in `Binders.hs`).
   Absorbing the sidecars into a JSON manifest *reduces* the number of
   hand-rolled JSON renderers from five to one; making it CBOR would grow
   `Codec.CBOR` into surface it does not touch today and would keep all five
   JSON renderers alive as a debug path or delete a human-readable channel
   outright.
4. **There is no numeric-precision hazard to buy CBOR for.** The manifest's one
   u64 field is `varId`, and the existing sidecar already encodes it as a
   decimal *string* for exactly this reason (`Binders.hs:447-448`). Keep that
   convention; it is a solved problem. There are no floats.

Argument for CBOR, honestly stated and rejected: byte-identity pinning. The
repo has a real practice of pinning wire bytes
(`tidepool-repr/tests/golden_wire_contract.rs`), and CBOR's canonical encoding
makes that trivial while JSON key order and whitespace make it fragile. It is
rejected because the manifest is not a fixture: nothing replays it, no corpus
stores it, and its correctness criterion is "the reader gets the right values",
not "the bytes match". Where a byte pin is genuinely wanted (a turn's
`wrappedSource`), the pin belongs on the *field value*, not the document.

Secondary decision: **the manifest is emitted on stdout, not to a
`--manifest <path>` file.** Reasons: (a) stdout is the only channel that
exists on the failure path before the output dir is created
(`Main.hs:255` wraps `Main.hs:276`); (b) all eight spawn sites already capture
and parse stdout, so no call site grows a new argument or a new missing-file
error; (c) it makes the manifest structurally impossible to forget — there is
no flag to omit.

---

## 3. The manifest schema

Written once per invocation, on stdout, on **every** exit path (success and
failure), by every dispatch arm — the invariant `Main.hs:50-54` already states
for the diagnostics report, unchanged in force.

```jsonc
{
  // ---- UNCHANGED legacy keys: an old reader parses this document today ----
  "version": 1,
  "diagnostics": [
    { "span": {"file":…,"startLine":…,"startCol":…,"endLine":…,"endCol":…} | null,
      "severity": "error" | "warning",
      "message": "…" }
  ],

  // ---- NEW: everything else ----
  "manifest": {
    "major": 1,
    "minor": 0,

    "mode": "one-shot" | "all-closed" | "targets" | "session" | "turn" | "classify",
    "status": "ok" | "failed",
    "output_dir": "/abs/path/to/outdir",

    "artifacts": [ /* §3.2 */ ],
    "turn":     { /* §3.3 */ } | null,
    "classify": { /* §3.4 */ } | null,
    "session":  { /* §3.5 */ } | null,
    "phases":   [ /* §3.6 */ ],
    "notes":    [ /* §3.7 */ ]
  }
}
```

### 3.1 Top-level fields

| Field | Type | Absorbs | Why it exists |
|---|---|---|---|
| `version` | `1`, frozen | channel 1 | Kept at `1` **forever** so `parse_diag_report`'s exact-match check (`diag.rs:48,71`) keeps passing on old readers. Evolution moves to `manifest.major/minor`. |
| `diagnostics` | as today | channel 1 | Byte-identical shape (`DiagJson.hs:106-119`). No consumer changes. |
| `manifest.major`/`.minor` | ints | — | The manifest's own version, policy in §6. |
| `manifest.mode` | enum | — (new) | Lets a consumer stop inferring mode from the flags it passed. Directly replaces the literal `"extract"`/`"classify"` prefix strings at `turn.rs:493` and `turn.rs:822` (channel-8 finding). |
| `manifest.status` | enum | the exit code's *meaning* | The extractor says whether it considers this a failure, instead of Rust inferring it from `ExitStatus` + "did stdout parse". Feeds D-A's non-zero-exit classification enum as its default variant's input; D-A's named `classify_block` variant is untouched. |
| `manifest.output_dir` | abs path | — (new) | The dir the extractor actually resolved (`Main.hs:273-275`, `:785-787`, `:845`), which the caller only *usually* knows: it defaults to `takeDirectory path </> takeBaseName path ++ "_cbor"` when `--output-dir` is absent. Makes `artifacts[].path` unambiguous. |

### 3.2 `artifacts[]` — absorbs channels 2, 4 (and the `Wrote:` stderr lines)

```jsonc
{
  "role": "tree" | "metadata" | "session-iface" | "scratch-module",
  "path": "result.cbor",          // relative to output_dir; the ACTUAL name written
  "target": "__result" | null,    // the Core binder this tree is; null for non-tree roles
  "bytes": 40122,
  "nodes": 811,                   // "tree" only, else absent
  "asks": [ {"site": 7, "type": "Text"} ]   // "tree" only; absorbs channel 4
}
```

| Field | Absorbs | Note |
|---|---|---|
| `path` | channel 2's `cborFileName` (`Main.hs:467-472`) | Readers stop *constructing* filenames. Deletes the reconstruction at `lib.rs:213`, `turn.rs:555`, `turn.rs:1012`, `compile.rs:271`, and the latent percent-encoding hazard `Main.hs:463-466` warns about. |
| `target` | — (new) | Makes the `targetName` ≠ `outFileBase` split explicit. Today a session turn compiles `__result` but writes `result.cbor` (`Main.hs:792`, `:900`) and the Rust side knows this only by convention (`turn.rs:965-971`). |
| `bytes`, `nodes` | channel 9's `Wrote:` lines | Already computed at every write site (`Main.hs:330, 366, 387, 403, 614, 622`); today they go to a stream nothing parses. |
| `asks` | channel 4, **both shapes** | Per-tree, so the `targets.len() > 1` shape rule dies on both sides at once (`Main.hs:512-520` and `compile.rs:263-268`). Also ends the write-but-never-read duplication on the `--turn` path. |
| `role: "session-iface"` | channel 9's `Wrote session iface:` line (`Main.hs:1064`) | Lists what `writeSessionIface` (`Main.hs:1062`) put on disk for a *later* invocation's `--inject-val`. |
| `role: "scratch-module"` | — (new) | The spliced/pragma-prepended scratch files (`Main.hs:138`, `:861-862`). Today an undeclared side effect inside the caller's own temp dir. |

### 3.3 `turn` — absorbs channel 7 entirely, and is §4's answer

```jsonc
"turn": {
  "verdict":  { "kind": "bind", "binders": ["x"], "source": "parsed" | "supplied" },
  "template": { "kind": "bind", "path": "/tmp/…/template-1.hs" },
  "spliced":  { "path": "…/Expr.hs", "module": "Expr",
                "user_lines": [33, 40], "line_offset": 32, "col_indent": 0 },
  "out": {
    "kind": "Decl" | "Bind" | "Expr",
    "binders": ["x"],
    "variant": 0,
    "boundBinders": [ {"name","varId","module","tier","typeDisplay"} ],
    "declItems": [ … ],
    "wrappedSource": "module Expr where\n…"
  } | null
}
```

| Field | Absorbs | Note |
|---|---|---|
| `verdict.kind`/`.binders` | the classify substep's result | Today visible only indirectly, via the `TurnOut` tag. |
| `verdict.source` | — (new) | `"supplied"` when `--turn-verdict` short-circuited the re-parse (`Main.hs:838, 844`), `"parsed"` otherwise. Makes `PHASE_CLASSIFY`'s conditional absence (`timing.rs:165-166`) an explicit statement instead of an inference from a missing timing row. |
| `template.kind` | — (new) | **The literal R2.6 fix.** The extractor's own selection (`Main.hs:866-868` for decl, `Main.hs:880-883` for the four-shape split). |
| `spliced.user_lines` / `.line_offset` / `.col_indent` | — (new) | **The full R2.6 fix** — see §4. |
| `out.*` | channel 7 (`turn.cbor` **and** `--json-output`) | Field-for-field the `TurnOut` variants (`Binders.hs:408-424`), keeping `renderTurnOutJson`'s existing key names (`Binders.hs:427-441`) so the shape is already specified and already reviewed. `null` when the turn failed before producing one. |
| `out.boundBinders` | channel 5, on the turn path | Same records as `--emit-bound-binders` (`Binders.hs:449-456`), `varId` still a decimal string. Deletes one of the two encoders. |

`out.wrappedSource` is inlined as a JSON string rather than referenced as a
file. It is a few KB of source text that already round-trips through CBOR
text today (`CborEncode.hs:176`), the consumer wants it as a `String`
(`turn.rs:520, 534`), and no reader wants bytes. It is the one payload where
inlining is right — see §5 for why the trees are not.

### 3.4 `classify` — absorbs channel 6

```jsonc
"classify": { "verdicts": [ {"kind": "bind", "binders": ["x"]} ] }
```

Exactly `renderVerdictsJson`'s shape (`Binders.hs:366-372`), moved inside.
Deletes `--classify-out`, the file write (`Main.hs:945`), and the read-back
(`turn.rs:858`). `parse_classify_json`'s count-mismatch hard-fail
(`turn.rs:864`) is preserved verbatim — it is load-bearing (a silent length
mismatch misaligns every item against the wrong verdict) and is *not* something
the manifest makes redundant.

### 3.5 `session` — absorbs channel 5

```jsonc
"session": {
  "generation": 12,
  "module": "Tidepool.Session.Val.G12",
  "root": "/…/session-root",
  "bound": [ {"name","varId","module","tier","typeDisplay"} ]
}
```

`bound` is `renderBoundBindersJson`'s array (`Main.hs:1099-1101`), moved
inside. Deletes `--emit-bound-binders`, the write (`Main.hs:1080`), and the
read (`turn.rs:1035-1041`). `generation`/`module` are new: today Rust passes
`--bind-gen` and reconstructs the module name from `SessionModule`
(`tidepool-repr/src/session_ids.rs`) while Haskell constructs it independently
via `sessionModuleString` — the two are documented as having to stay
byte-identical by hand (`tidepool-repr/CLAUDE.md:60-66`). The manifest does not
*fix* that (there is still no shared formatter), but it makes a divergence
detectable at the boundary instead of at the next reference turn.

### 3.6 `phases[]` — absorbs channel 8

```jsonc
"phases": [ {"name": "typecheck", "ms": 310}, {"name": "total", "ms": 2100} ]
```

- Names stay the frozen `PHASE_*` vocabulary (`timing.rs:136-197`). **The
  manifest changes the transport, not the vocabulary** — the
  `ghc_session`/`classify_extract` tombstone discipline
  (`timing.rs:138-146`, `Timing.hs:27-31`) is untouched, and a new phase still
  gets a new name rather than repurposing an old one.
- The `extract.` / `classify.` stage prefix is derived by the consumer from
  `manifest.mode`, not from a call-site string literal — closing the
  channel-8 finding. `timing.rs:29-41`'s rule ("a collector must never fold
  any `extract.<phase>` into the like-named `classify.<phase>`") becomes
  enforced by construction.
- Deletes both hand-rolled forwarders (`turn.rs:405-424`,
  `timing.rs:303-323`).
- **Preserved invariant, restated:** `phases` is `[]` unless
  `TIDEPOOL_TIMING=1`. `timing.rs:53-56` states that the wire contract "must be
  byte-identical with and without the env var set"; with timing inside the
  manifest that invariant must be restated as **"every emitted file and every
  manifest field other than `phases` is byte-identical with and without
  `TIDEPOOL_TIMING`."** A future wave that makes `phases` unconditional breaks
  a stated contract — say so in the code comment, not just here.
- The human-readable `tidepool-timing phase=… ms=…` stderr lines **stay**, as a
  debug aid for a hung or killed extract where no manifest is ever emitted.
  They keep their grammar; nothing parses them.

### 3.7 `notes[]` — absorbs channel 9

```jsonc
{
  "kind": "poisoned-external" | "skipped-binder" | "meta-coverage-gap" | "unresolved-external",
  "severity": "warning" | "error",
  "target": "someBinder" | null,
  "names": ["Dep.helper"],
  "slots": [3],
  "message": "human-readable text, identical to today's stderr line"
}
```

| `kind` | Absorbs | Consumer today |
|---|---|---|
| `poisoned-external` | `Translate.hs:800-802` | A test string-matching `"POISONED"` (`extract_poison_diagnostic.rs:69, 134`). `slots` carries D-C's per-module slot ids so a note and `meta.cbor`'s `poisoned` map join on the same key. |
| `skipped-binder` | `Main.hs:322`, `Main.hs:340` | none |
| `meta-coverage-gap` | D1 CHECK B, `Main.hs:703-711` | none — and `Main.hs:674-678` asks a human to read and escalate these. A field makes that mechanizable. |
| `unresolved-external` | `translateTargetClosed`'s hard error text (`Main.hs:447-450`) | none directly; today it arrives as a `diagFromException` message blob. A note gives it structure alongside the diagnostic. |

`notes` are **additive to**, never a replacement for, `diagnostics`. A failure
still produces its `diagnostics` entry exactly as today; a note is the
*structured* companion. This keeps every existing renderer working unchanged.

Every note keeps its human stderr line. The manifest adds a machine-readable
copy; it does not silence the operator's terminal.

---

## 4. R2.6, answered — template-kind attribution

This is the design's proof. Restating the finding
(`plans/post-restart/codex-review-2026-08-08.md`, item 19, R2.6):

> *"error-coordinate attribution prefers Expr on overlapping windows — a Bind
> error can get the Expr offset/excerpt. … overlap is the COMMON case, not a
> corner … `run_turn` hands ALL templates to ONE extract invocation and the
> EXTRACTOR selects which applies, so only the extractor knows the failing
> candidate — the fix is the diagnostics JSON … carrying the template kind …
> consumed by harness `pick_render_opts` in place of window-containment."*

### 4.1 What today's code actually does

`Harness::render_compile_error` (`tidepool-harness/src/harness.rs:388`) calls
`pick_render_opts` (`harness.rs:422-452`), which:

1. finds one *representative* diagnostic — the first whose span file ends with
   `"Expr.hs"` (`harness.rs:428-433`);
2. recomputes each candidate's user-code window itself, by
   `candidate_window(source, marker, content_lines)` (`harness.rs:354-362`),
   which locates a **hardcoded literal fragment of the template's own text** —
   `EXPR_MARKER = "__user = let {\n __b =\n"` (`harness.rs:338`),
   `BIND_MARKER = "__result = do {\n"` (`harness.rs:346`) — and counts
   newlines before it;
3. tries EXPR first, then BIND, and returns the first whose window *contains*
   the representative line (`harness.rs:435-450`).

Its own doc comment concedes the guess (`harness.rs:376-387`): *"a compile
FAILURE carries no verdict tag: `run_turn` returns before ever decoding which
template applied."* And R2.6's sharpening is right: the expr and bind windows
are the same block at offsets differing by a few preamble lines, so any
multi-line block overlaps and EXPR wins by ordering. Misattribution shifts
every reported line by the offset delta — in the text a model reads to fix its
own Haskell.

Note the deeper problem, which R2.6 names only implicitly: **the harness is
reverse-engineering a splice it did not perform, by string-matching a
fragment of a template it also authored.** Two independent encodings of one
template's internal structure, kept in sync by nothing.

### 4.2 The minimum fix — `manifest.turn.template.kind`

The extractor selects the template *before* it compiles: classify at
`Main.hs:844`, select at `Main.hs:866-868` (decl) / `Main.hs:880-883` (the
four-shape `bind`/`binddiscard`/`expr` split), splice at `Main.hs:869`/`:884`,
and only then `runPipelineSession` at `Main.hs:889`. The GHC failure lands in
the `try` opened at `Main.hs:835`, whose handler (`Main.hs:920-930`) today
prints diagnostics and nothing else.

So the extractor already holds the answer at the moment it reports the
failure. The manifest's `turn.template.kind` and `turn.verdict` are populated
**incrementally**, before the compile, and emitted on the failure path too.

`pick_render_opts` then becomes a lookup: read `turn.template.kind`, select
that candidate's `RenderOpts`. Steps 1 and 3 above — representative-diagnostic
selection and window containment — are deleted outright.

### 4.3 The full fix — the extractor states the coordinate transform

`spliceTemplate` (`Main.hs:981-989`) is the function that performs the
substitution, and `placeTurnStmt` (`Main.hs:1000-1010`) is the one that decides
whether a `let` turn is rewritten with explicit braces (which changes the line
count). The extractor therefore knows, exactly and without inference:

- the line at which the turn text begins in the spliced module,
- how many lines it occupies after placement,
- the module name and the scratch path it wrote (`Main.hs:861-863`).

Those are precisely `RenderOpts`' `user_lines`, `line_offset`, `col_indent`,
and `anchor` (`tidepool-runtime/src/diag.rs:98-133`). Publishing them as
`manifest.turn.spliced` additionally deletes:

- `candidate_window` (`harness.rs:354-362`),
- `EXPR_MARKER` / `BIND_MARKER` (`harness.rs:338, 346`) — the duplicated
  template fragments,
- `engine::content_line_count` (`tidepool-harness/src/engine.rs:1233`) at this
  call site, along with the convention test that pins it to
  `template_haskell_impl` (`engine.rs:1666`).

`pick_render_opts` reduces to:

```rust
let t = manifest.turn.as_ref()?;
let s = t.spliced.as_ref()?;
Some(RenderOpts {
    anchor: &s.path,
    label: TURN_LABEL,
    user_lines: Some((s.user_lines[0], s.user_lines[1])),
    line_offset: s.line_offset,
    col_indent: s.col_indent,
    drop_foreign_gen_warnings_except: None,
    source: source_for(t.template.kind),
})
```

**Recommendation: ship the full fix (§4.3).** §4.2 alone satisfies R2.6's
literal text and is a legitimate smaller step, but it leaves the harness still
recomputing the offset from a hardcoded template fragment — the same class of
parallel inference this lane exists to remove, just with one fewer branch. §4.3
is not more work in the extractor (the numbers are in scope at the splice); it
is less work in Rust.

The same `spliced` block serves the eval and repl paths, which build their own
`RenderOpts` from `extract_user_code_lines`' `-- [user-lines] <start>:<end>`
source marker (`tidepool-runtime/src/diag.rs:446-458`) — a marker the *Rust*
preamble generator emits and the *Rust* reader greps back out. Migrating those
to the manifest is deliberately **out of scope** for the first waves: the
marker is self-consistent (one language writes and reads it) and does not
suffer R2.6's cross-boundary guess. Noted here so a later lane can pick it up
knowingly rather than discovering the overlap.

---

## 5. What does NOT move, and why

### 5.1 CBOR trees stay files

Rejected alternative: base64 the trees into `artifacts[].content`.

- **Size.** A whole-module closed extraction of a real eval is hundreds of KB
  to MBs of CBOR. Base64 is a flat +33%, and the Haskell side would escape it
  through `jstr` (`DiagJson.hs:122-132`), a per-character `String` `concatMap`.
  Over megabytes that is not a constant factor; it is the dominant cost of the
  extraction.
- **Copy count on the hot path.** Today: `fs::read` → `ciborium` over the raw
  bytes. Inlined: parse JSON → decode base64 → decode CBOR, two extra full
  materializations per compile, in exactly the span `STAGE_CBOR_READ` /
  `STAGE_CBOR_DESERIALIZE` measures (`timing.rs:109-111`).
- **The cache stores tree bytes as files** (`tidepool-runtime/src/cache.rs:344,
  386`); inlining would require re-splitting them back out on the store path.
- **The byte pin is over files.** `tidepool-repr/tests/golden_wire_contract.rs`
  pins tree and metadata bytes over the committed corpus; that pin is only
  meaningful on a file.

The honest counter-argument — "one document, one read, no missing-file case",
which would delete `CompileError::MissingOutput` (`lib.rs:65`, raised at
`turn.rs:557-562`, `compile.rs:270`) — is answered halfway by the manifest
anyway: once the reader stops *constructing* the filename, the only remaining
missing-file case is a genuine I/O fault, which deserves its own error rather
than being folded into a parse failure.

### 5.2 `meta.cbor` stays a file, and keeps its warnings map

Same size argument, plus: one `meta.cbor` is shared across N trees
(`Main.hs:604, 620-622`), so inlining it means either duplication or an
indirection the file already is.

More importantly, the warnings map's contents (`has_io`, `captured_type`,
`var_names`, `warnings`, and D-C's `poisoned`) **stay there and are not
mirrored into the manifest.** This is the control-plane / program-plane line
from §0:

- They are facts about the *program*, consumed by the JIT alongside the
  `DataConTable` — `register_var_names` (`lib.rs:229`, `turn.rs:574`,
  `turn.rs:1031`) feeds `var_names` straight into
  `tidepool_codegen::host_fns`. `read_metadata` returns
  `(DataConTable, MetaWarnings)` as one unit at five call sites; splitting
  them creates a second read and a second failure mode for no gain.
- D-D locks the 2.0→2.1 minor bump adding `poisoned` to that map, landing in
  the `sentinels` lane **this wave**. Relocating its home in the next wave
  would churn what just landed.
- Duplicating them into the manifest would create dual sourcing — two places
  that must agree, which is the disease, not the cure.

The one apparent overlap is deliberate and is **not** duplication:
`meta.cbor`'s `poisoned` is a `slot → qualified-name` table for runtime error
*naming*; `manifest.notes[kind="poisoned-external"]` is an extraction-time
*diagnostic* for a human or a test. Different consumers, different lifetimes,
joined on `slot`. Say this in the code comment at both sites, or a future
reader will "unify" them.

### 5.3 Session interface files stay files

`writeSessionIface` (`Main.hs:1062`) writes GHC `.hi` blobs consumed by a
*later* extract invocation through `--inject-val` (`Main.hs:766-769`,
`Main.hs:885-888`). Rust never opens them. The manifest lists them
(`role: "session-iface"`) so a caller can assert they were produced; it does
not carry them.

### 5.4 The human stderr stream stays

Every `Processing:`, `Wrote:`, `SKIPPED`, `POISONED`, `D1 CHECK B`, and
`tidepool-timing` line keeps being written. The manifest adds structure; it
does not take away the thing you read when the process died before emitting
anything. What changes is that **nothing in Rust parses stderr any more** —
that is the actual deliverable of channels 8 and 9.

---

## 6. Migration — no flag day

### 6.1 Versioning policy

Two version numbers, two policies, matched to two evolution styles:

| | `version` (top level) | `manifest.major` / `.minor` |
|---|---|---|
| Value | frozen at `1`, forever | starts at `1.0` |
| Policy | exact match (`diag.rs:48, 71`) | mirror of the TPLR rule: reject a `major` mismatch, reject a **newer** `minor`, accept an **older** `minor` within the same major |
| Bumped when | never | `minor` on any additive key; `major` on removing or retyping a field |

The `minor` rule is deliberately the same sentence as
`tidepool-repr/CLAUDE.md:78-81` (*"a `major` mismatch, or a `minor` newer than
this build supports; an older `minor` within the same `major` is accepted"*),
so the repo has **one** versioning idiom, not two. Rejection is loud and names
`scripts/redeploy.sh`, matching every existing skew message
(`diag.rs:59-64, 72-76`; `turn.rs:850-855`).

### 6.2 Stale reader, new extract

An old Rust build reading a manifest-emitting extract sees
`{"version":1,"diagnostics":[…],"manifest":{…}}`. `DiagReport` carries **no**
`#[serde(deny_unknown_fields)]` (`tidepool-runtime/src/diag.rs:39-43`), so
`serde_json::from_value` ignores `manifest` entirely. The version check passes
(still `1`). **It keeps working, unchanged, on every path.**

This is the whole no-flag-day property, and it is true *only* because
`version` stays `1` and the change is purely additive. A wave that "cleans up"
by renaming `version` or adding `deny_unknown_fields` destroys it. Pin this
with a test (§7, M1).

### 6.3 Stale extract, new reader

The `manifest` key is absent. **Policy: a hard, named version-skew error. No
fallback to the old channels.**

Justification is not novel — it is the policy already written at
`turn.rs:820-845`, in the comment explaining why `--classify` skew must not
degrade:

> *"both shapes are `MalformedDiagnostics` (→ VersionSkew) … This is what
> makes the one-format wire policy true here: … a new runtime REQUIRES a
> matching extract, and `scripts/redeploy.sh` ships both together."*

Keeping a dual-read fallback would mean maintaining every absorbed channel's
reader for the lifetime of the fallback — i.e. not deleting anything, i.e.
the lane produces no benefit.

**One carve-out, already carved.** `tidepool-macro/src/expand.rs` documents
itself as *"the ONE call site in the workspace allowed that graceful
fallback — a dev-convenience macro-expansion tool talking to whatever
`tidepool-extract` happens to be on a user's PATH, potentially a much older
build"* (`expand.rs:1090-1093`). It keeps its fallback, unchanged.

### 6.4 Interaction with `scripts/redeploy.sh`

`redeploy.sh` already ships both sides together: `nix profile upgrade
tidepool-extract` (step 2, `scripts/redeploy.sh:49-60`) plus the Rust servers,
plus `rm -rf ~/.cache/tidepool/`. Its preflight already hard-fails on
untracked `haskell/` files, because the flake excludes them
(`redeploy.sh:31-39`) — the exact failure mode that would otherwise ship a new
reader against an old extract.

Two things a manifest wave must add:

1. The post-upgrade probe (`redeploy.sh:59+`) checks the no-args `Usage:`
   banner. Extend it to assert the banner path also emits a manifest —
   `Main.hs:75` prints `renderDiagsJson []` with no output dir, so it is the
   cheapest possible round-trip of the version handshake, and it catches a
   half-deployed pair before the first eval rather than at it.
2. The extract binary's fingerprint is part of the compilation cache key
   (`Main.hs:100-107` documents this; `cache.rs`'s `cache_key_salted`). A
   manifest change changes the fingerprint, so all cached CBOR misses once on
   redeploy — normal, and `redeploy.sh` clears the cache anyway.

**Cache caveat to write down.** The cache stores `.cbor` / `.meta.cbor` only
(`cache.rs:344, 386`) and the hit path returns `CompileResult { expr, table,
warnings }` with no manifest (`lib.rs:160-169`). That is safe **today** only
because the cached path is `compile_haskell`, which consumes no asks, no turn
data, and no notes; the turn and session paths are uncached by design
("turns are one-shot, no cache needed", `tidepool-harness/CLAUDE.md`). A
future wave that caches a manifest-dependent path must cache the manifest with
it. Stated here so it is a decision, not a surprise.

### 6.5 Interaction with the CBOR corpora — none, by construction

The manifest carries no `CoreExpr` and no `DataCon` bytes. It therefore cannot
move a byte in `haskell/test/suite_cbor`, `haskell/test/corpus_cbor`, or
`tidepool-repr/tests/golden_wire_contract.rs`'s corpus. The **STOP CONDITION**
at `plans/post-restart/extract-manifest.md:84-90` (fixture regeneration is a
sequenced root-level action, never a lane's call) is satisfied by construction,
not by care — and a wave whose diff touches those paths has gone wrong and
should stop.

D-D's separate 2.0→2.1 `meta.cbor` bump is orthogonal: the manifest never reads
the warnings map, so the two land in either order.

---

## 7. Implementation decomposition

Ordered. Each wave is independently landable and independently valuable; each
has one acceptance criterion. **All of these assume `extractcmd`, `writepaths`,
and `sentinels` have folded** — M2 in particular edits the write paths
`writepaths` owns, and every Rust-side change lands through
`tidepool-extract-cmd` (D-A) rather than at open-coded spawn sites.

### 7.0 The nine-channel fate table — no survival without a consumer

**Invariant, stated explicitly:** every one of the nine channels inventoried
in §1 has exactly one fate — ABSORBED into the manifest in a named wave, or
DELETED in a named wave. A channel may not survive past the final wave (M7)
without a named production consumer; a channel whose disposition is still
open blocks M7's closure rather than being left as a standing "no consumer"
finding for the next sweep to rediscover.

| # | Channel | Fate | Wave |
|---|---|---|---|
| 1 | Diagnostics JSON on stdout | **ABSORBED** — this channel does not move; it *is* the manifest's carrier. `manifest` is added beside the unchanged `diagnostics` key (§0, §3.1). | M1 |
| 2 | Per-binding CBOR trees, percent-encoded filenames | **ABSORBED** — tree bytes stay files forever (§5.1: size, cache, byte-pin arguments). Their name/byte-count/node-count/target/asks are named as `artifacts[]` entries (`role: "tree"`), deleting every Rust-side filename reconstruction. | M2 |
| 3 | Merged `meta.cbor` | **ABSORBED** (listing only) — the `DataConTable`/warnings-map bytes stay a file forever, by the deliberate control-plane/program-plane split (§5.2); the manifest never mirrors that content. Its existence/path/byte-count is named as an `artifacts[]` entry (`role: "metadata"`). | M2 |
| 4 | `asks.json` / `<target>.asks.json` | **ABSORBED** into `artifacts[].asks`, per-tree — ends both the write-but-never-read duplication on the `--turn` path and the parallel `targets.len() > 1` shape inference duplicated in both languages. | M2 |
| 5 | `bound_binders.json` (`--emit-bound-binders`) | **ABSORBED** into `session.bound`. | M2 |
| 6 | `classify.json` (`--classify-out`) | **ABSORBED** into `classify.verdicts`; the count-mismatch hard-fail is preserved verbatim (§3.4). | M2 |
| 7 | `turn.cbor` (`--turn-out`) + `--json-output` | **SPLIT fate.** `--json-output`: **DELETED** — done (`deadchannels` lane, this commit; see the amended Channel 7 finding in §1). `turn.cbor`/`encodeTurnOut`/`decode_turn_out`: **ABSORBED** into `manifest.turn.verdict`/`.template`/`.out`, then the CBOR encoder/decoder pair is deleted outright (M3's acceptance criterion). | done / M3 |
| 8 | Timing lines on stderr | **ABSORBED** into `phases[]`; both hand-rolled forwarders (`forward_extract_timing`, `ExtractTiming::parse`) are deleted. The stderr lines themselves are not deleted (§5.4) — they stay as the debug aid for a hung or killed extract that never emits a manifest. | M5 |
| 9 | Loud human stderr that is also machine-read (POISONED, SKIPPED ×2, D1 CHECK B, `Wrote:`, `Processing:`, `Top-level bindings:`) | **ABSORBED** into `notes[]` (`poisoned-external` / `skipped-binder` / `meta-coverage-gap` / `unresolved-external`). Every stderr line stays (§5.4); the manifest adds a structured companion, it does not silence the terminal. | M6 |

Three items root named explicitly, so their disposition is recorded here
rather than left to be re-derived:

- **`--json-output`** — channel 7's JSON half. DELETED, done, this lane
  (piece 1 of this commit set). Not scheduled — already landed. The CBOR half
  of channel 7 is untouched and stays live until M3.
- **`--harness-profile`, the `--all-closed` SKIPPED lines, and the `Wrote:`
  lines** — root's instruction was explicit: no piecemeal deletion now, each
  gets a named wave instead of standing as an open finding.
  - The two SKIPPED lines (`Main.hs:322`, `:340`) are channel 9 rows —
    **ABSORBED** as `notes[kind="skipped-binder"]`, wave **M6**, same as every
    other channel-9 line.
  - The `Wrote:` lines are channel 2/4's rows — their informational content
    (path/bytes/nodes) is **ABSORBED** into `artifacts[]`, wave **M2** (§3.2's
    heading already says this: *"artifacts\[\] — absorbs channels 2, 4 (and
    the `Wrote:` stderr lines)"*). The stderr text itself is unaffected.
  - **`--harness-profile`** is not one of the nine channels — it is an input
    flag that changes what source gets compiled, not an output the manifest
    describes — but it carries the same "no consumer" finding, so it gets a
    disposition rather than staying an open note. M7's old text punted this
    ("a different lane's call"); that punt is replaced below: **DELETED,
    folded into M7**, the wave that already retires every other flag this
    finding turned up dead.

**D1 CHECK B — a silent-negative bug, not merely a dead channel.** Per root's
framing, this is "a REAL FINDING to escalate" with no assertable field today
— the same family as codex items 16 and 20. Its field is
`notes[kind="meta-coverage-gap"]`, landing in **M6** (§3.7). M6 is wave 6 of
7 — far out enough that root's rule applies: *"an interim machine-readable
line is acceptable — but the doc must SAY WHICH it is choosing."* **Choosing
the interim:** the existing stderr line already carries a stable, greppable
prefix — `"D1 CHECK B (independent reachable-Core subset) DIAGNOSTIC for
binder <name>: …"` (`Main.hs:730`, unchanged by this lane) — and that prefix
is the sanctioned interim signal, grep-anchored on the literal string
`"D1 CHECK B"`, until `notes[kind="meta-coverage-gap"]` lands in M6. This is a
stated choice, not an implied one: no new code ships to make it more
machine-readable before M6; the existing prefix is judged sufficient as a
bridge.

**`[extract] POISONED` — KEEPS, and gains a field in the same wave as the
rest of channel 9.** Operator-facing and test-pinned
(`extract_poison_diagnostic.rs:69, 134`), so the stderr line is never a
deletion candidate. It gains `notes[kind="poisoned-external"]` in **M6**,
the same wave every other channel-9 line absorbs into — `slots` carries
D-C's per-target slot ids so a note and `meta.cbor`'s `poisoned` map join on
the same key (§3.7's table already specifies this).

**BOUND follow-up, not yet assigned a wave number: hash-derived poison
slots.** D-E (`plans/post-restart/extract-manifest.md`) found that poison
slots are per-target counters while `--targets` shares one merged
`meta.cbor`, so two targets can legitimately assign the SAME slot to
DIFFERENT externals; `Main.mergePoisonedTables` drops a non-unanimous slot
rather than guessing. Deriving the slot from a hash of the qualified name
instead of a counter fixes this at the source (same symbol ⇒ same slot
across every target) and deletes `mergePoisonedTables` entirely. This costs
a wire re-bump (D-D's minor-bump discipline), so it is BOUND, explicitly, to
ride in the SAME fixture-regen cycle as whichever wave next changes
`meta.cbor`'s wire bytes. No wave in M1-M7 above touches `meta.cbor`'s
content (§5.2 keeps it deliberately untouched throughout this manifest
project), so this follow-up has no wave number yet — it is a standing BOUND
constraint on the next lane that does open `meta.cbor`'s wire format for any
reason: that lane must land the hash-slot fix in the same commit/fixture-regen
cycle, not as a trailing follow-up PR. Fixture regeneration is a
root-sequenced action (D-D's STOP CONDITION), never a lane's own call to
make — the lane that eventually does this still stops and asks root to
sequence the regen, same as every other fixture-touching change.

### M1 — manifest skeleton + version handshake

Emit `manifest` with `major/minor/mode/status/output_dir` and an empty
`artifacts`/`notes` from every dispatch arm, on both the success and failure
paths. Rust: parse it in `tidepool-runtime::diag` alongside `DiagReport`;
absent ⇒ named skew error (§6.3) at every site except `expand.rs`.

> **Acceptance:** a Rust build *predating* this change parses a manifest-emitting
> extract's stdout with zero behaviour change (a checked-in golden stdout
> fixture, fed to the current `parse_diag_report`, still yields the same
> `DiagReport`); and a manifest-less stdout produces an error naming
> `scripts/redeploy.sh`, not a fallback.

### M2 — absorb the small JSON sidecars (channels 4, 5, 6)

Populate `artifacts[]` (with `path`/`target`/`bytes`/`nodes`/`asks`),
`session.bound`, `classify.verdicts`. Switch S4/S5/S6 to read the manifest.
Keep writing the sidecar files this wave (dual-write, single-read).

> **Acceptance:** `read_asks_sidecar` (`turn.rs:1059`), `parse_bound_binders`,
> and the file read at `turn.rs:858` have no callers; the `targets.len() > 1`
> shape test exists in **neither** language (`compile.rs:263-268` and
> `Main.hs:515` both deleted); the multi-target acceptance test
> (`tidepool-harness/tests/acceptance_multi_target.rs:124`) passes unchanged
> against per-artifact `asks`.

### M3 — absorb the turn result (channel 7)

Populate `turn.verdict` / `turn.template` / `turn.out`. S3 reads them instead
of `turn.cbor`.

> **Acceptance:** `decode_turn_out` and the CBOR decoders at `turn.rs:733-782`
> are deleted; `encodeTurnOut` (`CborEncode.hs:164-215`) is deleted; the
> `TurnOut` round-trip tests at `turn.rs:1569, 1598` pass against the manifest
> path with the same expected values.

### M4 — R2.6: template attribution and the coordinate transform

Populate `turn.spliced` (`path`, `module`, `user_lines`, `line_offset`,
`col_indent`) on the failure path as well as the success path. Rewrite
`pick_render_opts` (`harness.rs:422-452`) as the lookup in §4.3.

> **Acceptance:** a `bind`-verdict turn whose GHC error falls inside the
> *overlapping* expr/bind window region renders with BIND coordinates. Pin it
> as a regression test — today's `pick_render_opts` returns EXPR for that
> input, so the test must be red before the change and green after, on the same
> synthetic sources the existing tests at `harness.rs:3195-3268` build.
> `EXPR_MARKER`, `BIND_MARKER`, and `candidate_window` are deleted.

### M5 — absorb timing (channel 8)

Populate `phases[]` when `TIDEPOOL_TIMING=1`. Consumers derive the
`extract.` / `classify.` prefix from `manifest.mode`.

> **Acceptance:** `forward_extract_timing` (`turn.rs:405-424`) and
> `ExtractTiming::parse` (`timing.rs:303-323`) are both deleted; the existing
> parser tests (`timing.rs:357-390`) are rewritten against manifest input and
> keep asserting the same phase names; a new test asserts every manifest field
> other than `phases`, and every emitted file, is byte-identical with and
> without `TIDEPOOL_TIMING` set (§3.6's restated invariant).

### M6 — absorb the loud stderr notes (channel 9)

Populate `notes[]` for `poisoned-external`, `skipped-binder`,
`meta-coverage-gap`, `unresolved-external`. Keep every stderr line.

> **Acceptance:** `tidepool-runtime/tests/extract_poison_diagnostic.rs:69, 134`
> assert on `notes[kind="poisoned-external"].names` containing `Dep.helper`
> instead of `stderr.contains("POISONED")`, and still pass under the same
> `TIDEPOOL_TEST_FORCE_VALIDATION_ONLY` fault injection; a new test asserts a
> D1 CHECK B gap surfaces as a `meta-coverage-gap` note (it has no assertable
> form today).

### M7 — retire the absorbed channels

`--json-output` is already gone (§7.0; done in the `deadchannels` lane ahead
of this wave). This wave deletes what M2-M6 leave dual-written: `--classify-out`,
`--emit-bound-binders`, `--turn-out`, `renderBoundBindersJson`,
`renderVerdictsJson`, `renderAsksJson`, and the `asks.json` /
`<target>.asks.json` writes. It also deletes **`--harness-profile`**
(§7.0 — folded in here rather than left an open finding: nothing in the
workspace passes it, same "no consumer" shape as every other flag this wave
retires) and `spliceHarnessProfilePragma`. Update the usage banner
(`Main.hs:74`). Extend `redeploy.sh`'s post-upgrade probe per §6.4.

> **Acceptance:** the property, not the count — no file is written by the
> extractor that no consumer reads, asserted as a test that runs each mode into
> a temp dir and checks every produced file against the manifest's
> `artifacts[].path` set. `--harness-profile` is removed outright this wave —
> confirm zero references remain (flag parse, usage banner, the splice
> function) the same way `--json-output`'s removal was confirmed (§7.0).

---

## 8. Open questions for the implementing lane

1. **`--all-closed` has no Rust consumer** (`regen-corpus.sh:35` and by-hand
   regeneration are its only callers, `haskell/CLAUDE.md:83`). Its
   skip-on-failure loop (`Main.hs:315-345`) is the one path with no
   `assertMetaCoversEmitted` CHECK A (`Main.hs:573-574`). Emitting
   `skipped-binder` notes there is cheap and makes a fixture sweep's silent
   omissions visible in `regen-corpus.sh`'s log. Worth doing in M6; not worth
   blocking on.
2. **Should `manifest.status` subsume the exit code?** No — keep both. The exit
   code is what a shell caller (`regen-corpus.sh`) and a spawn-failure path see
   when no stdout was produced at all. `status` is the extractor's *opinion*;
   the exit code is the process's *fact*. D-A's classification enum consumes
   both.
3. **The eval/repl `-- [user-lines]` marker** (`diag.rs:446-458`) is a second
   coordinate-transport mechanism, Rust-internal. §4.3 deliberately leaves it
   alone. If a later lane unifies it onto `turn.spliced`, note that the marker
   is written by `tidepool_mcp::eval_prep::template_haskell_impl` and read back
   by grep in the same language — it has no cross-boundary guess to fix, so the
   unification buys consistency, not correctness.
