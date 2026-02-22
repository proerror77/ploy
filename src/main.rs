mod main_legacy;

#[tokio::main]
async fn main() -> ploy::error::Result<()> {
    main_legacy::run().await
}
