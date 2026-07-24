# tidepool-web — protocol server + Datastar observatory (R0 scaffold)

Everything is HTTP+SSE protocol FIRST (E1): the observatory is a client of
the documented verbs, no private APIs, everything curl-able. Loopback bind
only — reachability (tailnet) is the authorization boundary.

Stack (locked): axum + maud, Datastar official Rust SDK for SSE-pushed
fragments — server owns all state (stateless-per-render; state lives in
the tidepool-harness session tree + event log). No JS build step; Datastar
is one vendored file. Haskell never sees this layer — the `Ui` eDSL
arrives as data and is rendered here (segment 50).

Trust model: loopback binding is a NETWORK reachability boundary, not an
injection-surface defense. `Ui` content (`Prose` text, `Choice` keys/labels)
can originate from the calling MODEL via `dialogAsk`, and a prompt-injected
model is an untrusted-input carrier whose output the operator's own browser
then renders/executes same-origin. What actually closes the injection
surface is at the eDSL source, in `render.rs`: raw HTML in `Prose` is
neutralized to escaped text (never live DOM), and every model-supplied
`Choice` key is percent-encoded before it's interpolated into a
`@post('...')` JS string target.

Segment specs: `plans/harness-r0/30-harness-core/SPEC.md` (C4 protocol),
`plans/harness-r0/50-ui-edsl/SPEC.md` (renderer + D1 tree view).
