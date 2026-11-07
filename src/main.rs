use anyhow::Result;
use clap::{Parser, Subcommand};
use mad_super_bot::{
    auth::{AuthManager, Endpoints},
    config::ConfigPaths,
    runtime,
    twitch::Api,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Twitch command and scheduled message bot")]
struct Cli {
    /// YAML configuration file.
    #[arg(long, default_value = "./config.yml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Validate all settings locally without contacting Twitch or Bitwarden.
    CheckConfig,
    /// Authorize the intended bot account using Twitch Device Code Flow.
    Auth,
}
fn main() -> Result<()> {
    mad_super_bot::startup::run(run())
}
async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mad_super_bot=info".into()),
        )
        .init();
    let cli = Cli::parse();
    let paths = ConfigPaths::new(cli.config)?;
    let config = paths.load()?;
    if matches!(cli.command, Some(Action::CheckConfig)) {
        println!(
            "Configuration valid: {} commands, {} loops",
            config.runtime.commands.len(),
            config.runtime.loops.len()
        );
        return Ok(());
    }
    let mut shutdown = mad_super_bot::startup::Shutdown::new()?;
    let credentials = config.startup.credentials().await?;
    match cli.command {
        Some(Action::Auth) => {
            shutdown
                .until(AuthManager::authorize(
                    credentials,
                    &config.startup.token_file,
                    Endpoints::default(),
                    |uri, code| {
                        println!("Open {uri} and authorize the bot account with code {code}")
                    },
                ))
                .await?;
            println!("Bot authorization saved in the private token file");
            Ok(())
        }
        None => {
            let api = shutdown
                .until(async {
                    let auth = AuthManager::load(
                        credentials,
                        &config.startup.token_file,
                        Endpoints::default(),
                    )
                    .await?;
                    let channel_id =
                        Api::resolve_channel(&auth, &config.startup.twitch.channel).await?;
                    Api::new(auth, channel_id)
                })
                .await?;
            runtime::run_loaded(paths, config, api, shutdown.receiver()).await
        }
        Some(Action::CheckConfig) => unreachable!(),
    }
}
