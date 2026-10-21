use std::path::PathBuf;

#[derive(Clone, Copy)]
enum Mode {
    Build,
    Inspect,
    Entry,
}

impl Mode {
    fn parse(value: Option<&std::ffi::OsStr>) -> Result<Self, &'static str> {
        match value.and_then(std::ffi::OsStr::to_str) {
            Some("build") => Ok(Self::Build),
            Some("inspect") => Ok(Self::Inspect),
            Some("entry") => Ok(Self::Entry),
            _ => Err("usage: tidepool-module-package (build|inspect) --source ABS --target NAME [--target NAME] --source-root SNAPSHOT_ABS --output-root ABS"),
        }
    }
}

#[derive(Default)]
struct Arguments {
    source: Option<PathBuf>,
    source_root: Option<PathBuf>,
    output_root: Option<PathBuf>,
    targets: Vec<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = std::env::args_os().skip(1);
    let mode = Mode::parse(input.next().as_deref())?;
    let mut args = Arguments::default();
    while let Some(flag) = input.next() {
        let flag = flag.to_str().ok_or("non-UTF-8 argument flag")?;
        let value = input
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        let slot = match flag {
            "--source" => &mut args.source,
            "--source-root" => &mut args.source_root,
            "--output-root" => &mut args.output_root,
            "--target" => {
                args.targets
                    .push(value.into_string().map_err(|_| "non-UTF-8 target")?);
                continue;
            }
            _ => return Err(format!("unknown argument {flag}").into()),
        };
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(format!("duplicate argument {flag}").into());
        }
    }
    let source = args.source.ok_or("missing --source")?;
    let source_root = args.source_root.ok_or("missing --source-root")?;
    let output_root = args.output_root.ok_or("missing --output-root")?;
    if !source.is_absolute() || !source_root.is_absolute() || !output_root.is_absolute() {
        return Err("source, source root and output root must be absolute paths".into());
    }
    let mut scratch = tempfile::tempdir_in(std::env::current_dir()?)?;
    if matches!(mode, Mode::Inspect) {
        scratch.disable_cleanup(true);
        eprintln!(
            "catalog inspection retains scratch {} and output {}",
            scratch.path().display(),
            output_root.display()
        );
    }
    prepare_environment(mode, scratch.path())?;
    std::env::set_current_dir(scratch.path())?;
    let targets = args.targets.iter().map(String::as_str).collect::<Vec<_>>();
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        match mode {
            Mode::Build => {
                let package = tidepool_toolchain::artifacts::build_deployment_module_package(
                    &source,
                    &targets,
                    &source_root,
                    scratch.path(),
                    &output_root,
                )?;
                println!("{}", package.catalog_identity());
            }
            Mode::Inspect => {
                let report = tidepool_toolchain::artifacts::inspect_deployment_module_package(
                    &source,
                    &targets,
                    &source_root,
                    scratch.path(),
                    &output_root,
                )?;
                println!("{}", report.display());
            }
            Mode::Entry => {
                if targets != ["__prepared"] {
                    return Err("production entry requires exactly --target __prepared".into());
                }
                tidepool_toolchain::artifacts::build_production_entry(
                    &source,
                    &source_root,
                    scratch.path(),
                    &output_root,
                )?;
                println!("{}", output_root.join("entry.json").display());
            }
        }
        Ok(())
    })();
    if result.is_err() && matches!(mode, Mode::Build) {
        scratch.disable_cleanup(true);
        eprintln!(
            "native producer retains failed scratch {} and output {}",
            scratch.path().display(),
            output_root.display()
        );
    }
    result
}

