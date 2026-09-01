#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tidepool_agent::run_interactive_node().await
}
