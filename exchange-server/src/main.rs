#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    exchange_server::serve_from_env().await?;
    Ok(())
}