fn prepare_environment(mode: Mode, scratch: &std::path::Path) -> Result<(), String> {
    // The native producer retains its declared compiler and the endpoint issued
    // by its owning frontend. Binding authenticates the endpoint before use.
    let compiler = [
        "TIDEPOOL_EXTRACT",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_COMPILER_DEPLOYMENT",
        "TIDEPOOL_GHC_LIBDIR",
        tidepool_extract_cmd::DAEMON_SOCKET_ENV,
        tidepool_extract_cmd::REQUIRED_DAEMON_ENDPOINT_ENV,
    ]
    .into_iter()
    .map(|name| {
        std::env::var_os(name)
            .map(|value| (name, value))
            .ok_or_else(|| format!("module package build requires {name}"))
    })
    .collect::<Result<Vec<_>, _>>()?;
    // This binary owns the process and has not started any threads.
    unsafe {
        let inherited = std::env::vars_os()
            .map(|(name, _)| name)
            .filter(|name| name.to_string_lossy().starts_with("TIDEPOOL_"))
            .collect::<Vec<_>>();
        for name in inherited {
            std::env::remove_var(name);
        }
        for (name, value) in compiler {
            std::env::set_var(name, value);
        }
        std::env::remove_var("GHC_PACKAGE_PATH");
        std::env::remove_var("GHCRTS");
        std::env::set_var("GHC_ENVIRONMENT", "-");
        std::env::set_var("TMPDIR", scratch);
        std::env::set_var("XDG_CACHE_HOME", scratch.join("cache"));
        if matches!(mode, Mode::Build) {
            std::env::set_var("TIDEPOOL_KEEP_TEST_LOGS", "1");
            std::env::set_var("TIDEPOOL_TEST_ARTIFACT_ROOT", scratch);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const SELECTED: [(&str, &str); 6] = [
        ("TIDEPOOL_EXTRACT", "/declared/frontend"),
        ("TIDEPOOL_EXTRACT_WORKER", "/declared/worker"),
        ("TIDEPOOL_COMPILER_DEPLOYMENT", "/declared/deployment"),
        ("TIDEPOOL_GHC_LIBDIR", "/declared/ghc"),
        (
            tidepool_extract_cmd::DAEMON_SOCKET_ENV,
            "/owned/compiler.sock",
        ),
        (
            tidepool_extract_cmd::REQUIRED_DAEMON_ENDPOINT_ENV,
            "owned-endpoint",
        ),
    ];

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "isolated subprocess exercises the native CLI environment owner"
    )]
    fn owned_endpoint_survives_sanitization_and_requires_both_coordinates() {
        for (mode, missing) in [
            ("build", None),
            ("entry", None),
            ("inspect", None),
            ("build", Some(SELECTED[4].0)),
            ("build", Some(SELECTED[5].0)),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "tests::compiler_environment_child",
                    "--ignored",
                    "--nocapture",
                ])
                .envs(SELECTED)
                .env("TIDEPOOL_COMPILER_MODULES", "/ambient/catalog")
                .env("TIDEPOOL_EXTRACT_NO_DAEMON", "1")
                .env("TIDEPOOL_CACHE_DIR", "/ambient/cache")
                .env("TIDEPOOL_TEST_ARTIFACT_ROOT", "/ambient/artifacts")
                .env("GHC_PACKAGE_PATH", "/ambient/packages")
                .env("GHCRTS", "-N8")
                .env("MODULE_COMPILER_INPUT_MODE", mode)
                .env_remove("MODULE_COMPILER_INPUT_MISSING");
            if let Some(name) = missing {
                command
                    .env_remove(name)
                    .env("MODULE_COMPILER_INPUT_MISSING", name);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        }
    }

    #[test]
    #[ignore = "private subprocess entry for process-global environment mutation"]
    fn compiler_environment_child() {
        let scratch = tempfile::tempdir().unwrap();
        let mode =
            super::Mode::parse(std::env::var_os("MODULE_COMPILER_INPUT_MODE").as_deref()).unwrap();
        if let Ok(name) = std::env::var("MODULE_COMPILER_INPUT_MISSING") {
            assert!(super::prepare_environment(mode, scratch.path())
                .unwrap_err()
                .contains(&name));
            return;
        }
        super::prepare_environment(mode, scratch.path()).unwrap();
        for (name, value) in SELECTED {
            assert_eq!(std::env::var(name).unwrap(), value);
        }
        for name in [
            "TIDEPOOL_COMPILER_MODULES",
            "TIDEPOOL_EXTRACT_NO_DAEMON",
            "TIDEPOOL_CACHE_DIR",
            "GHC_PACKAGE_PATH",
            "GHCRTS",
        ] {
            assert!(std::env::var_os(name).is_none(), "ambient {name} survived");
        }
        assert_eq!(std::env::var("GHC_ENVIRONMENT").unwrap(), "-");
        if matches!(mode, super::Mode::Build) {
            assert_eq!(std::env::var("TIDEPOOL_KEEP_TEST_LOGS").unwrap(), "1");
            assert_eq!(
                std::path::PathBuf::from(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").unwrap()),
                scratch.path()
            );
        } else {
            assert!(std::env::var_os("TIDEPOOL_KEEP_TEST_LOGS").is_none());
            assert!(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").is_none());
        }
        assert_eq!(
            std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap()),
            scratch.path()
        );
    }
}
