settlementReady <- watch "lookup-acceptance-settled" (awaitSettled worker)
settlementReady
