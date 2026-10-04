//! Export the production generated Haskell surface for declared build consumers.
use std::{collections::BTreeSet, path::PathBuf, process::ExitCode};
fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut expected = BTreeSet::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output-root" if root.is_none() => root = args.next().map(PathBuf::from),
            "--expect-output" => match args.next() {
                Some(path) => {
                    expected.insert(path);
                }
                None => return ExitCode::FAILURE,
            },
            _ => return ExitCode::FAILURE,
        }
    }
    let Some(root) = root else {
        eprintln!("usage: tidepool-effects-gen --output-root DIRECTORY");
        return ExitCode::FAILURE;
    };
    let files = [
        (
            "Tidepool/Effects/Core.hs",
            tidepool_mcp::effects_core_module_source(),
        ),
        (
            "Tidepool/Effects/Authored.hs",
            tidepool_mcp::effects_authored_module_source(),
        ),
        (
            "Tidepool/Effects.hs",
            tidepool_mcp::effects_shim_module_source(
                &tidepool_mcp::all_decls(),
                &tidepool_mcp::RowArgs::default(),
            ),
        ),
    ];
    if !expected.is_empty()
        && files
            .iter()
            .map(|(path, _)| path.to_string())
            .collect::<BTreeSet<_>>()
            != expected
    {
        eprintln!("declared effects output roster differs from the production surface");
        return ExitCode::FAILURE;
    }
    for (relative, contents) in files {
        let path = root.join(relative);
        if let Err(error) = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(&path, contents))
        {
            eprintln!("could not materialize {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
