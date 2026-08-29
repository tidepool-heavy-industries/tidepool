fn main() -> std::process::ExitCode {
    match tidepool_extract_cmd::frontend::run(std::env::args_os().skip(1).collect()) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("tidepool-extract: {error}");
            std::process::ExitCode::from(2)
        }
    }
}
