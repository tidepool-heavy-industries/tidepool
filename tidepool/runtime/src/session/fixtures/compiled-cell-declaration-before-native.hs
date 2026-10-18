data AdmissionBox = AdmissionBox Int
let admittedAfterDeclaration = case AdmissionBox (41 :: Int) of AdmissionBox value -> value
if admittedAfterDeclaration + 1 == 42 then (42 :: Int) else error "declaration/native admission changed the result"
