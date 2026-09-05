# Astra discovery campaign and UX interview

Observed 2026-09-05 in `/home/inanna/dev/shoal-console`.

## Outcome

A medium-effort Astra root chose a useful product question, forked two
low-effort Astra children, reviewed and revised a candidate through retained
actors, interviewed a child, and stopped both children while preserving their
worktrees and history. The root remains retained for follow-up.

The strongest UX result came after the campaign: the root initially proposed a
new selective-inspection API, then successfully replaced most of that imagined
API with ordinary resident Haskell on its first attempt. This supports improving
discoverability and rendering before expanding the permanent DSL.

## Run and evidence

- tmux: `shoal-console-astra-discovery`; root pane `%433`.
- Run: `0ff35cf6-f3b0-44b5-ba12-2d855bee265a`.
- Root: `01a0707e-6bff-7a51-bd6e-f4a8425c8220` (Astra medium).
- Research child: `01a0707f-402c-79a3-8ea3-971724545850` (Astra low).
- Coding child: `01a0707f-40fb-7471-a601-ec0a30680c2b` (Astra low).
- Packaged Codex: `118e1cfcd1d7dd120460ff0685e976f0d17327dc`, launched
  through `/home/inanna/.cache/tidepool/codex-context-cache-affinity/bin/codex`.
- Prompt: project-local `.shoal/astra-discovery-prompt.md`.
- Provider rollouts: `/home/inanna/.codex/sessions/2026/09/05/`, filenames
  containing the thread UUIDs above. Native Haskell custom calls were visible
  in the live transcript.

The prompt invited a bounded, self-chosen discovery task, task-specific types
and helpers, and a small scaffold → unfold → fold wave. It permitted a candidate
in an isolated worktree but prohibited merging into shared main. This was guided
exploration, not an unprompted demonstration of orchestration preference.

## Product work

The root asked whether pinned-A/live-B waveform comparison makes the cause of
divergence understandable: parameter changes versus elapsed signal time.
It authored `ComparisonTask` and `Finding`, then sent typed inputs to an
inspection-only researcher and a coding child. Its watch was:

```haskell
joined <- watch joinLabel $
  (,) <$> awaitSettledFork (fst workers)
      <*> awaitSettledFork (snd workers)
```

The initial candidate explained a pause/pin/adjust workflow. A retained reviewer
caught the toggle-state ambiguity and identified a possible short-terminal
clipping problem. The coding child revised the text and added a rendered
72×14 test. The root inspected the exact candidate diff and preserved it without
merging.

Candidate: `155e7ddf3cbb4053615f403338e6fe4eb89205a4`, branch
`shoal/comparison-meaning/inspect-explain/branches/explanation`.
Changes are confined to `src/ui.rs`: state-aware comparison guidance, compact
help below 18 rows, and a focused rendering test. Child command output reported
`cargo test help_`: five passed, and formatting passed. The default build
directory policy worked without overrides. The observer reviewed the diff but
did not independently rerun those tests or certify all terminal sizes.

The source-derived clipping risk was not reproduced against the old candidate.
The new test demonstrates visibility after the revision. The user-owned
untracked `docs/TIDEPOOL_HASKELL_EXERCISE.md` remains untouched. Dirty source
snapshots appear in the worktree receipts; the candidate is not a clean-main
integration receipt.

## Interview findings

The root would keep the applicative watch above. It said full context inheritance
helped follow-up substantially, while its initial implementation/review
decomposition was conventional. It reused actors, typed responses, watches,
`Finding`, and later `ReviewInput`; it still repeated restrictions and wrote
substantial prose in follow-up requests.

The coding child liked inherited types and its retained `Finding`. A newly
introduced interview result type required `:type respond` and `:info Interview`.
It proposed showing the reply constructor shape in activation context. That is
a concrete discovery cost, but the suggested automatic expansion is untested;
large or recursive result types could make it counterproductive.

