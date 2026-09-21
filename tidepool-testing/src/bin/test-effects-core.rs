fn main() -> std::io::Result<()> {
    println!("{}", tidepool_mcp::ensure_effects_core_module()?.display());
    Ok(())
}
