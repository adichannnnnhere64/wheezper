//! Binary entry point: configuration, logging and signal handling.

use anyhow::Result;
use tracing_subscriber::EnvFilter;
use wheezper_server::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    let config = Config::from_env()?;
    eprintln!(
        "wheezper-server: bind={} assistant={} ({}) retention={}d",
        config.bind, config.assistant_url, config.assistant_model, config.retention_days
    );

    wheezper_server::serve(config, shutdown_signal()).await
}

/// Ctrl-C or SIGTERM (containers) trigger a graceful drain.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown requested");
}
