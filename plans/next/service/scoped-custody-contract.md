# Scoped custody host co-owner

Base service 3c96f192. Ownership is restricted to ActorWorkspaceCustody portions
of actor_host.rs, private scoped_custody module/tests, and fork_workspace.rs.
Do not edit host_dynamic_tools, inbox, BindingTable, EventJournal or LogWriter.

The private noncloneable ScopedCustodyOwner pairs the exact installed custody
lease (therefore its ActiveBinding generation) with the ServiceScope obtained
from its own spawn. Construction consumes a one-shot claim on that custody.
No release(custody, externalReceipt), no public completed bool, no copied receipt
as authority. Cleanup calls the captured scope itself. A caller cannot substitute
a sibling's scope or receipt. Concurrent/duplicate claim must be rejected.

Keep legacy process_may_exist irreversible and incompatible with scoped claims.
Track actor terminal observed separately from not-yet-stopped: the existing
actor_stopped callback is the source, not a new terminal registry. Last Arc drop
is not successful BindingTable settlement evidence.

At this baseline synchronous PreparedServiceScope::spawn Err is entirely before
successful spawn. Pinning is separate. Any lost asynchronous spawn result,
pinning error, cleanup timeout, or unobserved process state conservatively retains
custody/resources. A failed pre-spawn attempt must not manufacture release proof.

Host HTTP task plus resident effect quiescence is an unsatisfied prerequisite.
The scaffold's HostWorkQuiescence is deliberately uninhabited; stop can report
process cleanup with host-work-pending but cannot settle custody. Do not make
this inhabited by a test/public boolean or fake drain. Any future settlement
must observe the exact BindingTable result, preserve uncertainty, and await
host-tools integration. No scope launch switch or native controller before pin.

Implementer owns concrete claim/intent transitions, guard lifetimes and real
bwrap tests: sibling substitution/double claim/generation denial, true pre-spawn
error versus lost-result/pin/timeout retention, actor-active and host-pending
retention, duplicate retirement, and inability of legacy/copyable cleanup to
release. Independent reviewer requests contract-local repairs. Explicit no-settle
is acceptable; describe every remaining gate rather than overclaiming cleanup.
