//! Setu Validator - Verification and coordination node
//!
//! The validator is responsible for:
//! - Receiving events from solvers
//! - Verifying event validity
//! - Maintaining the global Foldgraph
//! - Coordinating consensus

use setu_core::{NodeConfig, ShardManager};
use std::sync::Arc;
use tracing::info;

/// Validator node
pub struct Validator {
    config: NodeConfig,
    shard_manager: Arc<ShardManager>,
}

impl Validator {
    /// Create a new validator
    pub fn new(config: NodeConfig) -> Self {
        info!(
            node_id = %config.node_id,
            "Creating validator node"
        );
        
        let shard_manager = Arc::new(ShardManager::new());
        
        Self {
            config,
            shard_manager,
        }
    }
    
    /// Run the validator
    pub async fn run(self) {
        info!(
            node_id = %self.config.node_id,
            port = self.config.network.port,
            "Validator started"
        );
        
        // TODO: Implement validator logic
        // - Listen for events
        // - Verify events
        // - Maintain Foldgraph
        
        // For now, just keep running
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
    
    /// Get node ID
    pub fn node_id(&self) -> &str {
        &self.config.node_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_validator_creation() {
        let config = NodeConfig::default();
        let validator = Validator::new(config);
        assert!(!validator.node_id().is_empty());
    }
}
