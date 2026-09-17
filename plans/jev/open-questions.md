# Open questions, risks, and decisions that remain the user's

Recorded 2026-09-16. Everything below is unresolved. Some are experiments,
some are policy, some are the user's call.

## About the provider

- **Rate limits and concurrency.** No concurrent calls were made. The
  investigation hylo with `hyloConcurrentM` and run-ahead on several triggers
  at once will issue overlapping packets. Limits, backoff behavior, and
  whether the provider degrades under load are unknown.
- **Calibration on our domain.** The publisher claims calibrated
  probabilities. Every threshold in these documents assumes that roughly
  holds for code and coordination judgments. The replay corpora will show
  whether a 0.8 means 0.8 here.
- **Stability across model versions.** `jev-latest` is an alias. A new
  version changes distributions; margins tuned on `jev-1.13.0` may not
  transfer. Pinning versus tracking is a policy decision, and the journals
  must record the resolved model.
- **Key bias magnitude on real pools.** Measured on synthetic duplicates;
  unmeasured on decision pools and edge pools with real descriptions.
- **State size in practice.** Single-question state passed at 768 synthetic
  records and 32,568 input tokens. Real states are structured and smaller,
  but the wake-economy packet over a large batch view could approach limits.
  Pool trimming policy is undecided.
- **`jev-preview`.** Currently identical to latest. Worth rechecking when it
  diverges.
- **The `maybe` Noul criterion and other accepted extras.** Accepted, meaning
  unknown. The DSL exposes the documented closed shape only.

## About the DSL

- **`:-` collision.** Qualified Jev operator or a shared open family. Sol's
  call.
- **Pool position.** Recommended in [dsl-review.md](dsl-review.md); not
  prototyped. Whether the generic traversal can serialize a pool into state
  and reference it from sibling fields in one pass is untested.
- **Dynamic heterogeneous question maps.** Static records and `Each` over a
  homogeneous schema are covered. A fully dynamic map with mixed kinds and
  retained answer association is not designed.
- **Missing-fields as error.** Recommended as compiler configuration for
  session modules. Whether that flag surprises authors in other contexts is
  a documentation question.
- **Where `Given` renders.** Premise-prefixed questions need the premise in
  the instruction. Whether the premise is a type-level string, a value, or a
  reference to a sibling field's key is open.

## About Shoal integration

- **Where triggers hook.** Coordination actor sinks, the batch driver around
  `advance`, or authored combinators. Run-ahead argues for authored first.
- **Recommendation-only mechanics.** Journaling a would-be action beside the
  real one needs a journal record shape and a comparison tool. Neither
  exists.
- **Authority boundaries.** Every document says Jev creates no authority.
  The concrete rule for which deterministic actions may follow a judgment
  without a model (deliver a decision, re-issue a repair within budget, queue
  a note) versus which must wait (merge, steer, halt) needs to be written
  into policy, not left to each author.
- **Budgets.** Per-turn packet budgets like the LLM call cap, per-trigger
  read budgets for run-ahead, and who owns them.
- **Retention.** Packets and responses are evidence. Which journal owns them,
  how they are referenced from wakes and cell outcomes, and retention limits.
- **The `WorkSink` is pure today.** Decision memory needs an effectful sink
  or a sink that returns a typed request for the actor to perform. The
  coordination-actors plan already notes the effect profile must admit Jev.
- **Model-facing documentation.** The notebook is the model's interface.
  Authors are models. The library's documentation, examples, and error
  messages are the product; none exist.

## About the thesis

- **Is a quarter of planner rounds really avoidable?** The estimate comes
  from reading the state machines, not from measurement. The question-events
  corpus answers it quickly.
- **Does run-ahead pay for its reads?** If most failing checks wake a model
  that would have started elsewhere, run-ahead is wasted tool time. The
  citation rate in the investigation benchmark answers it.
- **Do agents trust prefilled fields appropriately?** Too much trust hides
  wrong judgments; too little wastes the prefill. The overwrite rate is the
  monitor, but the right rate is unknown.
- **Does pervasive judgment change how authors write cells?** The
  microprogram patterns assume authors adopt the cadence rule. If models
  instead call Jev per step, the cost and latency profile changes. The
  packet-per-cell distribution in journals answers it.
- **Is the frontier model a good misprediction handler?** A model that wakes
  into a heavily prefetched state may anchor on the speculation. Whether
  wakes should present speculation prominently or as an appendix is a
  prompt-design question with a measurable answer.
- **Does the helper actually grow?** The System 1 framing predicts that
  recurring handback reasons become authored branches in the same session.
  Whether models do this unprompted, whether the branches they add are
  sound, and whether a grown helper stays readable are open. The fallback
  rate per reason over time is the measurement; the prompt guidance that
  invites extension is unwritten.
- **Cost of a handback versus a wrong commitment.** A cell that hands back
  early is cheap and safe; one that commits wrongly is expensive to
  discover. The floors for `defer_to_model` and near-tie handback should
  start generous and tighten only as checked outcomes accumulate.

## Explicitly out of scope

- Bounding boxes and any vision primitive.
- Prompt-injection and adversarial inputs; inputs are trusted codebase and
  Shoal state.
- Operator chat, generated commands, batching frameworks.
- Any engine change; production integration waits for the STG cutover.
