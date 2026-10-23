# Compiler-service group

Lead: /root/compiler_backing_lifetime, also P9 implementation.
Pending consolidated source2ee9c1bf60 contains dependency/SOURCE/runtime earlier
work and finite cleanup; two test WIPs remain separate. Interface series through
37e1693ffe is unjoined and has six WIP files. No joined compilation claim.
Do not let the new domain model become adapters around those partial candidates.

## P7 typed raw-request admission — /root/worker_request_shapes

Own WorkerRequest admission, CompilerRequest dispatch/HarnessSource and affected
Main entrypoints, coordinate GhcPipeline API calls with P9.
Decode raw record into operation-specific closed payloads with required fields.
Pure full admission precedes executor/RTS/source/harness scratch activity.
Delete repeated admitRequestOperation and failure-as-source fallback.
Direct and daemon modes share the resident kernel; no new lifecycle engine.

Acceptance: invalid mixed operations produce no source reads/scratch/executor
creation; positive operation variants run via real entrypoint. Protocol BEGIN/
request/END retains existing acknowledgements and cancellation semantics.

## P8 interface installation — /root/interface_backing_impl

Replace WIP free BackedInterface construction with captured owner -> hydrated
environment -> installed lexical view. Hydration and actual Finder registration
are one owner's transitions. Supply complete captured inputs from both ExactScope
and World adapters, including private inputs; no Maybe verified array paired
with raw artifacts. Keep TYPE/HPT-only passive paths and exact SOURCE proof distinct.
Handle pre-cycle qualification in its own cycle backing bracket. Make phase
environment restore a checked owner transition. Remove raw HOME reads and fake
Finder routes, including private prefixes/Session source-less entries.

Acceptance: cold qualification, private operands, completed prefixes, NameCache
mismatch, deleted cycle .hi paths with retained lazy HMI, no passive TYPE
materialization, no registration before hydration. Review five prior audit findings.

## P9 explicit compiler state — group lead

Fixed boot/package universe + completed version owners + active scope attempt.
Remove mutable latest reusable environment and runOperation Finder publication.
Narrow helper operations so no arbitrary HscEnv can silently become reusable.
Preserve actual completed HMI closures including reached target SOURCE backedges,
not unrelated target reuse roots; reject missing dependencies.
Converge supported finite wrappers onto resident kernel and delete StandaloneCycle
boot/cleanup alternate implementation. Test-only no-authority helpers must not
issue compiler completion.

Acceptance: compile/helper/END/newBEGIN/reuse; cold/evicted/reacquired selection;
missing dependency, cross-NameCache, lazy HMI after directory removal; cancellation/
uncertain cleanup refuses success/END. Preserve warm package/interpreter reuse.

## P10 World binding and reservation owners — /root/world_owner_resume

Own cell_plan and World open/restoration input/visible-binding structures.
One ordered ReservedWorldItem owns kind/generation/binders; derive slots.
VisibleWorldBindings owns occurrence->actual row and maintains indexes privately.
Keep actual WorldPrimaryInputs once. No supplied separate visible-row roster.
Link raw Base/Operand/Prior input variants to actual issuer before admitted stages;
use variant-specific parsed seal types. Coordinate stage types with P11/P12.
Preserve reserve validation before one-shot gate consumption.

Acceptance: foreign/duplicate/mismatched bindings and slots refuse at admission;
prior stage missing producer/binder fails before any admitted stage exists;
valid declaration/expression/empty workflows preserve allocation witnesses.

## P11 typed query ledger and sealed dependencies — /root/astra_dependency_plan_impl

Implement reviewer proposal in CONTRACTS atop earlier SOURCE/native work.
Distinct concrete query/reply types, one pending per-stage ledger, consuming
Result finish, all stage keys validated against emitted starts.
Batch authenticate terminal occurrences by exact use and retain payload owner;
borrow handles owner+index, avoid stage->terminal->stage cycle.
Remove repeated closure walks and raw terminal access. Keep native projection
narrow; SOURCE cannot call native acquisition. Keep actual request/World/operation
fixed across TYPE overlays. Review by astra_joined_dependency_review.

Acceptance: SOURCE orphan stage fails, empty/emitted SOURCE stages pass; selected
reference mutations fail at issuer; reached ambiguity refuses while unused remains
inert; N vs2N traversal per distinct use, no traversal at sealed access.

## P12 admitted program cursor — /root/compiler_stage_cursor

Own WorldProgramContinuation/Segment and artifacts ordered stage consumer; agree
World checked stage access with P10/P11. Open_next returns branch-specific pending
stage; only successful seal produces advancing state. Sealed result carries
output/context/source evidence. Remove unconditional finish and parallel outputs/
planned declarations/item context arrays. Independent SOURCE versus native prefix
histories remain distinct; no fake single phase that discards one.

Acceptance: unsealed stages cannot advance, wrong/replayed ordering refuses,
source-only advancement retains earlier native evidence, actual multi-stage
historical capture and failure/cancellation cannot publish future outputs.
