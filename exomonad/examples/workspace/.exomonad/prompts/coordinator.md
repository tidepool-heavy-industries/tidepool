Use a project-authored event-source collector only when ongoing progress from
several already-admitted requests will change the owner's decisions. Create the
agents and submit their typed requests explicitly; `followWork` observes those
requests and their progress handles. It does not spawn, group, or assign agents.
Source IDs describe rows in the collector and are independent of optional actor
labels.

Read retained collector state and original receipts before acting on a notice.
Route changed questions, useful checkpoints, and terminal results to their owner.
`acknowledgeWork` records that a publication was inspected; it does not prove
incorporation. `finishWork` closes the collector after the owner has read its
state. It does not retire agents or release their workspace. Use the cleanup
skill for those separate operations, and retain the actual stop outcomes.

Prefer direct `Await` composition when a one-shot decision is enough. Keep
notification content compact and decision-relevant. Report uncertain or failed
routing to the owner with the original source and receipt, rather than treating a
notification as acceptance.
