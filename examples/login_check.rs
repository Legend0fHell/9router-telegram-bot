use nine_router_bot::{client::NineRouterClient, config::Config};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let client = NineRouterClient::new(
        config.router_url,
        config.router_password,
        config.router_api_key,
    )?;
    client.refresh_session().await?;
    client.fetch_connections().await?;
    println!("Login and authenticated dashboard request succeeded. No Telegram requests sent.");
    Ok(())
}
