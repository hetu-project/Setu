use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

// Use independent VLC library
pub use setu_vlc::{VectorClock, VLCSnapshot};

// Placeholder types (to be replaced with actual implementations)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub from: String,
    pub to: String,
    pub amount: u64,
}

pub type EventId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventStatus {
    Pending,
    InWorkQueue,
    Executed,
    Confirmed,
    Finalized,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    Transfer,
    System,
    Genesis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub event_type: EventType,
    pub parent_ids: Vec<EventId>,
    pub transfer: Option<Transfer>,
    pub vlc_snapshot: VLCSnapshot,
    pub creator: String,
    pub status: EventStatus,
    pub execution_result: Option<ExecutionResult>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub message: Option<String>,
    pub state_changes: Vec<StateChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChange {
    pub key: String,
    pub old_value: Option<Vec<u8>>,
    pub new_value: Option<Vec<u8>>,
}

impl Event {
    pub fn new(
        event_type: EventType,
        parent_ids: Vec<EventId>,
        vlc_snapshot: VLCSnapshot,
        creator: String,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        
        let id = Self::compute_id(&parent_ids, &vlc_snapshot, &creator, timestamp);
        
        Self {
            id,
            event_type,
            parent_ids,
            transfer: None,
            vlc_snapshot,
            creator,
            status: EventStatus::Pending,
            execution_result: None,
            timestamp,
        }
    }

    pub fn genesis(creator: String, vlc_snapshot: VLCSnapshot) -> Self {
        Self::new(EventType::Genesis, vec![], vlc_snapshot, creator)
    }

    fn compute_id(
        parent_ids: &[EventId],
        vlc_snapshot: &VLCSnapshot,
        creator: &str,
        timestamp: u64,
    ) -> EventId {
        let mut hasher = Sha256::new();
        for parent_id in parent_ids {
            hasher.update(parent_id.as_bytes());
        }
        hasher.update(vlc_snapshot.logical_time.to_le_bytes());
        hasher.update(creator.as_bytes());
        hasher.update(timestamp.to_le_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn with_transfer(mut self, transfer: Transfer) -> Self {
        self.transfer = Some(transfer);
        self
    }

    pub fn set_status(&mut self, status: EventStatus) {
        self.status = status;
    }

    pub fn set_execution_result(&mut self, result: ExecutionResult) {
        let success = result.success;
        self.execution_result = Some(result);
        if success {
            self.status = EventStatus::Executed;
        } else {
            self.status = EventStatus::Failed;
        }
    }

    pub fn is_genesis(&self) -> bool {
        self.event_type == EventType::Genesis
    }

    pub fn has_parents(&self) -> bool {
        !self.parent_ids.is_empty()
    }

    pub fn depends_on(&self, event_id: &EventId) -> bool {
        self.parent_ids.contains(event_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_vlc_snapshot() -> VLCSnapshot {
        VLCSnapshot {
            vector_clock: VectorClock::new(),
            logical_time: 1,
            physical_time: 1000,
        }
    }

    #[test]
    fn test_event_creation() {
        let event = Event::new(
            EventType::Transfer,
            vec![],
            create_vlc_snapshot(),
            "node1".to_string(),
        );
        assert!(!event.id.is_empty());
        assert_eq!(event.status, EventStatus::Pending);
    }

    #[test]
    fn test_genesis_event() {
        let event = Event::genesis("node1".to_string(), create_vlc_snapshot());
        assert!(event.is_genesis());
        assert!(!event.has_parents());
    }
}
