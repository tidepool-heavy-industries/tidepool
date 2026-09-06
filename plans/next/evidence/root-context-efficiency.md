# Context-efficiency follow-up

User requirement: each lead and its descendants should note context-window waste,
especially Haskell-tool discovery and bookkeeping, and propose UX fixes before
next wave. Collect concrete examples and distinguish firsthand observations from
inferred savings. Prefer small fixes in owning prompts, shared in-system API
guide, focused docs or tool behavior; do not duplicate a verbose activity log.

## Root observation

The root attempted exact active-request amendments to all three first-wave leads.
All three returned UpdateNotPresented:
`connecting update proxy: app-server closed the connection before responding to initialize`.
Retained workbench evidence: serviceCtxUpdate/runMapCtxUpdate/usageCtxUpdate and
serviceCtxState/runMapCtxState/usageCtxState. Request IDs 1, 2, 3; update sequence 1.
No lead receipt or incorporation is established. No queued-assignment fallback
was attempted. This is delivery UX friction, not measured token/cache evidence.

Outstanding: collect retrospective notes from each retained lead once its active
assignment settles; propagate to remaining work using a confirmed route. Include
observed missing documentation, avoidable discovery/administration turns, and
small proposed owner-specific fixes. Review actionable fixes before launching
the next wave. The service transport replacement already owns this delivery bug;
do not add another steering channel as an incidental workaround.
