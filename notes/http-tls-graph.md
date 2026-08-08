# HTTP / TLS dependency graph — investigation

Measured from the committed lock with read-only `cargo tree --locked -e normal`
(no lock rewrite, no build). Question asked: can the two reqwest majors be
consolidated, and is the HTTP/TLS graph the dependency optimization target the
build audit called it?

## What is in the graph

| Client | Version | Reached via |
|--------|---------|-------------|
| reqwest | 0.13.4 | `tidepool-harness`, `tidepool-web`, and `genai 0.5.3` (→ handlers, tidepool, repl) |
| reqwest | 0.12.28 | `openai-auth 1.0.0` → `tidepool-harness` → `tidepool-web` |
| ureq | 2.12.1 | `tidepool-handlers` (→ tidepool, harness, repl) and `examples/tide` |

Crypto: `rustls 0.23.42` resolves with **both** provider features enabled —
`aws-lc-rs` (reqwest's default) **and** `ring` (ureq 2.12's). `ring` is also a
direct dependency of `jsonwebtoken 9.3.1` under `openai-auth`. `webpki-roots`
appears twice: 1.0.9 (reqwest) and 0.26.11 (ureq).

## Can reqwest be aligned?

No, not from a manifest. `openai-auth` requires `reqwest ^0.12`, and 1.0.0 is
the newest release — the sparse index shows 0.1.0, 0.2.0, and 1.0.0, all with
`reqwest: ^0.12`. There is no version to move to, and forcing a `[patch]` across
incompatible reqwest majors is exactly the move to avoid.

## What the duplication actually costs — the number that decides this

The reqwest 0.12 subtree is 135 packages, the 0.13 subtree 142. Every package
they share resolves to the **same version** in both. Set difference: the only
package unique to the 0.12 subtree is `reqwest 0.12.28` itself.

So the reqwest split costs one extra crate compile. It does not duplicate the
TLS stack, the HTTP core, or the async runtime: hyper 1.x, rustls 0.23,
aws-lc-rs 1.17.3, tokio, and webpki-roots 1.0.9 are shared by both majors.

That is a much smaller prize than a fork of `openai-auth` would be worth. Our
usage of that crate is `OAuthClient`, `OAuthConfig`, `TokenSet`, and
`run_callback_server` (`tidepool-harness/src/provider/oauth.rs`) — small enough
to reimplement on reqwest 0.13, which means the deferral is a scheduling choice
rather than a trap: the door stays open.

**Verdict: defer.** Revisit when `openai-auth` releases a 0.13-compatible
version, or if the OAuth integration is being rewritten for another reason
anyway. Do not fork to save one crate compile.

## The higher-value target is ureq, not reqwest

Two HTTP clients with two different rustls providers is the part of this graph
that costs real build time. `ureq 2.12` brings `rustls/ring` and
`webpki-roots 0.26`, while everything else in the workspace is already on
reqwest with `rustls/aws-lc-rs`; Cargo's feature unification then compiles both
providers into rustls. Moving `tidepool-handlers` (and `examples/tide`) from
ureq to the reqwest that is already in the graph would drop `ureq`, the second
`webpki-roots` generation, and rustls's ring provider.

That is source work, not manifest work, so it is out of this batch's scope —
recorded here as the actual next target, with the measurement that justifies
ranking it above reqwest alignment.
