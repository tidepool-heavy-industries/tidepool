---
name: exomonad-cleanup
description: Retire hosted agents deliberately and inspect retained cleanup outcomes. Load when an agent is finished or when teardown reports retained resources.
---

Retire an agent when its request work is settled and its result has been reviewed.
Settlement, closing an observer, stopping an actor, and releasing host resources
are distinct outcomes. Keep useful agents available while named work remains.
Before stopping one, retain its typed result, exact source identity, relevant
progress and cleanup evidence. A dirty worktree remains available after its
actor stops; retirement does not delete source, branches, commits, or user files.

Read the current status and retained receipts before acting. New requests,
children, or resource changes can make an earlier cleanup observation stale.
Treat refusal and partial cleanup as typed outcomes with their own next action;
do not repeat an uncertain effect without its receipt. A pending release is a
pending fact: continue independent work and read the eventual notice. If a
resource stays retained, report its identity, owner, reason, and required
repository or host action.

For a parent with work in progress, continue after retiring an unrelated child.
Do not use a shared group name as actor identity or cleanup authority. Actor
identity, request ownership, workspace attachment, and filesystem retention are
separate facts. Consult the live cleanup declarations and `doc cleanup` for the
specific operations available in this runtime.
