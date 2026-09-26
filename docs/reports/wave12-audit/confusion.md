# Wave 12 confusion audit

Run: `7d0bd907-1640-49b1-a952-f98005ca6e2d`  
Cutoff: `2026-09-26T02:33:13.588641Z`  
Scope: root thread `01a0db77-022a-7723-bd7d-9edf39931ea5` and all six bound child threads (actors 2, 3, 5, 6, 7, 8), through the cutoff. Evidence checked: structured host trace, detailed host log, each `binding.json`, and matching native rollout histories. The wave12 friction/interview notes and shared contract were used as context and checked against trace evidence. The native histories remain private; references below point to local artifacts and identify concise events only.

The cutoff is an observation point in an ongoing run, not a verdict. All seven bound histories were accessible through cutoff. The root and server owner had useful recovered work by cutoff; browser acceptance still had repeated failing runs without a resolved cause, and the repaired UI candidate was awaiting exact-revision review. No broad count is given for ordinary successful source-reading/tool actions. The 10 long root cells in `wave12-slow-calls.md` are context for environment/runtime friction, not model inference or confusion findings.

## Findings

### C1 — A mistyped command session ID caused an unknown-job response

- **Kind / actor / operation:** failure; root; command-job cancellation and inspection.
- **Evidence:** root native history `rollout-2026-09-25T19-06-44-01a0db77-022a-7723-bd7d-9edf39931ea5.jsonl`, ordinals 49, 53, and 56. The start receipt at `02:07:59.479Z` displayed `job7 :: Cmd.Job` and session ID `77551aff-1df9-4f4c-bbe3-d37904eec5d9`. The `cancel_command` call at `02:08:03.028Z` passed `77551aff-1df9-4f4c-bbe3-d37904eecdaed`; the reply at `02:08:03.197Z` was `CommandUnavailable "unknown command job"`. The argument's suffix differs from the issued ID.
- **Observed:** root copied the session ID incorrectly into the native cancellation call. The service rejected that exact unknown key; the correct job's cancellation was not attempted in this call.
- **Cause:** an input error at the string handle boundary. `CommandJobs::shared` looks up the exact ID before ownership checks; this evidence does not indicate registry loss or a service lifecycle race.
- **Confidence / impact:** high / medium; command cancellation remained unconfirmed, with no demonstrated product damage.
- **Owner / action:** command tool surface and guidance. Keep the typed `Cmd.Job` binding when using resident Haskell; if using native `session_id`, copy the issued ID exactly and report a mismatched ID as such. **Validation:** cancel a retained command with its exact ID, then inspect the same job; separately confirm an altered ID returns unknown without affecting the original job.
- **Pipeline / related:** `environment`; no related finding.

### C2 — Server owner asked a valid Engine-seam question, then ended the request as Blocked

- **Kind / actor / operation:** opportunity; actor 2 (`01a0db79-6d65-7872-8b6c-78ed8d80d221`); inspect/implement deterministic `--serve` behavior.
- **Evidence:** child rollout at `2026-09-26T02:11:08.543122Z` says the current path required `CodexFileAuth` and remote Engine, then marked the work Blocked and asked whether deterministic commands could bypass Engine. Root’s actual assignment (root rollout, `02:07:27.972730Z`, `NEXT.md` output) explicitly required deterministic behavior without model credentials. The shared contract `docs/wave12-contract.md` requires server-owned interpretation and durable outcomes. Root corrected the assignment at `02:19:53.773964Z` to use `Engine::with_transport` and a local deterministic transport. Actor 2 then delivered `70f3989` with five focused tests passing at `02:19:39.620853Z` (delivery report precedes root’s correction notice in the assembled history); later reviewer/root evidence found the first candidate still had fabricated child/progress behavior, and actor 2 submitted the corrected `bcedd45` with 7/7 focused tests passing at `02:32:50.109470Z`.
- **Observed:** asking the parent whether the credential-free path could remain Engine-backed was appropriate implementation clarification. The request ended as `Blocked` while the parent could still supply the seam and the owner could continue after that answer. Root resolved it with the local transport constructor. The first candidate’s passing unit tests did not prove the shared end-to-end contract, so root withheld integration and requested repair.
- **Hypothesis:** the model-facing brief named the product constraint but did not expose a known concrete recipe for an offline Engine transport at the server seam. The explicit local transport example/path may have made the permitted implementation less obvious. This is an inference; source exploration itself was necessary and is not counted as friction.
- **Confidence / impact:** high for the Blocked and subsequent recovery; medium for prompt cause / medium impact. The question itself is not counted as a failure.
- **Owner / action:** prompt/API. Add a short compiled or source-pinned example of constructing the existing Engine with a deterministic local transport, and say that credential-free deterministic execution remains Engine-backed where the acceptance contract requires it. **Validation:** at a fresh baseline, implement one server command with the example and verify that it uses Engine without reading remote credentials.
- **Pipeline / related:** `prompt`, `api`; related C3.

