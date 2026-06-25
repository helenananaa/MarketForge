use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    exchange_server::serve(SocketAddr::from(([127, 0, 0, 1], 57305))).await?;
    Ok(())
}
