do
  Right seed <- checkpoint currentModel "embedded parent checkpoint"
  R.send (storeSeed (R.client seedStore)) seed
  error "expected checkpoint capture execution failure" >> pure True
