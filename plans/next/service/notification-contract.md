# Notification vertical contract

The actor-owned seam is `notification.rs`: exact sender/target and opaque receipt
locator; separate send and poll handoffs to the existing deployment event loop.
Receipt is not authority: poll checks the calling actor against the issuing owner,
then host checks exact inbox instance and retained row sender/target. No receipt
ID issuer or registry. Inbox key comes from existing run/actor namespace.

Notification target resolution follows typed request routing (exact live actor in
this forest), not subtree-only stop authority. Peer notification through retained
handles is a supported use. Sender identity comes from actor context, never payload.
Observation is owner-only; stale incarnation/foreign inbox/forged sequence is denied.

`Notifications` is a distinct extensible effect; generated protocol remains the
single source for effect declarations/decoding. The public facade exports notify,
pollNotification, abstract NotificationReceipt, small NotificationError and state.
No concrete row order dependencies. Add it deliberately to appropriate actor role
profiles and shipped facade, not via ambient authority broadening.

Send/poll host command tokens have exactly one completion sender and no typed actor
response obligation. An active request is never replaced, settled, or fenced by
notification. Provider idle does not imply no active request. Notification handling
must not mount sessionInput/sessionReply/respond; existing request mounting remains
sole owner. Retained historical bindings are not a newly granted reply capability.

Production send rejects Unavailable before publication: there is no correlated
native controller. Production poll validates the exact inbox key and persisted
sender/target provenance. This is an explicit missing send seam, not mounted
notification delivery. Host tests drive actual interpreter handoffs into the sole
DurableInbox and preserve original request bindings; they do not model a native
provider receiving a notification.

DurableInbox owns bounded receipt evidence and a noncloneable pre-send attempt.
Legacy pending() fails closed on tracked rows; the host uses legacy_pending_prefix
so ordinary prefix rows can progress without sending or acknowledging tracked
rows. Notification rows use existing text payload encoding plus typed envelope
receipt provenance, not a new payload variant that old tail repair cannot decode.
New-format inboxes still must not be reopened by old binaries.

Node-local strict durability is accepted under the single-open-owner and
controlled-hierarchy contract. Accepted atomic-write/JSONL owners propagate
parent open/fsync failures; inbox prepares BOTH parent hierarchies with the shared
durable directory helper and stabilizes surviving rows/cursor after tail repair
before exposing receipt/send capability. Symlink entries, targets and target
ancestry must already be durable; concurrent hierarchy replacement is excluded.
At integrated source c5b74bf8, 22 inbox tests (including 20 hit-checked syscall-fault
subprocess cases) and 3 notification host regressions passed. These prove failure
propagation, poison/no retry and process reconciliation, not physical power-loss
safety. Native send remains unavailable. Pre-deployment socket cleanup on failed
inbox open remains a separate service-owned startup obligation.
Submitted transport acceptance maps to Unconfirmed, never Presented; only explicit
correlated presentation evidence may establish Presented.

Ownership: schema/runtime worker owns notification.rs, actor effect plumbing,
protocol/generated consumers, Haskell facade and focused actor tests. Inbox worker
owns tidepool-node inbox evidence extensions and tests. Notification TL owns
actor_host.rs host consumer and separate host test module, plus manifest escalation.
Custody test exhaustive event descriptions were updated in shared scaffold; avoid
concurrent edits there. Native service process/controller wiring belongs to service
TL, not this subtree. Unsupported native behavior must remain visible.
