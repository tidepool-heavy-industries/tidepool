//! The standalone configuration checker uses its embedded library and performs
//! no actor/provider startup, even when invoked from a development checkout.
use std::fs;
use std::process::Command;

#[test]
fn check_uses_embedded_sources_and_rejects_invalid_workspace_modules() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("app");
    let authored = workspace.join(".shoal");
    fs::create_dir_all(authored.join("Project")).unwrap();
    fs::write(
        authored.join("config.toml"),
        "[defaults]\nmodel = 'gpt-5.6-sol'\neffort = 'low'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Contract']\n",
    )
    .unwrap();
    fs::write(
        authored.join("Project/Contract.hs"),
        "module Project.Contract where\ndata Contract = Checked\n",
    )
    .unwrap();
    let development = root.path().join("development");
    fs::create_dir_all(development.join("haskell/lib/Tidepool")).unwrap();
    fs::write(
        development.join("haskell/lib/Tidepool/Prelude.hs"),
        "this development library must never enter the frozen Shoal program\n",
    )
    .unwrap();
    let check = |override_library: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shoal"));
        command
            .current_dir(&development)
            .args(["check", "--workspace"])
            .arg(&workspace)
            .env(
                "TIDEPOOL_INTERACTIVE_CODEX_BIN",
                root.path().join("no-provider"),
            );
        if override_library {
            command.env("TIDEPOOL_PRELUDE_DIR", root.path().join("missing-library"));
        } else {
            command.env_remove("TIDEPOOL_PRELUDE_DIR");
        }
        command.output().unwrap()
    };
    for override_library in [false, true] {
        let output = check(override_library);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("Workspace definitions compile:"));
    }
    fs::write(
        authored.join("Project/Contract.hs"),
        "module Project.Contract where\nvalue :: Int\nvalue = True\n",
    )
    .unwrap();
    let rejected = check(false);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("Contract.hs"));
    for runtime in ["sessions", "runtime", "logs", "build"] {
        assert!(!authored.join(runtime).exists(), "check created {runtime}");
    }
}
