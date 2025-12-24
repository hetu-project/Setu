//! Setu Validator - Verification and coordination node
//!
//! The validator is responsible for:
//! - Receiving events from solvers
//! - Verifying event validity
//! - Maintaining the global Foldgraph
//! - Coordinating consensus

use setu_core::{NodeConfig, ShardManager};
use setu_types::event::Event;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, error};

/// Event verification error
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("Event has no execution result")]
    NoExecutionResult,
    
    #[error("Event execution failed: {0}")]
    ExecutionFailed(String),
    
    #[error("Invalid event creator: {0}")]
    InvalidCreator(String),
    
    #[error("Event timestamp is in the future")]
    FutureTimestamp,
    
    #[error("Missing parent event: {0}")]
    MissingParent(String),
    
    #[error("Invalid VLC snapshot")]
    InvalidVLC,
}

/// Validator node
pub struct Validator {
    config: NodeConfig,
    shard_manager: Arc<ShardManager>,
    event_rx: mpsc::UnboundedReceiver<Event>,
    /// Store of verified events (event_id -> event)
    verified_events: HashMap<String, Event>,
}

impl Validator {
    /// Create a new validator with event receiver channel
    pub fn new(
        config: NodeConfig,
        event_rx: mpsc::UnboundedReceiver<Event>,
    ) -> Self {
        info!(
            node_id = %config.node_id,
            "Creating validator node"
        );
        
        let shard_manager = Arc::new(ShardManager::new());
        
        Self {
            config,
            shard_manager,
            event_rx,
            verified_events: HashMap::new(),
        }
    }
    
    /// Run the validator
    pub async fn run(mut self) {
        info!(
            node_id = %self.config.node_id,
            port = self.config.network.port,
            "Validator started, waiting for events..."
        );
        
        // Main loop: receive and verify events
        while let Some(event) = self.event_rx.recv().await {
            info!(
                event_id = %event.id,
                creator = %event.creator,
                event_type = ?event.event_type,
                "Received event"
            );
            
            // Verify the event
            match self.verify_event(&event).await {
                Ok(()) => {
                    info!(
                        event_id = %event.id,
                        "Event verified successfully"
                    );
                    
                    // Store the verified event
                    self.verified_events.insert(event.id.clone(), event);
                    
                    info!(
                        total_verified = self.verified_events.len(),
                        "Event added to verified store"
                    );
                }
                Err(e) => {
                    warn!(
                        event_id = %event.id,
                        error = %e,
                        "Event verification failed"
                    );
                }
            }
        }
        
        info!("Validator stopped");
    }
    
    /// Verify an event
    async fn verify_event(&self, event: &Event) -> Result<(), ValidationError> {
        info!("Verifying event: {}", event.id);
        
        // 1. Check execution result exists
        let execution_result = event.execution_result.as_ref()
            .ok_or(ValidationError::NoExecutionResult)?;
        
        // 2. Check execution was successful
        if !execution_result.success {
            return Err(ValidationError::ExecutionFailed(
                execution_result.message.clone()
                    .unwrap_or_else(|| "Unknown error".to_string())
            ));
        }
        
        // 3. Verify creator is valid (basic check)
        if event.creator.is_empty() {
            return Err(ValidationError::InvalidCreator(
                "Creator cannot be empty".to_string()
            ));
        }
        
        // 4. Check timestamp is not in the future
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        
        if event.timestamp > now + 60000 { // Allow 60s clock skew
            return Err(ValidationError::FutureTimestamp);
        }
        
        // 5. Verify parent events exist (if not genesis)
        if !event.is_genesis() {
            for parent_id in &event.parent_ids {
                if !self.verified_events.contains_key(parent_id) {
                    return Err(ValidationError::MissingParent(parent_id.clone()));
                }
            }
        }
        
        // 6. Verify VLC snapshot is valid
        if event.vlc_snapshot.logical_time == 0 && !event.is_genesis() {
            return Err(ValidationError::InvalidVLC);
        }
        
        info!("Event verification passed: {}", event.id);
        Ok(())
    }
    
    /// Get node ID
    pub fn node_id(&self) -> &str {
        &self.config.node_id
    }
    
    /// Get number of verified events
    pub fn verified_count(&self) -> usize {
        self.verified_events.len()
    }
    
