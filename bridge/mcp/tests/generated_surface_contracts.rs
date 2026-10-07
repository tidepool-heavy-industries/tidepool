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
    let surface = Surface::new();
    let compiled = surface.compile("ModelCodec.hs", true);
    assert!(
        compiled.status.success(),
        "model codec compilation: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new(surface.scratch.path().join("codec"))
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "model codecs: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
}

#[test]
fn compiled_public_actor_rows_match_generated_rust_order() {
    use exomonad_tool::PublicActorEffectRow;

    let surface = Surface::new();
    let compiled = surface.compile("ActorProfiles.hs", true);
    assert!(
        compiled.status.success(),
        "public actor row compilation: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new(surface.scratch.path().join("codec"))
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "public actor row reflection: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let reflected: Vec<Vec<String>> = serde_json::from_slice(&executed.stdout)
        .expect("the compiled fixture must emit every named row as a string array");
    let expected: Vec<Vec<String>> = PublicActorEffectRow::ALL
        .into_iter()
        .map(|row| {
            std::iter::once(row.haskell_alias().to_owned())
                .chain(
                    row.effect_keys()
                        .iter()
                        .map(|key| format!("Effect{}", key.haskell_name())),
                )
                .collect()
        })
        .collect();
    assert_eq!(
        reflected, expected,
        "compiled KnownEffects must preserve each public alias's exact effect order"
    );
}
