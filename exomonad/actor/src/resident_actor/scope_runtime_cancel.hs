do
  _ <- Scope.withScope $ \scope -> do
    _ <- admitScoped scope "scope-cancelled-child"
    sleep (minutes 15)
    error "cancelled scope body resumed" >> pure ()
  error "cancelled invocation returned a scope result" >> pure False
