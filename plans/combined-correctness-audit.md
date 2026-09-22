# Combined correctness audit

Review baseline: `8290543df..6245dde3b` (159 commits, including the structural
compiler/runtime work, selected shell output, self-hosted lookup, and Codex
recovery integration). Matched Codex review: `fc8e158d58..cb7248d69b`.
Runtime performance changes are outside this audit; verification efficiency
must preserve coverage and process isolation.

## Findings and repairs

| Boundary | Finding | Repair / status |
|---|---|---|
| Suite runner | Separate per-target runners repeat preparation and stop after the first failed target; a second script duplicates process ownership | `91039fd23`: one battery invocation for the same explicit integration targets; metadata failures and unknown packages fail before execution |
| Scaffold inventory | Directory-entry errors were silently dropped; skill filtering assumed a suffix it did not enforce | `d1e34de8d`: fail explicitly on unreadable entries and select exact skill filenames |
| Durable JSONL | Complete final records rejected by schema/version could be truncated as torn writes; a valid row without its final delimiter could merge with the next append | `422fdf356` and `ee6ace796`: reject complete invalid records without mutation; repair only incomplete tails; complete a valid final delimiter before reopening for append |
| Prepared requests | Refusal after payload tenure could leak a registered root; cross-scope park refusal could retain its continuation | `12018593c` and `2395af4e1`: classify before tenure and consume both transferred roots on every park outcome |
| Streamed responses | Two handwritten field counts disagreed with the authenticated schema; every projection repeated the same count policy | `12018593c`: all 99 sites derive arity from the authenticated `DataConTable`; success tests cover the formerly wrong 8- and 23-field constructors |
| Recovery | Missing lifecycle evidence could reopen as fresh; unresolved operation did not fence delayed sibling calls | `dfdc91a2a`: lifecycle v2 creation evidence and reconstructed pending boundaries; matched Codex manifest publishes every enforced transport limit |
| Compiler front door | Retired stable/session injection and compiler prebinding remained in supported libraries after their only consumer retired | `defaaeb69`: removed the dead front doors and cache branch; supported callers use the immutable compilation owner |
| Selected shell | A fixed output reservation followed by final truncation could remove the recovery footer; section rereads accepted a short page | `c25a502d7`: fit selected sections against the complete rendering, bound prompt scalars, and require the recorded page end |
| Host initialization | A first-start failure before root/lifecycle publication cannot prove a resumable later incarnation | Deliberately fail closed and document starting a fresh run. Safe retry needs a separate durable initialization-phase design spanning all ownership artifacts. |

## Verification inventory

The initial nextest inventory contains 2,785 non-ignored tests. The default
tier selects 2,288; its exact complement selects 497. These are execution
obligations, not counts of already-passed tests. The complement was enumerated
and compared by binary identity and test name, not inferred from aggregate
counts. Test additions during repair will update the final counts.

`just verify` runs formatting, all-target strict Clippy, default-tier tests,
suite registration, and the prepared fixture corpus. It does not execute the
497-test complement or the separately selected Haskell test components.

Passed so far: eight runner contracts; 14 extractor-helper tests; 19 changed-test
selection tests; exact registered runtime target listing; eight JSONL tests;
three isolated delegated-resource tests (2.24 seconds); eight selected Haskell
components; compiler/cache/rebind 8/8; extractor transaction 3/3; schema JSON
6/6; actor projection 5/5; compiler-backed response streaming 1/1; park ownership
1/1; GC traversal/construction 6/6. The original matched Codex executable
contract passed; the updated-pin contract is still running.

The initial strict Clippy pass exposed supported-path findings. Owning repairs
removed the retired compiler APIs and fixed the codegen findings; the joined
workspace gate has not yet been rerun.

Fresh Astra review approved the compiler, schema, recovery, Codex manifest,
JSONL, response, GC, and final root-transfer repairs. A selected-shell focused
test compiled but its execution was interrupted at the requested planning
boundary, so it remains unverified.

The exact 498-test default-profile complement is running no-fail-fast. It has
found a repeated prepared-engine constructor-tag mismatch across actor-host
documentation scenarios. That shared failure is the next investigation item;
do not count the complement as passed. Pending after repair: the joined strict
gate, complete fixture corpus, fresh external workspace, updated Codex contract,
and final report.

## Verification efficiency

For a package with N integration targets, the suite runner previously launched
N+1 nextest invocations (one prebuild and N executions). It now launches one
with identical target arguments. Signal, daemon, failure and zero-selection
ownership are inherited from the existing battery wrapper. No concurrency cap
or test isolation was weakened. This is a structural reduction in preparation;
no wall-time speedup has yet been measured.
