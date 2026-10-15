//! Compile the production Core/Authored modules and real companion consumers.
use std::{path::PathBuf, process::Command};

struct Surface {
    scratch: tempfile::TempDir,
    roots: Vec<PathBuf>,
    fixtures: PathBuf,
}
impl Surface {
    fn new() -> Self {
        let required = |name| {
            PathBuf::from(
                std::env::var_os(name)
                    .unwrap_or_else(|| panic!("{name} must name a declared build resource")),
            )
        };
        Self {
            scratch: tempfile::tempdir().unwrap(),
            roots: vec![
                required("TIDEPOOL_EFFECTS_SOURCE_ROOT"),
                required("TIDEPOOL_PROTOCOL_HASKELL_ROOT"),
                required("TIDEPOOL_PRELUDE_DIR"),
                required("TIDEPOOL_HASKELL_ACTORS_DIR"),
            ],
            fixtures: required("TIDEPOOL_GENERATED_SURFACE_FIXTURES"),
        }
    }
    fn compile(&self, fixture: &str, link: bool) -> std::process::Output {
        let mut command = Command::new(
            std::env::var_os("TIDEPOOL_GHC")
                .expect("TIDEPOOL_GHC must name the declared pinned compiler"),
        );
        command
            .arg("-v0")
            .arg("-fforce-recomp")
            .arg("-XGHC2024")
            .arg("-XOverloadedStrings")
            .arg("-outputdir")
            .arg(self.scratch.path());
        for root in &self.roots {
            command.arg(format!("-i{}", root.display()));
        }
        if link {
            command.arg("-o").arg(self.scratch.path().join("codec"));
        } else {
            command.arg("-fno-code");
        }
        command.arg(self.fixtures.join(fixture)).output().unwrap()
    }
    fn execute(&self, fixture: &str) -> std::process::Output {
        let compiled = self.compile(fixture, true);
        assert!(
            compiled.status.success(),
            "{fixture} compilation: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = Command::new(self.scratch.path().join("codec"))
            .output()
            .unwrap();
        assert!(
            executed.status.success(),
            "{fixture}: {}",
            String::from_utf8_lossy(&executed.stderr)
        );
        executed
    }
    fn control(&self) {
        let compiled = self.compile("SurfaceConsumer.hs", false);
        assert!(
            compiled.status.success(),
            "compiled Core/Authored/spec consumer: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
    }
}
#[test]
fn core_authored_and_spec_consumers_compile() {
    Surface::new().control();
}
#[test]
fn authored_private_constructor_and_forged_request_site_are_refused_after_valid_control() {
    let surface = Surface::new();
    surface.control();
    for fixture in [
        "PrivateConstructor.hs",
        "ForgedEffectWitness.hs",
        "ForgedRequestSite.hs",
        "ForgedWorkspaceHandle.hs",
        "ForgedContextCheckpoint.hs",
        "ForgedAuthoredWorkspaceHandle.hs",
        "ForgedAuthoredScope.hs",
        "ForgedScope.hs",
        "ForgedAgentRef.hs",
    ] {
        let refused = surface.compile(fixture, false);
        assert!(
            !refused.status.success(),
            "{fixture} compiled after a valid control"
        );
        assert!(
            !refused.stderr.is_empty(),
            "{fixture} refusal has no compiler diagnostics"
        );
    }
}
#[test]
fn generated_model_control_codecs_execute_and_preserve_callback_value() {
    Surface::new().execute("ModelCodec.hs");
}

#[test]
fn compiled_default_actor_row_matches_generated_rust_order() {
    use exomonad_tool::{ActorEffectKey, ToolEffectKey, DEFAULT_ACTOR_EFFECTS};

    let surface = Surface::new();
    let executed = surface.execute("ActorProfiles.hs");
    let reflected: Vec<Vec<String>> = serde_json::from_slice(&executed.stdout)
        .expect("the compiled fixture must emit the default row as a string array");
    let named = |label: &str, values: Vec<String>| {
        std::iter::once(label.to_owned())
            .chain(values)
            .collect::<Vec<_>>()
    };
    let actors = ActorEffectKey::ALL;
    let tools = ToolEffectKey::ALL;
    let nominal = |key: ToolEffectKey| {
        let identity = key
            .nominal_identity()
            .expect("active catalogue keys have nominal owners");
        format!("{}.{}", identity.module, identity.name)
    };
    let expected = vec![
        named(
            "ActorEffects",
            DEFAULT_ACTOR_EFFECTS
                .iter()
                .map(|key| key.haskell_constructor().unwrap().to_owned())
                .collect(),
        ),
        named(
            "CatalogueConstructors",
            actors
                .iter()
                .map(|key| key.haskell_constructor().unwrap().to_owned())
                .collect(),
        ),
        named(
            "CatalogueEffects",
            actors
                .iter()
                .map(|key| key.wire_name().to_owned())
                .collect(),
        ),
        named(
            "Nominals",
            actors
                .iter()
                .copied()
                .map(ToolEffectKey::Actor)
                .map(nominal)
                .collect(),
        ),
        named(
            "ToolEffects",
            tools.iter().map(|key| key.wire_name().to_owned()).collect(),
        ),
        named("ToolNominals", tools.iter().copied().map(nominal).collect()),
        named(
            "SyncEffects",
            tools
                .iter()
                .filter(|key| key.is_invocation_only())
                .map(|key| key.wire_name().to_owned())
                .chain(
                    DEFAULT_ACTOR_EFFECTS
                        .iter()
                        .map(|key| key.haskell_constructor().unwrap().to_owned()),
                )
                .collect(),
        ),
    ];
    assert_eq!(
        reflected, expected,
        "compiled witnesses, defining TyCons, names and ordered profiles must agree with the nominal catalogue"
    );
}

#[test]
fn generated_and_handwritten_substrate_helpers_preserve_semantics() {
    Surface::new().execute("HelperSemantics.hs");
}

#[test]
fn synchronous_reload_profile_and_async_source_inspection_compile() {
    let surface = Surface::new();
    let compiled = surface.compile("ReloadSync.hs", false);
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let refused = surface.compile("ReloadAsync.hs", false);
    assert!(
        !refused.status.success(),
        "asynchronous Reload profile compiled"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("Reload cannot be used by an asynchronous"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

#[test]
fn reload_defaults_and_structured_receipts_execute() {
    Surface::new().execute("ReloadCodec.hs");
}