    /// Check if an event has been verified
    pub fn is_verified(&self, event_id: &str) -> bool {
        self.verified_events.contains_key(event_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use setu_types::event::{Event, EventType, ExecutionResult, StateChange};
    use setu_vlc::VLCSnapshot;
    use tokio::sync::mpsc;

    fn create_test_config() -> NodeConfig {
        use setu_core::config::NetworkConfig;
        NodeConfig {
            node_id: "test-validator".to_string(),
            network: NetworkConfig {
                listen_addr: "127.0.0.1".to_string(),
                port: 9999,
                peers: vec![],
            },
        }
    }

    fn create_vlc_snapshot() -> VLCSnapshot {
        use setu_vlc::VectorClock;
        VLCSnapshot {
            vector_clock: VectorClock::new(),
            logical_time: 1,
            physical_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    fn create_valid_event() -> Event {
        let mut event = Event::new(
            EventType::Transfer,
            vec![],
            create_vlc_snapshot(),
            "solver-1".to_string(),
        );

        let execution_result = ExecutionResult {
            success: true,
            message: Some("Success".to_string()),
            state_changes: vec![
                StateChange {
                    key: "balance:alice".to_string(),
                    old_value: Some(vec![]),
                    new_value: Some(vec![]),
                },
            ],
        };
        event.set_execution_result(execution_result);
        event
    }

    #[test]
    fn test_validator_creation() {
        let config = create_test_config();
        let (_tx, rx) = mpsc::unbounded_channel();
        let validator = Validator::new(config, rx);
        assert_eq!(validator.node_id(), "test-validator");
        assert_eq!(validator.verified_count(), 0);
    }

    #[tokio::test]
    async fn test_verify_valid_event() {
        let config = create_test_config();
        let (_tx, rx) = mpsc::unbounded_channel();
        let validator = Validator::new(config, rx);

        let event = create_valid_event();
        let result = validator.verify_event(&event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_verify_event_without_execution_result() {
        let config = create_test_config();
        let (_tx, rx) = mpsc::unbounded_channel();
        let validator = Validator::new(config, rx);

        let event = Event::new(
            EventType::Transfer,
            vec![],
            create_vlc_snapshot(),
            "solver-1".to_string(),
        );

        let result = validator.verify_event(&event).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::NoExecutionResult));
    }

    #[tokio::test]
    async fn test_verify_event_with_failed_execution() {
        let config = create_test_config();
        let (_tx, rx) = mpsc::unbounded_channel();
        let validator = Validator::new(config, rx);

        let mut event = Event::new(
            EventType::Transfer,
            vec![],
            create_vlc_snapshot(),
            "solver-1".to_string(),
        );

        let execution_result = ExecutionResult {
            success: false,
            message: Some("Execution failed".to_string()),
            state_changes: vec![],
        };
        event.set_execution_result(execution_result);

        let result = validator.verify_event(&event).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::ExecutionFailed(_)));
    }

    #[tokio::test]
    async fn test_verify_event_with_empty_creator() {
        let config = create_test_config();
        let (_tx, rx) = mpsc::unbounded_channel();
        let validator = Validator::new(config, rx);

        let mut event = Event::new(
            EventType::Transfer,
            vec![],
            create_vlc_snapshot(),
            "".to_string(), // Empty creator
        );

        let execution_result = ExecutionResult {
            success: true,
            message: Some("Success".to_string()),
            state_changes: vec![],
        };
        event.set_execution_result(execution_result);

        let result = validator.verify_event(&event).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::InvalidCreator(_)));
    }

    #[tokio::test]
    async fn test_validator_receives_and_stores_events() {
        let config = create_test_config();
        let (tx, rx) = mpsc::unbounded_channel();
        let mut validator = Validator::new(config, rx);

        // Send a valid event
        let event = create_valid_event();
        let event_id = event.id.clone();
        tx.send(event).unwrap();

        // Process one event manually
        if let Some(event) = validator.event_rx.recv().await {
            let _ = validator.verify_event(&event).await;
            validator.verified_events.insert(event.id.clone(), event);
        }

        assert_eq!(validator.verified_count(), 1);
        assert!(validator.is_verified(&event_id));
    }
}
