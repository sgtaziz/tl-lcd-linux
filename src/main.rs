mod config;
mod fan;
mod hardware;
mod media;
mod service;

use clap::Parser;
use env_logger::Env;
use log::LevelFilter;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Linux service for Lian Li TL Wireless LCD fans"
)]
struct Cli {
    /// Path to the configuration file
    #[arg(long, default_value = "config.json")]
    config: PathBuf,

    /// Logging verbosity (error, warn, info, debug, trace)
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logger(&cli.log_level)?;
    let mut manager = service::ServiceManager::new(cli.config)?;
    manager.run()
}

fn init_logger(level: &str) -> anyhow::Result<()> {
    let filter = match level.to_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" | "warning" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        other => anyhow::bail!("invalid log level '{other}'"),
    };

    env_logger::Builder::from_env(Env::default().default_filter_or(level))
        .filter_level(filter)
        .format_timestamp_secs()
        .init();
    Ok(())
}
