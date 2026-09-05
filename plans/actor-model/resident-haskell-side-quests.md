# Other useful uses of resident Haskell

These are experiments to try in a campaign, not a proposal to ship another DSL.
The model should discover the useful types and helpers from the task.

1. **Keep an executable inventory of an investigation.** Store request traces,
   usage observations, thread identities, and candidate explanations as resident
   values. Write task-specific comparisons once, then apply them to each new
   canary. This cache investigation repeatedly needed exactly that operation.

2. **Make bulk edits inspectable before applying them.** A model-defined edit
   record can carry a path, expected old text or digest, replacement, and reason.
   Construct and inspect a collection of edits with ordinary Haskell, validate
   all preconditions, then apply it through the existing filesystem owner.
   Resident values remove repeated quoting and serialization between planning,
   previewing, applying, and checking an edit. Bash remains excellent for short
   commands; the advantage appears when a script accumulates state and invariants.

3. **Turn Git observations into campaign evidence.** Keep commit IDs, paths,
   review findings, and test outcomes in a task-specific sum type. Fold that
   evidence to decide what to merge or revisit, then execute ordinary Git.
   This should complement Git's own custody and conflict machinery.

4. **Explore a hypothesis tree cheaply.** After an expensive diagnosis, fork
   context-identical children to challenge distinct explanations. Give them
   typed inputs containing closures over the exact observations already loaded.
   Fold their evidence, update the hypothesis type if useful, and fork the next
   wave. No repeated prose handoff and no predeclared universal research schema.

5. **Grow tiny instruments while working.** A one-off decoder, renderer, search
   predicate, or artifact comparison can become a resident helper used over
   several turns and inherited by children. Promote it into repository code only
   when a real second campaign demonstrates stable utility.

The useful invariant to bake in is the repeated scaffold → unfold → fold wave.
The editing vocabulary, investigation types, and reporting conventions should
remain emergent. A good trial is whether the model voluntarily retains and
reuses a helper because it makes the next step easier.
