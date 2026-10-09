{-# LANGUAGE CPP #-}
module RetainedSummaryBinders where
import RetainedSummaryProvider (answer)
#include "RetainedSummaryName.h"
BINDER :: Int
BINDER = answer + 1
