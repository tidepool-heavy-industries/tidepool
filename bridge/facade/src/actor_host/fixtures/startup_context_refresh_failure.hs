do
  startupCurrent <- C.getContext
  C.putContext (over C.contextBlocks (map (startupReplace "startup-must-not-publish")) startupCurrent)
  error "startup refresh failure control" >> pure True
