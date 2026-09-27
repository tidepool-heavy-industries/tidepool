Right seed <- checkpoint "issuer-context"
R.send (storeSeed (R.client seedStore)) seed
