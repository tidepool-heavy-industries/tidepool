do
  _ <- Scope.withScope $ \scope -> do
    _ <- admitScoped scope "scope-cancelled-child"
    sleep (minutes 15)
    Effects.error "cancelled scope body resumed" >> pure ()
  Effects.error "cancelled invocation returned a scope result" >> pure False
