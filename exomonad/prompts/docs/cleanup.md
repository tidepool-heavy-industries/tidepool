Agent retirement and workspace cleanup are separate. A settled request does not
stop its actor; stopping an actor does not delete a worktree or shared directory.
Before retirement, retain the result, source identity, relevant progress, and any
cleanup evidence. Read current status because accepted work or a new attachment
may have changed since the last observation.

A refusal or partial outcome is a typed fact with its own next step. Do not retry
an uncertain effect without reading its receipt. A release that is still pending
will report its eventual outcome; continue independent work until that notice
arrives. If resources remain retained, report their exact identities, owner,
reason, and required host or repository action.

Workspace backing and actor attachments are independent. Stopping one actor
must not retire sibling attachments or delete shared files. Dirty work remains
available after actor retirement. Cleanup never removes commits, branches, build
evidence, or user files.

Load `exomonad-cleanup` for the current lifecycle operations and their retained
outcomes.
