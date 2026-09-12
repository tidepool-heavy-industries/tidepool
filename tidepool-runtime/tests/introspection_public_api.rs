use tidepool_testing::eval_harness::EvalHarness;

#[test]
fn public_introspection_helpers_and_structured_projection_compile() {
    tidepool_testing::eval_harness::require_extract();
    let mut declarations = tidepool_mcp::standard_decls();
    declarations.push(tidepool_mcp::introspection_decl());
    let effects = tidepool_mcp::ensure_effects_module(&declarations)
        .expect("write Introspection effect module");
    let source = r#"
{-# LANGUAGE DataKinds, FlexibleContexts, NoImplicitPrelude, OverloadedStrings #-}
module IntrospectionPublicApi where
import Tidepool.Prelude
import qualified Tidepool.Introspection as API

current :: API.NameQuery
current = API.here "WorkProgress"

explicit :: API.NameQuery
explicit = API.inNamespace API.TypeName
  (API.inModule "Project.Types" "WorkProgress")

project :: API.IdentifierInfo -> ([Text], [Text])
project details =
  ( API.identifierName . API.constructorRef <$> API.constructors details
  , concatMap (map API.fieldName . API.recordFields) (API.constructors details)
  )
"#;
    if let Err(error) = EvalHarness::new()
        .with_stdlib()
        .with_includes(effects.include_paths())
        .compile_many(source, &["current", "explicit", "project"])
    {
        panic!("public Introspection module failed: {error:?}");
    }
}
