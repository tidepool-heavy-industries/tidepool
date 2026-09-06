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

Scaffold consumer currently rejects send and poll as Unavailable before publication:
there is no correlated native controller. This is an explicit hole, not working
notification behavior. The host implementation must use the sole DurableInbox for
accepted publication/evidence. Legacy push/ack cannot establish presentation and
must not accidentally deliver tracked notifications in its ordinary batch.

Durability design is pending the inbox specialist: record before-send ambiguity
fence before touching transport; uncertain publication/send cannot be auto-retried.
Ack/compaction/reopen must not invent Presented. Observation retention is bounded,
with explicit unavailable/expired result rather than false success. Generic node
payload contains sender identity; node must not depend on actor/runtime types.

Ownership: schema/runtime worker owns notification.rs, actor effect plumbing,
protocol/generated consumers, Haskell facade and focused actor tests. Inbox worker
owns tidepool-node inbox evidence extensions and tests. Notification TL owns
actor_host.rs host consumer and separate host test module, plus manifest escalation.
Custody test exhaustive event descriptions were updated in shared scaffold; avoid
concurrent edits there. Native service process/controller wiring belongs to service
TL, not this subtree. Unsupported native behavior must remain visible.
