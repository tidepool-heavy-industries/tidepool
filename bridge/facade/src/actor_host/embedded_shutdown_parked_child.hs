_ <- Actor.awaitExit (R.actorRef shutdownGate)
error "HOSTED_SHUTDOWN_GATE_EXITED_BEFORE_NATIVE_CANCELLATION" >> respond capturedGetter
