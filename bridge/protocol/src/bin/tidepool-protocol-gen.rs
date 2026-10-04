//! Materialize the schema under an explicitly supplied output root.
use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut list = false;
    let mut expected = std::collections::BTreeSet::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output-root" if root.is_none() => root = args.next().map(PathBuf::from),
            "--list" => list = true,
            "--expect-output" => match args.next() {
                Some(path) => {
                    expected.insert(path);
                }
                None => return ExitCode::FAILURE,
            },
            _ => {
                eprintln!("usage: tidepool-protocol-gen --output-root DIRECTORY | --list");
                return ExitCode::FAILURE;
            }
        }
    }
    let files = tidepool_protocol::generated_files()
        .into_iter()
        .chain(tidepool_protocol::runtime_generated_files())
        .chain(tidepool_protocol::actor_generated_files())
        .chain(tidepool_protocol::recipe_generated_files());
    let files: Vec<_> = files.collect();
    if !expected.is_empty()
        && files
            .iter()
            .map(|file| file.path.clone())
            .collect::<std::collections::BTreeSet<_>>()
            != expected
    {
        eprintln!("declared protocol output roster differs from schema; update build/protocol/outputs.txt from --list");
        return ExitCode::FAILURE;
    }
    if list && root.is_none() {
        for file in files {
            println!("{}", file.path);
        }
        return ExitCode::SUCCESS;
    }
    let Some(root) = root.filter(|_| !list) else {
        eprintln!("an explicit --output-root is required");
        return ExitCode::FAILURE;
    };
    for file in files {
        let path = root.join(&file.path);
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(&path, file.contents));
        if let Err(error) = result {
            eprintln!("could not materialize {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
