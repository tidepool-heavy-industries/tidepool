replaceActor collector (stateful "missing-sources" ReadOnly (\values (value, reply) -> pure (reply, value : values)) :: ActorDefinition [Int] ((,) Int) [Int])
