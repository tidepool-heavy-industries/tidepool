# tidepool-web — protocol server + Datastar observatory (R0 scaffold)

Everything is HTTP+SSE protocol FIRST (E1): the observatory is a client of
the documented verbs, no private APIs, everything curl-able. Loopback bind
only — reachability (tailnet) is the authorization boundary.

Stack (locked): axum + maud, Datastar official Rust SDK for SSE-pushed
fragments — server owns all state (stateless-per-render; state lives in
the tidepool-harness session tree + event log). No JS build step; Datastar
is one vendored file. Haskell never sees this layer — the `Ui` eDSL
arrives as data and is rendered here (segment 50).

Segment specs: `plans/harness-r0/30-harness-core/SPEC.md` (C4 protocol),
`plans/harness-r0/50-ui-edsl/SPEC.md` (renderer + D1 tree view).
