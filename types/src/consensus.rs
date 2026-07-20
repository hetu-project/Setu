use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::event::{EventId, VLCSnapshot};
use crate::merkle::AnchorMerkleRoots;

#[allow(unused_imports)]
use crate::event::VectorClock;

pub type AnchorId = String;
pub type CFId = String;

/// Anchor represents a checkpoint in the DAG that commits to a set of events
/// and the resulting state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub id: AnchorId,
    pub event_ids: Vec<EventId>,
    pub vlc_snapshot: VLCSnapshot,
    /// Legacy state root (for backward compatibility)
    pub state_root: String,
    /// Full Merkle roots (new field)
    #[serde(default)]
    pub merkle_roots: Option<AnchorMerkleRoots>,
    pub previous_anchor: Option<AnchorId>,
    pub depth: u64,
    pub timestamp: u64,
}

impl Anchor {
    /// Create a new anchor with legacy state_root (backward compatible)
    pub fn new(
        event_ids: Vec<EventId>,
        vlc_snapshot: VLCSnapshot,
        state_root: String,
        previous_anchor: Option<AnchorId>,
        depth: u64,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let id = Self::compute_id(&event_ids, &vlc_snapshot, &state_root, timestamp);

        Self {
            id,
            event_ids,
            vlc_snapshot,
            state_root,
            merkle_roots: None,
            previous_anchor,
            depth,
            timestamp,
        }
    }
    
    /// Create a new anchor with full Merkle roots
    pub fn with_merkle_roots(
        event_ids: Vec<EventId>,
        vlc_snapshot: VLCSnapshot,
        merkle_roots: AnchorMerkleRoots,
        previous_anchor: Option<AnchorId>,
        depth: u64,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Use global_state_root as the legacy state_root
        let state_root = hex::encode(&merkle_roots.global_state_root);
        let id = Self::compute_id(&event_ids, &vlc_snapshot, &state_root, timestamp);

        Self {
            id,
            event_ids,
            vlc_snapshot,
            state_root,
            merkle_roots: Some(merkle_roots),
            previous_anchor,
            depth,
            timestamp,
        }
    }

    fn compute_id(
        event_ids: &[EventId],
        vlc_snapshot: &VLCSnapshot,
        state_root: &str,
        _timestamp: u64,
    ) -> AnchorId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"SETU_ANCHOR_ID:");
        for event_id in event_ids {
            hasher.update(event_id.as_bytes());
        }
        hasher.update(&vlc_snapshot.logical_time.to_le_bytes());
        hasher.update(state_root.as_bytes());
        // NOTE: timestamp is intentionally excluded from the ID computation.
        // Using SystemTime::now() made anchor IDs non-deterministic across validators,
        // breaking consensus. The event_ids + vlc + state_root already ensure uniqueness.
        hex::encode(hasher.finalize().as_bytes())
    }

    pub fn event_count(&self) -> usize {
        self.event_ids.len()
    }
    
    /// Get the global state root (from merkle_roots if available)
    pub fn global_state_root(&self) -> Option<&[u8; 32]> {
        self.merkle_roots.as_ref().map(|m| &m.global_state_root)
    }
    
    /// Get the events root
    pub fn events_root(&self) -> Option<&[u8; 32]> {
        self.merkle_roots.as_ref().map(|m| &m.events_root)
    }
    
    /// Get the anchor chain root
    pub fn anchor_chain_root(&self) -> Option<&[u8; 32]> {
        self.merkle_roots.as_ref().map(|m| &m.anchor_chain_root)
    }
    
    /// Check if this anchor has full Merkle roots
    pub fn has_merkle_roots(&self) -> bool {
        self.merkle_roots.is_some()
    }
    
    /// Get a specific subnet's state root
    pub fn get_subnet_root(&self, subnet_id: &crate::subnet::SubnetId) -> Option<&[u8; 32]> {
        self.merkle_roots
            .as_ref()
            .and_then(|m| m.get_subnet_root(subnet_id))
    }
    
    /// Compute a hash of this anchor for anchor chain tree
    /// 
    /// This hash includes all critical fields:
    /// - id, depth, previous_anchor (chain structure)
    /// - events_root, global_state_root, anchor_chain_root (Merkle commitments)
    /// - vlc_snapshot, timestamp (ordering)
    pub fn compute_hash(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"SETU_ANCHOR_HASH:");
        hasher.update(self.id.as_bytes());
        hasher.update(&self.depth.to_le_bytes());
        if let Some(prev) = &self.previous_anchor {
            hasher.update(prev.as_bytes());
        }
        if let Some(roots) = &self.merkle_roots {
            hasher.update(&roots.events_root);
            hasher.update(&roots.global_state_root);
            hasher.update(&roots.anchor_chain_root);
        }
        hasher.update(&self.vlc_snapshot.logical_time.to_le_bytes());
        hasher.update(&self.timestamp.to_le_bytes());
        *hasher.finalize().as_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CFStatus {
    Proposed,
    Voting,
    Approved,
    Finalized,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    pub validator_id: String,
    pub cf_id: CFId,
    pub approve: bool,
    pub signature: Vec<u8>,
    pub timestamp: u64,
}