### C3 — Acceptance owner asked about a real dependency, then ended the test request as Blocked

- **Kind / actor / operation:** opportunity; actor 5 (`01a0db7a-5aac-7f32-972b-e19f01b6ba93`); black-box HTTP/WS acceptance journey.
- **Evidence:** acceptance rollout `2026-09-26T02:12:29.378Z` reports Blocked because baseline `7abb219` still routed to `CliDriver::ask` and lacked deterministic wait/cancel, message, child, and failure semantics. Root’s initial task had allowed independent acceptance-test work before server completion (`02:19:53.773964Z`, root status), but the child’s observation that the behavior was absent was correct. Root landed HTTP/WS test dependencies and contract amendment at `6e12252`; actor 5 received the revised task at `02:25:10.193Z`. Between `02:28:49Z` and `02:31:44Z`, the browser journey test repeatedly exited 101. By cutoff its cause and a passing recovery were not established in the readable evidence.
- **Observed:** the owner correctly asked about missing production behavior. Ending the request as `Blocked` delayed an expected-red test slice that could still be authored against the contract. The test owner resumed after the server/manifest baseline changed. Repeated red runs are observable, but the sampled output does not establish their common cause; they may be legitimate product feedback rather than confused retries.
- **Hypothesis:** assignment did not distinguish writing the black-box test against a known upcoming contract from requiring the feature to exist first. A source-pinned contract and a test harness plan could have let the owner write/run against an expected-red baseline while server work proceeded. This is an inference; waiting for real production wiring was understandable.
- **Confidence / impact:** high for the Blocked/resumption sequence; low for causal prompt gap / medium impact.
- **Owner / action:** prompt. State that test owners should build the contract-level black-box fixture while the implementation is in progress, report expected-red behavior, and keep failures distinct from dependency absence. **Validation:** assign the same split from a baseline without the feature; acceptance owner produces the real HTTP/WS test and a bounded baseline result before the implementation lands.
- **Pipeline / related:** `prompt`, `experiment`; related C2.

### C4 — Initial UI candidate failed typecheck and overstated server-observed state

- **Kind / actor / operation:** failure followed by successful recovery; actors 3 and 6; UI implementation and exact-source review.
- **Evidence:** UI owner 3 (`01a0db79-aa4a-7833-bf2d-8545ae6fc118`) reported candidate `f2a040aaef44b10f62757ff69c406c1462427d0d`, `npm test` 11/11 passing but `npm run check` and build failing with `string | undefined` passed to a required `string` callback (`02:14:08.059Z`). Reviewer 6's exact candidate finding `02:14:38.892924Z` additionally rejected unsupported activity/outcome claims. Owner delivered repair `ef5e6128c6577d4ec1a80c4b440dbaad89003db0` with tests, check, build, and diff check passing (`02:31:35.649Z`); at cutoff root said this exact revision was still under review (`02:32:49.271770Z`).
- **Observed:** a green unit-test suite was insufficient because typecheck/build failed; review also caught UI claims that the wire data could not support. The owner repaired both categories instead of relabeling inferred states as facts.
- **Hypothesis:** no shared UI command/outcome types were available yet because the server wire extension was still being built. That made unsupported status language easy to introduce, while normal static checks caught the callback type mismatch.
- **Confidence / impact:** high / medium; prevented a broken/overclaiming UI candidate from being integrated.
- **Owner / action:** API/prompt. Keep the server-owned event/request schema as the sole source for displayed states and include the actual wire fields in the UI assignment. **Validation:** exact candidate must pass typecheck/build and tests, and a review against snapshots with absent additive fields must confirm no inferred read/acted/reconnect claims.
- **Pipeline / related:** `api`, `prompt`; related C5.

