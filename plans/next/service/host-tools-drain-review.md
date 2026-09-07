# Independent HTTP lifecycle review

Source reviewed at0f6fb7aaae3be01fd3c1ca4c960e0cb6e9f103bc against
host-tools-drain-contract.md. The implementation is accepted for staged parent
integration, not deployment or effect/custody retirement.

The watch lock linearizes phase publication and admission. Quiesce only moves
Serving forward; drain cannot reopen; cloned controls share one service phase.
Handlers admit at their owning entry after request extraction and before endpoint
await. An admitted pre-fence handler may subsequently dispatch, including waiting
for session/endpoint locks. Quiesce return is NOT proof that every accepted call
has already reached the endpoint. Host integration must account for this rather
than snapshotting actor-idle immediately and releasing custody.

Completion and registration remain available in Quiescing, including fresh
connections. Draining rejects new handler admission and triggers the existing
axum connection supervisor's graceful shutdown. No new work ledger or endpoint
counter was added. A timeout must retain the same server JoinHandle and its
result; Drop/abort is not an equivalent receipt. The production caller currently
retains no control and still aborts the listener; that old integration is not
made safe merely by this owner implementation.

Trace: ResidentInteractivePolicy dispatches through ResidentToolClient;
resident_tools::dispatch_workbench submits KernelMessage::Workbench then awaits
oneshot reply; complete similarly submits ToolCompleted. Dropping HTTP waiters
is not a demonstrated cancellation of these actor-owned submissions. Tests use
gated endpoint futures, not actual Haskell cancellation/retirement. In particular
the disconnected-client test demonstrates explicit fixture release and no second
dispatch; it does not prove survival/completion of arbitrary detached effects.

Inspected real Unix HTTP tests: blocked accepted call, new/pooled client call and
session rejection, completion while Quiescing, bounded drain timeout retaining
&mut handle followed by successful await, idle keepalive/raw connection drain,
client abort followed by explicit endpoint release. HTTP client pooling is used;
no kernel-level socket identity assertion establishes every reuse. No mounted
native controller, lost-completion replay, or process-custody release is tested.

No source repair required for the scoped contract. Parent should remove staged
dead_code allowances when wiring control; keep existing endpoint completion/replay
owner and do not promote HTTP drain to an actor-terminal receipt.

Independent execution at exact0f6fb7aa: `NEXTEST_TEST_THREADS=1 just test-lib
tidepool 'test(host_dynamic_tools::drain_tests) | test(host_dynamic_tools::tests)'`
passed12, excluded110; nextest5a139994-9b1e-4d0e-9c0e-eb937b63962d. Full tidepool
library test target compiled; private compile daemon teardown observed. Formatting
and diff checks passed. Retained reviewer evidence: target/http-lifetime-review/
{tests.log,hashes.txt}. No running host or external native source changed.
