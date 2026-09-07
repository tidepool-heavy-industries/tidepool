# Opt-in service namespace lifetime contract

Scaffold 99382fdd; root-owned rustix dependency af94f6050 incorporated as d120044f.
Existing ProcessMountBoundary::wrap and live tmux mode remain unchanged. The
service-only source/API lives in process_scope.rs, reached through
prepare_service_scope; the explicit service_scope example is its current consumer.
Service TL owns eventual host composition and lib.rs public reexports.

## Audited gate and witness

At bwrap 0.11.0, --unshare-pid with normal init/reaper (never --as-pid-1 or
--pidns) reports namespace init's outer PID through private info-fd. Block-fd is
read before command fork. It ignores EOF/errors, so a bare RAII gate is UNSAFE.
Use an empty pipe with BLOCKING read end and a distinct duplicate WRITE end as
sync-fd. Init retains its own writer before the block and in the reaper keep set;
exec child closes it. Host writer closure alone cannot produce EOF/start. Descriptor
numbers must be distinct, valid, above stdio, and only deliberately inherited
child descriptors may lose CLOEXEC. No nonblocking gate reader or option injection.

Pin ordering: retain proc directory -> pidfd_open(reported PID) -> fresh status
open/read through retained directory. Validate exact still-owned monitor PPid,
proc mount PID view, one new namespace with NSpid ending in 1, and reported
namespace inode consistency. Keep monitor unreaped; no competing wait or automatic
SIGCHLD reaping. No cached status read and no signal before witness validation.
Store witness before deliberate gate-byte write. A number from info is not proof.

For exact default namespace init, audited upstream Linux v6.12.63 orders
zap_pid_ns_processes before exit-state publication/pidfd notification. Checked
init exit therefore witnesses namespace drain, not Shoal reaping init. Outer
monitor Child::wait is separate. Only both facts for the same retained owner
construct ServiceScopeCleanup; never timeout, POLLNVAL, ignored error or arbitrary
process exit. Coverage is local namespace descendants, not external daemons,
remotes or host-side Haskell tasks.

## Failure/verification boundary

Before-pin host/monitor death may strand blocked init. Sync self-hold prevents
payload launch but does not prove cleanup. Pin/stop errors borrow the owner and
retain uncertain resources; no retry, custody release or invented completion.
Destructor emergency signaling is not a successful cleanup receipt. Tests must
force writer closure before release, invalid/stale identity, monitor death, and
unavailable proc/namespace permission cases. Use real mounted detached descendants
and exact init witness plus monitor wait for positive cleanup, with deterministic
pipe barriers rather than timing guesses. Fix unsafe example error paths as needed.

Read-only independent identity and gate audits remain retained by namespace TL.
Sources: bubblewrap v0.11.0 bubblewrap.c (monitor_child, block read, default reaper
keep-set); Linux v6.12.63 kernel/exit.c, kernel/pid_namespace.c, fs/pidfs.c,
fs/proc/base.c and fs/proc/internal.h. Audits establish source reasoning only,
not exact local-kernel binary identity or executed process behavior.
