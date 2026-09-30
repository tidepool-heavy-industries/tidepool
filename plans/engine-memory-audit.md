# Engine and runtime memory audit

Read-only review at Tidepool source `9630c91d166fb344fc9d4fcd677f0e0fd8bcc81b`. No source changes, builds, daemon activity, or live runtime inspection were performed. The retained compiler-failure artifact is an on-disk measurement; Rust allocation peaks below are source-derived hypotheses, not profiler results.

## Compiler/artifact evidence

The retained directory
`/tmp/tidepool-final-facade-recovery/bridge/facade/target/tidepool-test-runs/compiler-failures/.tmpVO53c1`
contains 174 regular files totaling **49,297,386 bytes**. Its largest files are:

| Artifact | Bytes |
|---|---:|
| `module-products.cbor` | 39,867,990 |
| `certified-products.cbor` | 4,365,306 |
| `module-candidates.cbor` | 1,247,604 |
| `__prepared.prepared.cbor` | 741,207 |
| consumed sources | 748,958 |
| `dependencies.json` | 426,540 |
| `meta.cbor` | 140,357 |

The product sidecar decodes to 53 modules, 12,408 independently encoded projected groups, 3,034,719 interface bytes, and 36,794,296 group bytes. Hashing each complete group payload and interface found no exact duplicates. The only identical files in the retained directory are `Expr.hs` and `consumed-sources/4.hs`, 3,196 bytes each; retaining both appears to preserve original versus consumed-source evidence. The product file and certificate are distinct formats: the worker's `Tidepool.ExecutionEncode.encodeModuleProducts` writes exact interfaces and group definitions, while `Tidepool.CertifiedProducts.encodeCertifiedProducts` writes ownership witnesses. Rust needs both to cross-check before product admission. No removal is justified by this artifact alone.

A small read-only definite-length CBOR walk found substantial repeated text inside those group payloads: 2,239,836 text occurrences, 591,884 distinct strings counted per group, and 1,948,178 repeated occurrences. The current encoded text leaves occupy about 23,513,958 bytes. A hypothetical per-group string table with compact integer references would occupy about 10,267,959 bytes for string entries and references before table framing, or a theoretical saving of about 13.2 MB. This is an encoding lower-bound estimate, not a valid format proposal or a measured build/RSS saving. Common repeated atoms include `type`, `constructor`, `main`, `Tidepool.Aeson.Value`, and package/module names. A format change must distinguish schema tags and identity components from source-defined names and preserve the existing validation contract.

The more immediate allocation candidate is repeated decoding in one Rust owning workflow:

- `artifacts::seal_turn_outputs` reads the byte sidecar and parses it into `fresh_products` (`tidepool/toolchain/src/artifacts.rs:786-794`), then passes both the same bytes and parsed vector to certification (`:842-855`).
- `certified_products::certify_products` parses those same bytes again and compares the new graph to `fresh_products` (`tidepool/toolchain/src/certified_products.rs:1489-1495`).
- `parse_module_products` first decodes the full document into an owned `ciborium::Value`, then copies interface bytes and strings into `RawModuleProduct` while the decoded document still owns its byte strings (`tidepool/repr/src/execution_schema.rs:1321-1358, 1382-1411`). Each projected group is then independently decoded and validated (`:1384-1407`).
- `split_module_product_bytes` performs another full `Value` decode of the same sidecar, then reconstructs and serializes a one-module sidecar for every row (`tidepool/toolchain/src/module_candidates.rs:37-79`). The certified path retains this set in `fresh_sidecars` (`certified_products.rs:1497-1504`) because candidates must remain module-independent when unrelated modules change.

For the measured 39.87 MB input, this proves several passes over the full encoded input and one full generic `Value` tree coexisting with typed products during each parse. It does **not** establish allocator peak or that the typed graph duplicates all 39.87 MB: group payloads are parsed into `ProjectedGroup` structures and nested CBOR values are short-lived. A candidate improvement is one validated parse that returns typed products plus exact source byte ranges for each row, followed by canonical per-module framing from those ranges. That can avoid the second full parse and its transient `Value` tree while preserving the independent per-module product boundary. First determine whether canonical-byte rejection is required: the current split re-encodes each row, so it may also normalize accepted CBOR encodings. A row-range implementation must preserve all input bounds, exact byte/hash identities, semantic validation, and transaction isolation. String-table encoding is a separate schema/performance decision and needs measured parser and retained-object costs before adoption.

## Runtime/actor retention evidence

`exomonad/actor/src/command_jobs.rs` inserts each job into a shared `HashMap` at `start` (`:469-472`); the checked-in file has no entry-removal path. The entry owns `Arc<Shared>`, whose backend and terminal phase are retained. `CommandConnection::drop` only decrements the observer count (`:311-337`), and the status snapshot walks every retained entry (`:475-490`). The facade host backend stores `Arc<HostCommand>` in `running` (`bridge/facade/src/actor_host/commands.rs:371-409, 509`) and never clears that slot. Each `HostCommand` owns stdout and stderr ring buffers, each capped at 4 MiB (`exomonad/node/src/host_command.rs:95-99, 122-145`). Therefore N completed host jobs can retain up to approximately **8 MiB × N** of output bytes, plus deque capacity slack and job metadata. This is a per-job source-derived upper bound, not a count or byte measurement from a live run.

The retention serves a stated API contract: a `Job` is a handle to existing work, and late status/output reads must not rerun it (`bridge/haskell/lib/Tidepool/Command.hs:254, 676`; `exomonad/prompts/base.md:116`). No forget/expiry API was found. Eviction would therefore change behavior. Resolve an explicit release boundary or an output-budget policy that returns a typed expired/unavailable result before changing the owner. Keep active observers, unsettled cleanup, command settlement, and output-page cursors valid through their required lifetime.

## Scope and confidence

The compiler byte counts and CBOR occurrence counts were measured directly from the named retained artifact. Runtime buffer capacity and registry lifetime were established from source; live command counts, actual retained output sizes, Rust peak RSS, and expected cache hit impact remain unmeasured. No source was modified in this audit.
