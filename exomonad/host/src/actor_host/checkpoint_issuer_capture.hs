Right seed <- checkpoint currentModel "issuer-context"
R.send (storeSeed (R.client seedStore)) seed
