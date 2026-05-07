use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    hc_api::serve().await
}
