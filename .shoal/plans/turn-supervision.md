# Hot-reloadable turn supervision

## Goal and first acceptance boundary

Parent-authored Haskell policies use Jev to review completed model turns,
supply useful context, nudge a worker, or escalate evidence to its parent.
Policy reload changes subsequent invocations without restarting the actor.

First deliver an observational vertical slice: a text-only completed turn
invokes `afterTurn`; hot-reload the policy; the next completed turn invokes
the new revision. Do not enable automatic self-nudges in this slice.
Implement and test on a separate updated host, preserving the live session.

## Existing baseline

`AgentSpec.afterTool` and dispatcher entry 1 already support hot reload.
The local `.shoal/AgentSpec.hs` installs child-only `Project.Watchdog` rules.
The watchdog batches trigger and evidence-support judgments, explicitly marks
unavailable history, and gates advice on both judgments. The support floor
of 0.8 is an experimental advisory policy, not a calibrated guarantee.

Live evidence: installation 3 successfully swapped; a direct synthetic
`watchWith [guessingInsteadOfReading]` invocation advised reading first.
Three notebook fixtures distinguished prior reading, explicit guessing, and
missing history. These are smoke experiments, not regression coverage.
No child escalation, deduplication, turn-end hook, or automatic wake was tested.
Local watchdog changes have not been promoted to the shipped example.

## Ownership and implementation sequence

1. **Provider completion source — `tidepool-agent`.**
   Inspect `interactive.rs`, Codex `rollout_conversation.rs`, and
   `active_update.rs`. Extend the existing provider-neutral interactive seam;
   keep Codex record parsing in its backend. Reuse the existing projection
   rather than inventing a second conversation parser.
   Distinguish provider/model completion from typed request settlement.
2. **Resident slot — `haskell/lib/Tidepool/Agent/Contract.hs`, `tidepool-actor`.**
   Add a defaulted `afterTurn` field and distinct stable dispatcher entry.
   Admit completed-turn events through the actor's serialized resident
   machinery. Capture the spec revision at admission; an accepted invocation
   keeps it across reload. Preserve after-tool behavior.
3. **Connection lifecycle — `tidepool/src/actor_host.rs`.**
   Attach completion observation to exact interactive binding and retire it
   with the connection. Do not deliver passive hook invocations through the
   provider inbox: that would itself wake the model.
4. **Authored supervision — workspace Haskell.**
   Supply assignment provenance, completed turn, bounded history with explicit
   availability, actor identity, and evidence references. Keep named policy
   composition in Haskell. Do not silently upgrade running descendants.

Before splitting implementation, settle the event and slot types, startup
cursor semantics, error behavior, and integration owner. Suggested dogfood:
one Sol Medium implementation worker for the vertical slice, followed by
fresh-context Sol review; use additional workers only for independent work
after the shared contract is committed. Root owns integration and live proof.

## Required invariants

- Define which completions are eligible at binding; do not guess from polling
  timing. Deduplicate exact thread/turn identities. Specify restart behavior
  explicitly rather than promising durable exactly-once side effects.
- Partial JSONL records remain pending; aborted turns do not complete.
  Rotation, truncation, unavailable evidence, and watcher failure are visible.
- Serialize with cells/tools without holding a machine checkout while waiting
  on external inference. Shutdown cancels and joins the observer.
- Failed or timed-out policies cannot wedge retirement or user input.
- Before enabling nudges: tag provenance, cap consecutive self-wakes, and
  deduplicate notifications in exact code. Jev confidence is not a loop guard.
- Escalations carry exact child identity and recoverable triggering evidence.
  Treat commands already executed as observations, not preventable actions.

## Focused acceptance checks

- Backend: both supported boundary spellings; foreign/inherited thread and
  aborted-turn exclusion; torn tail; explicit initial cursor; duplicate events;
  truncation/rotation and unavailable source.
- Resident: absent slot; effectful invocation; failure/timeout; serialization;
  accepted invocation versus reload revision; stop cleanup.
- Host: exact binding; one passive invocation per eligible completion; visible
  observer failure; observer cancellation and join.
- Separate live host: text-only completion fires; tool-using completion fires
  after-tool per eligible tool and after-turn once; reload changes the next
  turn's policy. No automatic self-wake in this acceptance slice.

Use owning crates' focused `just` checks, compile changed consumers, and
record what ran versus merely compiled. Do not run `just verify` routinely.
