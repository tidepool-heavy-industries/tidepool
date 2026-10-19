import qualified Tidepool.Introspection as Introspection
detail <- Introspection.info
  (Introspection.NameQuery Introspection.CurrentScope Introspection.ValueName "readFile")
display detail
