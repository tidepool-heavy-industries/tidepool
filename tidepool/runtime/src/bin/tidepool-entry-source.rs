//! Build-action projection of the existing settled-entry source renderer.

use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut module = None;
    let mut entry = None;
    let mut effects = None;
    let mut output = None;
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or("missing build-action argument value")?;
        match flag.as_str() {
            "--module" if module.is_none() => module = Some(value),
            "--entry" if entry.is_none() => entry = Some(value),
            "--effects" if effects.is_none() => effects = Some(value),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown or duplicate argument {flag}").into()),
        }
    }
    let module = module.ok_or("missing --module")?;
    let entry = entry.ok_or("missing --entry")?;
    let effects = effects.ok_or("missing --effects")?;
    let output = output.ok_or("missing --output")?;
    for name in [&module, &entry, &effects] {
        if name.is_empty()
            || !name.split('.').all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_')
            })
        {
            return Err("build action requires qualified Haskell names".into());
        }
    }
    let entry_module = entry.rsplit_once('.').ok_or("entry must be qualified")?.0;
    let effects_module = effects
        .rsplit_once('.')
        .ok_or("effect row must be qualified")?
        .0;
    let mut preamble = format!(
        "{}\nmodule {module} where\nimport Prelude\nimport Control.Monad.Freer (Eff)\nimport qualified Data.Text as T\nimport qualified {entry_module}\n{}",
        tidepool_runtime::session::EVAL_PRAGMAS,
        tidepool_runtime::session::PREAMBLE_IMPORT_MARKER,
    );
    if entry_module != effects_module {
        preamble = tidepool_runtime::session::insert_preamble_imports(
            &preamble,
            &format!("qualified {effects_module}"),
        );
    }
    let source = tidepool_runtime::session::assemble_opaque_expression_module(
        &preamble,
        "__result",
        &effects,
        &entry,
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    std::fs::write(output, source)?;
    Ok(())
}
