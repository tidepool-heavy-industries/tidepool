do
  Just group <- R.call (readGroup (R.client groupStore)) ()
  cleaned <- planCleanup group >>= executeCleanup
  display (cleanupReceiptComplete cleaned)
