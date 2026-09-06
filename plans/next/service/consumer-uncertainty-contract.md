# Consumer uncertainty follow-up

Baseline 4088cd94 incorporates reviewed strict owners. No format migration.
Each writer now has a private write_uncertain latch, initially false. It becomes
true upon any potentially visible persistence failure and never resets on that
handle. Follow-up implementation must wire that scaffold before acceptance.

EventJournal owner: guard append before allocating/writing, poison on failed
append, preserve last acknowledged diagnostic snapshot, refuse subsequent
mutations until drop/exclusive reopen. Reopen validates existing version policy,
repairs only the authorized torn tail, and durably establishes the loaded file
and path before resuming. Monitor must not mutate baselines/dispatch from failed
append. Durable parent creation uses shared atomic owner.

BindingTable owner: guard bind/settle before mutation and deny authority lookup
from poisoned state. Preserve conservative diagnostic rows on failures without
claiming rollback restored disk. No returned lease on failed bind; consumed
settlement lease cannot be reused. Preserve exact generation checks and lifetime
lock. Reopen loads authoritative retained disk (Active stays blocking with no
manufactured lease) and confirms durability before admitting custody. Storage
helper changes restricted to durable path establishment/read stabilization.

LogWriter owner: establish parent hierarchy via shared helper, create_new remains
exclusive, persist header/file then new pathname before returning writer. Failed
creation retains evidence, never overwrites on retry. Poison on ambiguous append
before sequence reuse; there is no append-reopen API, so do not invent one.
Best-effort observers untouched. Parent owns harness files; separate specialists
own journal/monitor and binding/storage respectively, disjoint tests. Fresh
review follows integrated concrete candidate and public-path process-scoped faults.
