//! Common types for the router module
//!
//! Centralizes type definitions to avoid duplication across modules.

use serde::{Deserialize, Serialize};

/// Subnet identifier (32 bytes, matches setu_types::SubnetId)
pub type SubnetId = [u8; 32];

/// Object identifier (32 bytes, matches setu_types::ObjectId)
pub type ObjectId = [u8; 32];

/// Shard identifier (numeric for efficient routing)
pub type ShardId = u16;

/// Legacy shard ID (string-based, for MVP compatibility)
pub type LegacyShardId = String;

/// Root subnet constant - for global/system operations
pub const ROOT_SUBNET: SubnetId = [0u8; 32];

/// Default number of shards (should be power of 2)
pub const DEFAULT_SHARD_COUNT: u16 = 16;

/// Default shard ID for MVP (single shard mode)
pub const DEFAULT_SHARD_ID: &str = "default";

/// How a routing decision was made
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingMethod {
    /// Routed by subnet ID (all subnet txs go to same shard)
    BySubnet,
    /// Routed by primary object ID (same object = same shard)
    ByObject,
    /// Routed by sender address
    BySender,
}

impl std::fmt::Display for RoutingMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingMethod::BySubnet => write!(f, "subnet"),
            RoutingMethod::ByObject => write!(f, "object"),
            RoutingMethod::BySender => write!(f, "sender"),
        }
    }
}
