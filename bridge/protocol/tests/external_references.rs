use tidepool_protocol::{
    effects,
    schema::{ExternalType, RustBinding},
    HsType,
};

#[test]
fn request_site_reference_owns_the_leaf_import_and_rust_binding() {
    for effect in [
        effects::actor_local::actor_local(),
        effects::agent_session::agent_session(),
    ] {
        effect.validate().unwrap();
        let reference = effect
            .external_types
            .iter()
            .find(|reference| reference.haskell_name == "RequestSite")
            .unwrap();
        assert_eq!(reference.rust_wire, "i64");
        assert_eq!(
            reference.core_import().as_deref(),
            Some("import Tidepool.Internal.RequestSite (RequestSite)")
        );
        let site = effect
            .verbs
            .iter()
            .flat_map(|verb| &verb.args)
            .find(|arg| arg.name == "site")
            .unwrap();
        assert_eq!(site.rust, RustBinding::External);
        assert_eq!(site.rust.rust_type(&site.ty, "site", &effect), "i64");
        assert_eq!(
            RustBinding::External.rust_type(
                &HsType::maybe(HsType::Named("RequestSite")),
                "site",
                &effect
            ),
            "Option<i64>"
        );
        assert!(!effect
            .extra_imports
            .iter()
            .any(|import| import.contains("RequestSite")));
    }
}
#[test]
fn core_leaf_import_refuses_a_self_cycle_after_valid_schema_control() {
    let mut effect = effects::sleep::sleep();
    effect.validate().unwrap();
    effect.external_types = &[ExternalType {
        haskell_name: "Duration",
        rust_wire: "crate::request_effect::RequestDuration",
        core_module: Some("Tidepool.Effects.Core"),
    }];
    assert!(effect.validate().is_err());
}
