# Actor journal owner protocol

One dedicated OS thread owns the journal path and plain mutable records,
sequence, and permanent uncertainty fence. Typed commands are admitted by
successful channel send; the owner validates, appends with full durability, and
applies each command in channel order. Callers await a oneshot reply. Dropping a
reply receiver does not cancel an admitted command. No public callback can run
inside the journal owner.

Queries return only the selected immutable facts. An explicitly requested full
records inventory copies the inventory once; ordinary reads and updates never
clone the journal state. Full inventory has explicit linear copying cost. Durable successor certification captures only the exact owner,
backend conversation and manifest facts after actual durable readback. Its native
metadata validators inspect those immutable facts without accessing live state.
The lifecycle owner must retain an execution claim spanning certification and
native use. Native predecessor currentness alone cannot authorize a successor
that the lifecycle owner has since retired or superseded. Certification is
immutable evidence and cannot independently issue this currentness claim.

An uncertain append permanently fences this owner and is exposed through its
status observation. All later commands requiring authority fail. The dedicated
thread publishes termination (including panic) through a retained watch status;
reply-channel loss is an owner-unavailable error, never successful completion.
The thread drains admitted commands after the final sender disappears, then
reports clean termination. Open/replay, creation and subsequent instance operations run on the same
dedicated thread; callers asynchronously await initialization and command
completion. Only the offline filesystem observer remains explicitly blocking.
No blocking forwarding compatibility API is retained.

The serialized version and rows remain unchanged. Recovery requires reopening
and replaying the real journal; no attempt resets a live uncertainty fence.

Binding commands return a non-cloneable JournalApplicationBindingAck naming the
exact actor, conversation, issuing journal and committed prefix. This ack only
certifies journal evidence. The verified enclosing facade bind composition must
confirm the native backend binding under the same lifecycle claim before issuing
a distinct final settlement receipt; appending a journal row cannot issue it.

Startup admission requires the sealed RunControl-issued RootStartupAdmission
permit for the exact actor, predecessor option and journal endpoint. A live
predecessor carries an owner-issued drain result; initial and recovered issuance
are distinct. The permit remains held through journal acknowledgement. The root
owner closes prior incarnation admission and settles its pending successor claim
before admitting the next incarnation. Ordinary nonstartup admission is separate.

Opening requires an opaque retained enclosing lifetime owner. The dedicated
thread holds that existing run lease through bootstrap, queued writes and exit,
including when the opening waiter disappears. Standalone test journals retain
their temporary-directory owner. This keepalive does not issue authority and
adds no lock or registry; facade callers supply their existing lifetime owner.

Boot recovery consumes a trusted host adapter before exposing the writer. The
writer validates the actual exclusive run authority, freezes only initial actor
membership, and issues a non-cloneable issuer-bound JournalBootRegistration.
RunControl consumes it once before reservations; later current-host appends never
enter that membership. Actor and host incarnation numbers are independent.

Actor lineage tests establish journal facts and authenticated admission. Complete
native owner transfer, actual Store binding, final claim settlement and admission
of the next successor require facade acceptance through its real bind adapter.
