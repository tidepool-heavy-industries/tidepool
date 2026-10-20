data CapturedGreeting = CapturedGreeting Aeson.Value
capturedGreeting :: () -> CapturedGreeting
capturedGreeting () = CapturedGreeting input
