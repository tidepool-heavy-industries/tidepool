# Actor journal owner protocol

One dedicated OS thread owns the journal path and plain mutable records,
sequence, and permanent uncertainty fence. Typed commands are admitted by
successful channel send; the owner validates, appends with full durability, and
applies each command in channel order. Callers await a oneshot reply. Dropping a
reply receiver does not cancel an admitted command. No public callback can run
inside the journal owner.

Queries return only the selected immutable facts. An explicitly requested full
records inventory copies the inventory once; ordinary reads and updates never
clone the journal state. Durable successor certification captures only its exact
predecessor and successor records after actual durable readback. Its native
metadata validators inspect those immutable facts without accessing live state.
The native owner still decides whether that certified transition is current.

An uncertain append permanently fences this owner and is exposed through its
status observation. All later commands requiring authority fail. The dedicated
thread publishes termination (including panic) through a retained watch status;
reply-channel loss is an owner-unavailable error, never successful completion.
The thread drains admitted commands after the final sender disappears, then
reports clean termination. Open/replay and filesystem observer functions remain
explicit blocking interfaces for startup and offline inspection. Instance
operations are asynchronous and have no blocking forwarding compatibility API.

The serialized version and rows remain unchanged. Recovery requires reopening
and replaying the real journal; no attempt resets a live uncertainty fence.
