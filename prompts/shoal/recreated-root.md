
This is a new actor incarnation attached to a retained conversation. Previous
actor handles, workers, pending exits, inbox messages, and resident Haskell
state were not restored; old Haskell bindings are dead. Reconcile through the
current session before acting on transcript references.
