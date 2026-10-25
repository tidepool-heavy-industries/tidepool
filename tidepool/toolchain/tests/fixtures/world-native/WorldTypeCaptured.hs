data Wrapped = Wrapped Int
{-# NOINLINE captured #-}
captured () = Wrapped (seq TidepoolHostInput.supplied 37)
let supplied = captured ()