impl Vote {
    pub fn new(validator_id: String, cf_id: CFId, approve: bool) -> Self {
        Self {
            validator_id,
            cf_id,
            approve,
            signature: Vec::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    pub fn with_signature(mut self, signature: Vec<u8>) -> Self {
        self.signature = signature;
        self
    }
    
    /// Sign the vote with a private key (ed25519)
    /// 
    /// This creates a signature over the vote content (cf_id + validator_id + approve + timestamp)
    /// using the validator's private key.
    pub fn sign(&mut self, private_key: &[u8]) -> Result<(), String> {
        use ed25519_dalek::{Signer, SigningKey};
        
        if private_key.len() != 32 {
            return Err(format!("Invalid private key length: expected 32, got {}", private_key.len()));
        }
        
        let signing_key = SigningKey::from_bytes(
            private_key.try_into().map_err(|_| "Failed to convert key")?   
        );
        
        // Create deterministic message to sign
        let message = self.signing_message();
        
        // Sign the message
        let signature = signing_key.sign(&message);
        self.signature = signature.to_bytes().to_vec();
        
        Ok(())
    }
    
    /// Verify the vote signature with a public key (ed25519)
    /// 
    /// Returns true if the signature is valid for the given public key.
    pub fn verify_signature(&self, public_key: &[u8]) -> bool {
        use ed25519_dalek::{Verifier, VerifyingKey, Signature as Ed25519Signature};
        
        // Check signature is not empty
        if self.signature.is_empty() {
            return false;
        }
        
        // Check public key length
        if public_key.len() != 32 {
            return false;
        }
        
        // Parse public key
        let verifying_key = match VerifyingKey::from_bytes(
            public_key.try_into().unwrap_or(&[0u8; 32])
        ) {
            Ok(key) => key,
            Err(_) => return false,
        };
        
        // Parse signature
        let signature = match Ed25519Signature::from_slice(&self.signature) {
            Ok(sig) => sig,
            Err(_) => return false,
        };
        
        // Create message and verify
        let message = self.signing_message();
        verifying_key.verify(&message, &signature).is_ok()
    }
    
    /// Create the deterministic message for signing
    /// 
    /// Message format: protocol_id || cf_id || validator_id || approve || timestamp
    /// 
    /// The protocol identifier prevents cross-protocol signature attacks where
    /// a signature from one protocol could be replayed in another context.
    fn signing_message(&self) -> Vec<u8> {
        let mut message = Vec::new();
        // Protocol domain separator (version 1)
        message.extend_from_slice(b"SETU_VOTE_V1");
        message.extend_from_slice(self.cf_id.as_bytes());
        message.extend_from_slice(self.validator_id.as_bytes());
        message.push(if self.approve { 1 } else { 0 });
        message.extend_from_slice(&self.timestamp.to_le_bytes());
        message
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusFrame {
    pub id: CFId,
    /// Consensus round at which this CF was proposed. Binds proposer identity
    /// to the leader-rotation schedule so a forged CF carrying a stale
    /// proposer cannot pass `verify_id` at a different round (PR-4 v3).
    pub round: u64,
    pub anchor: Anchor,
    pub proposer: String,
    pub status: CFStatus,
    pub votes: HashMap<String, Vote>,
    pub created_at: u64,
    pub finalized_at: Option<u64>,
}

impl ConsensusFrame {
    pub fn new(round: u64, anchor: Anchor, proposer: String) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let id = Self::compute_id(round, &anchor, &proposer, timestamp);

        Self {
            id,
            round,
            anchor,
            proposer,
            status: CFStatus::Proposed,
            votes: HashMap::new(),
            created_at: timestamp,
            finalized_at: None,
        }
    }

    fn compute_id(round: u64, anchor: &Anchor, proposer: &str, _timestamp: u64) -> CFId {
        let mut hasher = blake3::Hasher::new();
        // V2 domain separator: includes round to bind proposer<->round (PR-4).
        hasher.update(b"SETU_CF_ID_V2:");
        hasher.update(&round.to_le_bytes());
        hasher.update(anchor.id.as_bytes());
        hasher.update(proposer.as_bytes());
        // NOTE: timestamp is intentionally excluded from the ID computation.
        // Using SystemTime::now() made CF IDs non-deterministic across validators,
        // breaking consensus. The round + anchor + proposer already ensure uniqueness.
        hex::encode(hasher.finalize().as_bytes())
    }

    pub fn add_vote(&mut self, vote: Vote) {
        self.votes.insert(vote.validator_id.clone(), vote);
    }

    pub fn approve_count(&self) -> usize {
        self.votes.values().filter(|v| v.approve).count()
    }

    pub fn reject_count(&self) -> usize {
        self.votes.values().filter(|v| !v.approve).count()
    }

    pub fn check_quorum(&self, total_validators: usize) -> bool {
        // BFT quorum: ceil(2n/3). For n=3 → 2, n=4 → 3, n=6 → 4.
        // Previous formula `(n * 2) / 3 + 1` required unanimity for n=3 (3/3),
        // which is incorrect for BFT consensus.
        let threshold = (total_validators * 2 + 2) / 3;
        self.approve_count() >= threshold
    }

    /// Check if CF should be rejected (1/3+1 validators rejected)
    /// 
    /// In BFT consensus, if f+1 nodes reject a CF (where f = n/3),
    /// it should be explicitly rejected to prevent indefinite pending.
    pub fn check_rejection(&self, total_validators: usize) -> bool {
        let threshold = total_validators / 3 + 1;
        self.reject_count() >= threshold
    }

    /// Check if CF has timed out
    /// 
    /// Returns true if the CF has been pending longer than the timeout threshold.
    /// Used for garbage collection of stale CFs that never reached quorum.
    pub fn is_timeout(&self, timeout_ms: u64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        now.saturating_sub(self.created_at) > timeout_ms
    }

    pub fn finalize(&mut self) {
        self.status = CFStatus::Finalized;
        self.finalized_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        );
    }

    pub fn reject(&mut self) {
        self.status = CFStatus::Rejected;
    }
    
    /// Verify that the CF ID matches the content
    /// 
    /// This prevents malicious nodes from constructing CFs with mismatched IDs.
    pub fn verify_id(&self) -> bool {
        let expected_id = Self::compute_id(self.round, &self.anchor, &self.proposer, self.created_at);
        self.id == expected_id
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConsensusConfig {
    pub vlc_delta_threshold: u64,
    pub min_events_per_cf: usize,
    pub max_events_per_cf: usize,
    pub cf_timeout_ms: u64,
    pub validator_count: usize,
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            vlc_delta_threshold: 10,
            min_events_per_cf: 1,
            max_events_per_cf: 1000,
            cf_timeout_ms: 5000,
            validator_count: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_vlc_snapshot() -> VLCSnapshot {
        VLCSnapshot {
            vector_clock: VectorClock::new(),
            logical_time: 10,
            physical_time: 10000,
        }
    }

    #[test]
    fn test_anchor_creation() {
        let anchor = Anchor::new(
            vec!["event1".to_string(), "event2".to_string()],
            create_vlc_snapshot(),
            "state_root_hash".to_string(),
            None,
            0,
        );
        assert_eq!(anchor.event_count(), 2);
    }

    #[test]
    fn test_cf_voting() {
        let anchor = Anchor::new(
            vec!["event1".to_string()],
            create_vlc_snapshot(),
            "state_root".to_string(),
            None,
            0,
        );
        let mut cf = ConsensusFrame::new(0, anchor, "validator1".to_string());

        cf.add_vote(Vote::new("validator1".to_string(), cf.id.clone(), true));
        cf.add_vote(Vote::new("validator2".to_string(), cf.id.clone(), true));
        cf.add_vote(Vote::new("validator3".to_string(), cf.id.clone(), true));

        assert_eq!(cf.approve_count(), 3);
        assert_eq!(cf.reject_count(), 0);
        assert!(cf.check_quorum(3));
    }

    #[test]
    fn consensus_frame_round_in_id() {
        let anchor = Anchor::new(
            vec!["e1".to_string()],
            create_vlc_snapshot(),
            "state_root".to_string(),
            None,
            0,
        );
        // Two CFs identical except for round must produce distinct IDs.
        let timestamp = 12345u64;
        let id_r0 = ConsensusFrame::compute_id(0, &anchor, "v1", timestamp);
        let id_r1 = ConsensusFrame::compute_id(1, &anchor, "v1", timestamp);
        assert_ne!(id_r0, id_r1, "round must affect compute_id output");
    }

    #[test]
    fn consensus_frame_verify_id_detects_round_tamper() {
        let anchor = Anchor::new(
            vec!["e1".to_string()],
            create_vlc_snapshot(),
            "state_root".to_string(),
            None,
            0,
        );
        let mut cf = ConsensusFrame::new(5, anchor, "v1".to_string());
        assert!(cf.verify_id(), "fresh CF must self-verify");
        cf.round = 999;
        assert!(!cf.verify_id(), "tampered round must invalidate id");
    }

    #[test]
    fn consensus_frame_id_v2_domain_separator_diverges_from_v1() {
        // Hand-compute a V1-style id (legacy domain, no round) and compare
        // against V2 with round=0; they must differ so V1 blobs decoded into
        // V2 structs cannot accidentally self-verify.
        let anchor = Anchor::new(
            vec!["e1".to_string()],
            create_vlc_snapshot(),
            "state_root".to_string(),
            None,
            0,
        );
        let timestamp = 7777u64;
        let v2_id = ConsensusFrame::compute_id(0, &anchor, "v1", timestamp);

        let mut h = blake3::Hasher::new();
        h.update(b"SETU_CF_ID:");
        h.update(anchor.id.as_bytes());
        h.update(b"v1");
        h.update(&timestamp.to_le_bytes());
        let v1_id = hex::encode(h.finalize().as_bytes());

        assert_ne!(v1_id, v2_id, "V1 and V2 domains must produce distinct ids");
    }
}
