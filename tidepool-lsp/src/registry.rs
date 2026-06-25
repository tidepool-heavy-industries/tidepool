//! File extension → language-server command. v1 wires rust-analyzer only;
//! adding a language is one entry here (the effect surface is unchanged).

/// The server command for a file, by extension, or `None` if unsupported.
pub fn server_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Some("rust-analyzer"),
        _ => None,
    }
}
