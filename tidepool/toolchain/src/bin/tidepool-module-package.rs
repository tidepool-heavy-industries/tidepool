use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 5
        || arguments[0] != "build"
        || arguments[1] != "--source-root"
        || arguments[3] != "--output-root"
    {
        return Err(
            "usage: tidepool-module-package build --source-root ABS --output-root ABS".into(),
        );
    }
    let source_root = PathBuf::from(&arguments[2]);
    let output_root = PathBuf::from(&arguments[4]);
    if !source_root.is_absolute() || !output_root.is_absolute() {
        return Err("source and output roots must be absolute final deployment paths".into());
    }
    let package =
        tidepool_toolchain::artifacts::build_deployment_module_package(&source_root, &output_root)?;
    println!("{}", package.catalog_identity());
    Ok(())
}
