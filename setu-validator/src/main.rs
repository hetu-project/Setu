//! Setu Validator - Main entry point

use setu_core::NodeConfig;
use setu_validator::Validator;
use tokio::sync::mpsc;
use tracing::Level;
use tracing_subscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    // Load configuration from environment
    let config = NodeConfig::from_env();

    // Create event channel
    let (_event_tx, event_rx) = mpsc::unbounded_channel();

    // Create and run validator
    let validator = Validator::new(config, event_rx);
    validator.run().await;

    Ok(())
}

