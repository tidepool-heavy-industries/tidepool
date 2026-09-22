# JIT memory lifetime

Status: open follow-up, with no reclamation implementation scheduled. The actor
guide links here because retiring a binding root does not establish that its
compiled code is unreachable.

`tidepool-codegen` owns JIT module allocation and releases it when the module is
dropped. `PersistentSession` owns a live machine; there is no live-machine code
collector. A future design must trace the same closures, literals, compiled
references, metadata, and parked continuations used by execution. Actor
liveness or lexical-scope retirement alone cannot establish code reachability.

Measure retained code under real workloads before adding reclamation or reuse.
Any reuse key must include resolved binding identity and compiler settings;
matching source text alone is not enough. Process RSS alone does not prove that
code was reclaimed.
