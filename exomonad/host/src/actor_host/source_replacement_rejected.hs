replaceActor collector (stateful "missing-sources" (Selected (knownEffects @'[])) (\values (value, reply) -> pure (reply, value : values)) :: ActorDefinition [Int] ((,) Int) [Int])
