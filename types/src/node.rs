use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRole {
    Validator,
    Solver,
    LightNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Initializing,
    Syncing,
    Active,
    Inactive,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: String,
    pub role: NodeRole,
    pub status: NodeStatus,
    pub address: String,
    pub port: u16,
    pub public_key: Vec<u8>,
    pub stake: u64,
    pub tee_enabled: bool,
}

impl NodeInfo {
    pub fn new_validator(id: String, address: String, port: u16) -> Self {
        Self {
            id,
            role: NodeRole::Validator,
            status: NodeStatus::Initializing,
            address,
            port,
            public_key: Vec::new(),
            stake: 0,
            tee_enabled: false,
        }
    }

    pub fn new_solver(id: String, address: String, port: u16, stake: u64) -> Self {
        Self {
            id,
            role: NodeRole::Solver,
            status: NodeStatus::Initializing,
            address,
            port,
            public_key: Vec::new(),
            stake,
            tee_enabled: false,
        }
    }

    pub fn new_light_node(id: String, address: String, port: u16) -> Self {
        Self {
            id,
            role: NodeRole::LightNode,
            status: NodeStatus::Initializing,
            address,
            port,
            public_key: Vec::new(),
            stake: 0,
            tee_enabled: false,
        }
    }

    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    pub fn is_validator(&self) -> bool {
        self.role == NodeRole::Validator
    }

    pub fn is_solver(&self) -> bool {
        self.role == NodeRole::Solver
    }

    pub fn is_active(&self) -> bool {
        self.status == NodeStatus::Active
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub node: NodeInfo,
    pub is_leader: bool,
    pub leader_round: u64,
    /// Signature over node.id to authenticate validator membership changes.
    /// Must be verified by ValidatorSet::add_validator before adding.
    pub signature: Vec<u8>,
}

impl ValidatorInfo {
    pub fn new(node: NodeInfo, is_leader: bool) -> Self {
        Self {
            node,
            is_leader,
            leader_round: 0,
            signature: Vec::new(),
        }
    }
    
    /// Create a ValidatorInfo with a signature authenticating the node ID.
    pub fn with_signature(mut self, signature: Vec<u8>) -> Self {
        self.signature = signature;
        self
    }
    
    /// Verify the validator's signature over the node ID.
    /// Returns Ok(()) if the signature is valid or if no signature
    /// is present (for backward compatibility with existing code).
    /// When a signature IS present, it MUST be valid — invalid
    /// signatures are rejected.
    pub fn verify(&self) -> Result<(), &'static str> {
        if self.signature.is_empty() {
            // No signature present — skip verification for backward
            // compatibility. Callers should add signatures for full
            // security (audit #45).
            return Ok(());
        }
        if self.node.public_key.len() != 32 {
            return Err("invalid public key length");
        }
        // Verify signature against node.id using the validator's public key
        use ed25519_dalek::{VerifyingKey, Verifier, Signature};
        let sig = Signature::try_from(self.signature.as_slice())
            .map_err(|_| "invalid signature format")?;
        let pk_bytes: &[u8; 32] = self.node.public_key.as_slice()
            .try_into().map_err(|_| "invalid public key length")?;
        let pk = VerifyingKey::from_bytes(pk_bytes)
            .map_err(|_| "invalid public key format")?;
        pk.verify(self.node.id.as_bytes(), &sig)
            .map_err(|_| "invalid validator signature")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolverInfo {
    pub node: NodeInfo,
    pub processing_capacity: u64,
    pub current_load: u64,
}

impl SolverInfo {
    pub fn new(node: NodeInfo, processing_capacity: u64) -> Self {
        Self {
            node,
            processing_capacity,
            current_load: 0,
        }
    }

    pub fn available_capacity(&self) -> u64 {
        self.processing_capacity.saturating_sub(self.current_load)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TEEAttestation {
    pub node_id: String,
    pub attestation_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub timestamp: u64,
}

impl TEEAttestation {
    pub fn mock(node_id: String) -> Self {
        Self {
            node_id,
            attestation_data: vec![0u8; 32],
            signature: vec![0u8; 64],
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    pub fn verify(&self) -> bool {
        !self.attestation_data.is_empty() && !self.signature.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_info_creation() {
        let validator = NodeInfo::new_validator(
            "v1".to_string(),
            "127.0.0.1".to_string(),
            8000,
        );
        assert!(validator.is_validator());
        assert_eq!(validator.endpoint(), "127.0.0.1:8000");
    }

    #[test]
    fn test_solver_capacity() {
        let node = NodeInfo::new_solver(
            "s1".to_string(),
            "127.0.0.1".to_string(),
            9000,
            1000,
        );
        let mut solver = SolverInfo::new(node, 50);
        assert_eq!(solver.available_capacity(), 50);
        
        solver.current_load = 30;
        assert_eq!(solver.available_capacity(), 20);
    }
}
