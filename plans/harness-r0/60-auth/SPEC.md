# Spec: provider client — ChatGPT-subscription OAuth + API-key mode

> **STEERING (operator, 2026-07-23): this segment is operator-steered.**
> Thesis: an existing crate likely covers the client/OAuth surface —
> handcoding is presumed wrong. First deliverable is a crate SURVEY
> (OpenAI-compatible clients, Codex-auth/oauth2 device+PKCE coverage,
> maintenance signals, remaining glue) with a recommendation, before any
> implementation. The steps below describe the required BEHAVIOR, not a
> mandate to hand-write it.
>
> **DECISION RECORD (2026-07-23, post-survey):** chat calls route through
> `genai` (already a workspace dep — AuthResolver/ServiceTargetResolver);
> OAuth = Codex flow 1 (auth-code+PKCE loopback :1455) via the
> `openai-auth` crate (v1.0, MIT; does not persist tokens — our
> config-dir/0600 glue does). The beta device-code flow is NOT ported
> (undocumented wire shape, server-issued PKCE verifier — reopens only if
> remote-browser sign-in becomes a hard requirement). R0 constraint,
> documented in the module: sign-in browser needs `ssh -L 1455:...` to
> the harness box, one-time; refresh automatic.

The harness spawns calling-model turns (parent programs' authors and
child answerers). R0 bills those to a ChatGPT subscription via
Codex-style OAuth sign-in, with API-key mode as the co-equal supported
configuration (F4). Fully independent of all other segments.

## ANTI-PATTERNS

- DO NOT couple harness core to a provider — ONE trait
  (`ModelProvider: complete/stream`), two impls (oauth-subscription,
  api-key). The harness knows turns and tokens, not providers.
- DO NOT put auth in the consent path — reachability (tailnet) is the
  authorization boundary for the UI (F2); provider auth is only for
  upstream billing.
- DO NOT store tokens in the repo or world-readable paths — config-dir
  secrets location per `tidepool_runtime::paths` conventions
  (`~/.config/tidepool/secrets/`), 0600.
- DO NOT hand-roll OAuth flows beyond what the provider requires —
  device-code/PKCE flow, refresh handling, clear re-auth error when the
  refresh token dies.
- This is NOT the in-program `Llm` effect (that stays as-is in
  tidepool-handlers) — different consumer, different budget accounting.

## Leaf (sonnet)

1. `ModelProvider` trait in the harness crate (contracts.md);
   request/response shapes minimal for turn-driving (messages in, text +
   usage out; streaming optional in R0).
2. OAuth impl: sign-in verb on the protocol (`auth/start` →
   verification URL + code, `auth/status`), token persistence + refresh.
3. API-key impl: env/config key, same trait.
4. Both impls pass ONE shared behavior suite (record/replay HTTP
   fixtures; no live calls in tests).

## VERIFY

`cargo nextest run -p tidepool-harness` green; manual smoke: sign in
once, run a turn, `meters` usage recorded in the event log; flip config
to api-key mode, same suite passes.
