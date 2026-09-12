let tools () =
      InspectionTools
        { inspectApi =
            tool "Inspect the live resident API." $ \_ -> do
              local <- API.info (API.inNamespace API.TypeName (API.here "InspectOutput"))
              function <- API.typeOf (API.inNamespace API.ValueName (API.here "serveToolsWith"))
              public <- API.info (API.inNamespace API.TypeName (API.inModule "Tidepool.Actor" "ActorDefinition"))
              unknown <- API.info (API.inNamespace API.ValueName (API.here "noSuchResidentName"))
              ambiguous <- API.info (API.here "InspectOutput")
              constructor <- API.info (API.inNamespace API.ConstructorName (API.here "InspectOutput"))
              unknownModule <- API.info (API.inNamespace API.TypeName (API.inModule "No.Such.Module" "Missing"))
              let (name, fields, generation, fingerprint) = case local of
                    Right details ->
                      ( API.identifierName (API.inspectedIdentifier details)
                      , concatMap (map API.fieldName . API.recordFields) (API.constructors details)
                      , API.provenanceGeneration (API.identifierProvenance details)
                      , API.provenanceFingerprint (API.identifierProvenance details)
                      )
                    Left _ -> ("local-error", [], -1, "")
                  signature = case function of
                    Right details -> API.typeCanonical (API.typeExpression details)
                    Left _ -> "type-error"
                  publicName = case public of
                    Right details -> API.identifierName (API.inspectedIdentifier details)
                    Left _ -> "public-error"
                  unknownTyped = case unknown of
                    Left (API.UnknownIdentifier _) -> True
                    _ -> False
                  ambiguousTyped = case ambiguous of
                    Left (API.AmbiguousIdentifier _ candidates) -> length candidates == 2
                    _ -> False
                  namespaceTyped = case constructor of
                    Right details -> API.identifierNamespace (API.inspectedIdentifier details) == API.ConstructorIdentifier
                    _ -> False
                  unknownModuleTyped = case unknownModule of
                    Left (API.UnknownModule "No.Such.Module") -> True
                    _ -> False
              pure (InspectOutput name fields signature publicName unknownTyped ambiguousTyped namespaceTyped unknownModuleTyped generation fingerprint)
        , finishInspection =
            finishTool "Finish the inspection actor." $ \_ ->
              pure (FinishOutput True, ())
        }
in serveToolsWith () tools