The root's priorities for longer sessions were compact, inspectable working
memory and quieter asynchronous conversation. Receipts supplied useful custody
facts but buried authored findings under repeated identities and nested output.
Broad `:browse` output also interrupted product reasoning. Native source-search
and Git commands remained useful; this run supplies no reason to replace them.

The root did not voluntarily use `fmt`, rich closures, or substantial pure
computation in the product campaign. It explicitly declined to claim benefit
for those unexercised features or economic superiority for low-effort children.

## Testing the proposed inspection API against ordinary Haskell

After the interview, the observer asked the root to try its own local projection
before recommending a new runtime API. The following definitions succeeded on
the first attempt, without more discovery calls:

```haskell
data CustodyView
  = NoCustody
  | CustodyProblem WorktreeError
  | CandidateCustody GitOid [Text] WorkingState
  deriving Show

let custodyView NoBoundWorktree = NoCustody
    custodyView (WorktreeObservationFailed e) = CustodyProblem e
    custodyView (WorktreeObserved _ _ submission) =
      CandidateCustody (headOid (submittedHead submission))
        (committedPaths submission) (workingState submission)

let findingView (ReplyUnavailable failure) = Left failure
    findingView (ReplyAvailable result) =
      Right (responseValue result, custodyView (responseWorktree result))

let firstFindingView WatchPending = Nothing
    firstFindingView (WatchUnavailable failure) = Just (Left failure)
    firstFindingView (WatchReady (finding, _)) =
      Just (Right (findingView finding))

firstFindingView results
firstFindingView finalResult
```

This is formatted from the executed definitions; multiline input still needs
the resident tool's usual input-unit grouping. The projection preserves failure
cases and hides irrelevant custody fields. Its outer shape is specific to this
campaign's paired watches; it is not proposed as library surface.

The root said it would keep the custody/finding projections and that this
experiment materially weakened its case for a new inspection API. It still
found `Just (Right (Right ...))` rendering awkward. A task-specific sum type
could improve that too; it was not tested here.

## Cache and operational observations

Both children's first recorded usage was 16,768 cached / 32,552 input tokens
(51.5%). The observer extracted the first token-count event from each child
rollout. The actors themselves did not retain that first-inference observation
and correctly refused to infer it from later usage.

This is substantial reuse but not the minimal canary's 91%. The cached amount
is close to the root's initial 16,853-token request; a larger prefix accumulated
before the fork. That relationship is an observation, not proof of the provider's
cache placement policy. Do not label the entire inherited context cache-verified
or promise a fixed hit ratio from this run.

Startup exposed environment friction: ambient PATH lacked bubblewrap and later
selected an obsolete extractor. Explicit local extractor selection inside
`nix develop` resolved both. The old retained Sol host owned the project's
binding registry; the user authorized stopping it. Its shutdown needed time to
release ownership before the new launch succeeded. No lock files were deleted.

The TUI offered model-switch suggestions after idle turns; the observer selected
keep-current-model so the experiment remained Astra medium/low. Child shutdown
also generated a root lifecycle wake with no substantive work, illustrating the
root's complaint about conversational noise.

## Recommendations

1. Preserve the small typed fork/request/watch core and ordinary Haskell
   projections. This campaign does not justify another orchestration layer.
2. Teach one brief habit: when a retained value becomes noisy, define a local
   view and reuse it. Avoid prescribing the view's schema or shipping these
   exact helpers as universal APIs.
3. Investigate compact default rendering with expandable custody detail at the
   existing presentation owner. Preserve typed evidence and explicit failures.
4. Make input and reply-type discovery adjacent at activation. Test a compact
   discovery affordance before automatically dumping constructor definitions.
5. Examine redundant lifecycle/status activations and presentation noise while
   preserving durable wakeups. Do not solve noise by weakening delivery.
6. Expose first-inference cache observations separately from latest usage.
7. A future computation-heavy task should test model-authored functions over
   real retained evidence. This small UI edit mainly tested persistent context
   and actor follow-up, not the full expressive Haskell premise.

No harness/API changes were implemented from these interviews. Both children
were stopped through typed `stopAgent` handles, returning `StoppedNow`; the root
and useful worktrees remain retained.
