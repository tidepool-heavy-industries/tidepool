use std::path::PathBuf;

#[derive(Clone, Copy)]
enum Mode {
    Build,
    Inspect,
}

impl Mode {
    fn parse(value: Option<&std::ffi::OsStr>) -> Result<Self, &'static str> {
        match value.and_then(std::ffi::OsStr::to_str) {
            Some("build") => Ok(Self::Build),
            Some("inspect") => Ok(Self::Inspect),
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
    // These four inputs are the calling build action's declared compiler pair
    // and package closure. No inherited resident or cache selection survives.
    let compiler = [
        "TIDEPOOL_EXTRACT",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_COMPILER_DEPLOYMENT",
        "TIDEPOOL_GHC_LIBDIR",
    ]
    .into_iter()
    .map(|name| {
        std::env::var_os(name)
            .map(|value| (name, value))
            .ok_or_else(|| format!("module package build requires {name}"))
    })
    .collect::<Result<Vec<_>, _>>()?;
    let mut scratch = tempfile::tempdir_in(std::env::current_dir()?)?;
    if matches!(mode, Mode::Inspect) {
        scratch.disable_cleanup(true);
        eprintln!(
            "catalog inspection retains scratch {} and output {}",
            scratch.path().display(),
            output_root.display()
        );
    }
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
        std::env::set_var("TMPDIR", scratch.path());
        std::env::set_var("XDG_CACHE_HOME", scratch.path().join("cache"));
    }
    std::env::set_current_dir(scratch.path())?;
    let targets = args.targets.iter().map(String::as_str).collect::<Vec<_>>();
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
    }
    Ok(())
}