### C5 — Server’s first green focused candidate failed the actual child/progress contract

- **Kind / actor / operation:** failure followed by successful recovery; root and actor 2; candidate review/integration boundary.
- **Evidence:** root report `02:31:41.149230Z` says the earlier server candidate had 5/5 focused tests passing but bypassed Engine and fabricated child interaction; root rejected it and returned the work for repair. Shared contract `docs/wave12-contract.md` states child must be a distinct conversation row and progress must be a durable envelope; it also defines the Engine/server/event/persistence acceptance boundary. Actor 2 later reported `bcedd45` using `Engine::with_transport`, with 7 matched/executed/passed focused tests and formatting/diff checks (`02:32:50.109470Z`). The candidate was submitted just before cutoff; independent review and browser journey success were not complete at cutoff.
- **Observed:** focused helper tests did not exercise the product distinction that mattered: fabricated/static child output was not a real parent-child interaction. Root used the contract and withheld integration. Later tests cover real Engine path at the unit seam, but end-to-end proof remains open.
- **Hypothesis:** candidate test scope did not encode the child identity/event provenance invariant strongly enough, even though the shared contract did. A contract-derived acceptance case for distinct child row, ordered parent message/reply, and durable progress should catch this before review.
- **Confidence / impact:** high / high; this was a material product correctness issue caught before integration.
- **Owner / action:** experiment/API. Promote those shared-contract clauses into the actual black-box acceptance test, retaining unit tests for local behavior. **Validation:** HTTP/WS journey asserts child row ID/path, real message/reply order, durable progress envelope, and refresh snapshot preservation; verify that removing Engine or substituting a child-looking envelope makes it fail.
- **Pipeline / related:** `experiment`, `api`; related C2, C4.

### C6 — Root recovered quickly from a host-cancelled first acceptance task

- **Kind / actor / operation:** success; root; delegation recovery.
- **Evidence:** root native history says original acceptance request 3 became unavailable with `TargetCancelled("host selected completion abort for shutdown")` (`02:09:53.272336Z` and `02:11:14.682169Z`). Root replaced it; status at `02:14:32.543659Z` shows replacement request 4 active. Root then decoupled black-box test work from implementation at `02:19:53.773964Z` rather than treating the first cancelled reply as a product result.
- **Observed:** the first acceptance child was host-cancelled, not rejected for its work. Root distinguished unavailable from ready and continued with a replacement. The event traces show quick workflow recovery; no claim is made about the full acceptance outcome.
- **Hypothesis:** none required; host shutdown was an environmental lifecycle event, not model confusion.
- **Confidence / impact:** high / low; one lost task and a replacement.
- **Owner / action:** environment/runtime. Keep typed unavailable/cancelled request state visible in status and avoid treating it as a candidate. **Validation:** host-cancel one child and verify it appears unavailable while replacement delivery remains independently trackable.
- **Pipeline / related:** `environment`; no related finding.

## Unknowns

- The repeated browser journey exit-101 runs between `02:28:49Z` and `02:31:44Z` were visible as failures, but this audit did not establish from the sampled output whether each was the same defect, a setup/build issue, or successive expected product findings. No causal attribution is made.
- At cutoff, `bcedd45` was submitted with a focused 7/7 server check, but there was no completed independent exact-candidate review or successful black-box browser journey in the sampled evidence.
- No actionable wrong Haskell symbol/type name or parser diagnostic was established from the private native histories in this sample. Ordinary source and API reading was not counted as a confusion episode.
- The 402 Jev billing failures in the detailed host log affected after-tool judgments and were explicitly abstained; these are environmental service failures, not model-facing type/API mistakes. They were not observed to block the useful actions summarized above.
