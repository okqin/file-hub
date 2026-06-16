//! File Hub executable entrypoint.

#![forbid(unsafe_code)]
#![warn(rust_2024_compatibility, missing_docs, missing_debug_implementations)]

use anyhow::{Context, Result};
use file_hub::{config::AppConfig, http::build_router};
use tokio::{net::TcpListener, signal};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config_path = std::env::args_os()
        .nth(1)
        .context("usage: file-hub <config.yaml>")?;
    let config = AppConfig::load_from_path(config_path)
        .await
        .context("load application configuration")?;
    let bind_address = config.server().bind_address();
    let listener = TcpListener::bind(bind_address)
        .await
        .with_context(|| format!("bind HTTP listener at {bind_address}"))?;

    info!(%bind_address, "serving File Hub");
    axum::serve(listener, build_router(config))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP requests")
}

async fn shutdown_signal() {
    if let Err(error) = signal::ctrl_c().await {
        tracing::warn!(%error, "failed to listen for shutdown signal");
    }
}
