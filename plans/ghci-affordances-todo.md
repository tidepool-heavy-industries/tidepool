# TODO: GHCi affordances for harness turns (`:t`, multi-item turns)

## Priority 0 (Inanna, same session): types IN the prompt beat `:t`

For CORE types — tidepool-native surface a model cannot know from
training data (Form fields, Fork/Finalize verbs, effect records, answer
contract types) — the signature belongs in the prompt so no probe round
trip is ever needed. `:t` is the fallback for the long tail, not the
mechanism for the core. Landed for Form fields in d82cf099
(`ANSWERER_FRAMING_SUFFIX`). Remaining mechanism: the hole-card can render
a synopsis from the answer type's `DataConTable` — but an HONESTLY SHALLOW
one: the table captures field LABELS, not field TYPES (`uiof.rs` module
doc), so the near-term output is `Contribution { addedIdeas, draftDelta,
advance }`, constructor + selector names only. Still kills most of the
authored duplication. The full typed declaration arrives with the
generic-surface wave's `GTypeDoc` interpreter (see
`self-iterating-harness/15-generic-surface-wave.md`), which supersedes
this; extending extract metadata with field types is the middle option
only if that wave slips. Routed: shallow synopsis → harness-lifecycle
wave-1.5.

Inanna, 2026-08-08, during live dogfooding: "we _kinda_ want to encourage
ghci-style usage of the tool — maybe we could support `:t`? and support up
to N lines? … or that's a thing that makes more sense as just giving models
a tool, maybe — either could work."

Motivating incident: the wizard session's answerer burned retries probing
the type system through failed compiles (the only type oracle it had was
the corrective-retry error text). A model fluent in GHCi would have typed
`:t textField` first.

## Two candidate shapes

1. **In-dialect** (on-thesis: the API is the prompt, GHCi is the prior).
   A turn whose block is `:t expr` (or contains leading `:t` lines) gets a
   type answer instead of an execution. Precedent already in-tree:
   tidepool-repl's block-runner classifies `:commands` (`:t`, `:i`,
   `:browse`) today. The one-spawn-turn protocol (Phase B, in flight)
   moves turn classification INTO extract — `:t` support becomes one more
   classification arm there, shared by repl and harness by construction.
   "Up to N lines" similarly maps onto repl's existing multi-item block
   classification (decl/bind/expr sequences), which the harness could
   adopt once the shared path lands.
2. **Tool-shaped**: a separate typecheck probe the model calls outside the
   eval block. Cheaper to bolt on, but off-thesis (reintroduces the
   N-tool-calls surface the eval block exists to replace) and splits the
   model's attention between two surfaces.

## Sequencing decision

Do NOT build either before Phase B (one-spawn-turn) lands: extract-side
classification is the natural mechanism for shape 1, and building shape 1
pre-Phase-B means writing the classification twice. Revisit at the
extract-wave TL's spawn; the interim mitigation is signatures-in-prompt
(landed 2026-08-08: `ANSWERER_FRAMING_SUFFIX` field-builder signatures +
compiling finalize shape).

Tier-0 telemetry angle: if `:t` lands, its usage rate per session is a
direct dialect-adoption metric (models reaching for GHCi affordances
unprompted = the fluency thesis observed live).
