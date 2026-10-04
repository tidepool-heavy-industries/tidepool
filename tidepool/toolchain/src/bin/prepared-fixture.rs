//! Declared immutable fixture compilation through the production artifact owner.
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Default)]
struct Arguments {
    source: Option<PathBuf>,
    output: Option<PathBuf>,
    frontend: Option<PathBuf>,
    worker: Option<PathBuf>,
    deployment: Option<PathBuf>,
    ghc_libdir: Option<PathBuf>,
    runtime_libraries: Option<PathBuf>,
    targets: Vec<String>,
    include: Vec<PathBuf>,
}

fn arguments() -> Result<Arguments, String> {
    let mut arguments = Arguments::default();
    let mut input = std::env::args_os().skip(1);
    while let Some(flag) = input.next() {
        let flag = flag.to_str().ok_or("non-UTF-8 argument flag")?;
        let value = input
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        let slot = match flag {
            "--source" => &mut arguments.source,
            "--output" => &mut arguments.output,
            "--frontend" => &mut arguments.frontend,
            "--worker" => &mut arguments.worker,
            "--deployment" => &mut arguments.deployment,
            "--ghc-libdir" => &mut arguments.ghc_libdir,
            "--runtime-libraries" => &mut arguments.runtime_libraries,
            "--include" => {
                arguments.include.push(PathBuf::from(value));
                continue;
            }
            "--target" => {
                arguments
                    .targets
                    .push(value.into_string().map_err(|_| "non-UTF-8 target")?);
                continue;
            }
            _ => return Err(format!("unknown argument {flag}")),
        };
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(format!("duplicate argument {flag}"));
        }
    }
    Ok(arguments)
}

fn required(path: Option<PathBuf>, name: &str) -> Result<PathBuf, String> {
    let path = path.ok_or_else(|| format!("missing {name}"))?;
    std::fs::canonicalize(&path).map_err(|error| format!("{}: {error}", path.display()))
}

fn run() -> Result<(), String> {
    let args = arguments()?;
    let source = required(args.source, "--source")?;
    let frontend = required(args.frontend, "--frontend")?;
    let worker = required(args.worker, "--worker")?;
    let deployment = required(args.deployment, "--deployment")?;
    let ghc_libdir = required(args.ghc_libdir, "--ghc-libdir")?;
    if !ghc_libdir.starts_with("/nix/store") {
        return Err("--ghc-libdir must select the pinned Nix compiler package".into());
    }
    let libraries = required(args.runtime_libraries, "--runtime-libraries")?;
    let output = args.output.ok_or("missing --output")?;
    let output = if output.is_absolute() {
        output
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(output)
    };
    let include = args
        .include
        .into_iter()
        .map(|path| required(Some(path), "--include"))
        .collect::<Result<Vec<_>, _>>()?;
    let scratch = tempfile::tempdir_in(std::env::current_dir().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    // This process owns no concurrent work. Discard inherited runtime input
    // selection and contain compiler/package caches inside its action scratch.
    unsafe {
        let inherited = std::env::vars_os()
            .map(|(name, _)| name)
            .filter(|name| name.to_string_lossy().starts_with("TIDEPOOL_"))
            .collect::<Vec<_>>();
        for name in inherited {
            std::env::remove_var(name);
        }
        std::env::remove_var("GHC_PACKAGE_PATH");
        std::env::remove_var("GHCRTS");
        std::env::set_var("GHC_ENVIRONMENT", "-");
        std::env::set_var("TIDEPOOL_EXTRACT", frontend);
        std::env::set_var("TIDEPOOL_EXTRACT_WORKER", worker);
        std::env::set_var("TIDEPOOL_COMPILER_DEPLOYMENT", deployment);
        std::env::set_var("TIDEPOOL_GHC_LIBDIR", ghc_libdir);
        std::env::set_var("LD_LIBRARY_PATH", libraries);
        std::env::set_var("TMPDIR", scratch.path());
        std::env::set_var("XDG_CACHE_HOME", scratch.path().join("cache"));
        std::env::set_var(
            "TIDEPOOL_COMPILE_CACHE_DIR",
            scratch.path().join("unused-runtime-cache"),
        );
        std::env::set_var(
            "TIDEPOOL_BUILD_PRODUCTS_DIR",
            scratch.path().join("build-products"),
        );
    }
    std::env::set_current_dir(scratch.path()).map_err(|error| error.to_string())?;
    let targets = args.targets.iter().map(String::as_str).collect::<Vec<_>>();
    tidepool_toolchain::artifacts::build_prepared_fixture(
        &source,
        &targets,
        &include,
        scratch.path(),
        &output,
    )
    .map_err(|error| error.to_string())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("prepared fixture compilation failed: {error}");
            ExitCode::FAILURE
        }
    }
}
