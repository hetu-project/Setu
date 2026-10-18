// Copyright (c) Setu Contributors
// SPDX-License-Identifier: Apache-2.0

//! RPC server implementation for discovery

use super::{DiscoveryConfig, SignedNodeInfo, State};
use anemo::{Request, Response, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// Discovery RPC trait
///
/// This trait defines the RPC methods for the discovery service.
/// It can be used with anemo-build to generate the server implementation.
#[anemo::async_trait]
pub trait Discovery: Send + Sync + 'static {
    /// Get known peers from this node
    async fn get_known_peers(
        &self,
        request: Request<GetKnownPeersRequest>,
    ) -> Result<Response<GetKnownPeersResponse>>;

    /// Push peer info to this node
    async fn push_peer_info(
        &self,
        request: Request<PushPeerInfoRequest>,
    ) -> Result<Response<PushPeerInfoResponse>>;
}

/// Request for getting known peers
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetKnownPeersRequest {
    /// Maximum number of peers to return
    pub limit: Option<usize>,
    /// Filter by node type (optional)
    pub node_type_filter: Option<String>,
}

/// Response with known peers
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetKnownPeersResponse {
    /// Our own node info
    pub own_info: Option<SignedNodeInfo>,
    /// List of known peers
    pub known_peers: Vec<SignedNodeInfo>,
}

/// Request for pushing peer info
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushPeerInfoRequest {
    /// The node info to share
    pub info: SignedNodeInfo,
}

/// Response for push peer info
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushPeerInfoResponse {
    /// Whether the info was accepted
    pub accepted: bool,
    /// Reason if not accepted
    pub reason: Option<String>,
}

/// Discovery server implementation
pub struct Server {
    pub(crate) state: Arc<RwLock<State>>,
    pub(crate) config: DiscoveryConfig,
}

#[anemo::async_trait]
impl Discovery for Server {
    async fn get_known_peers(
        &self,
        request: Request<GetKnownPeersRequest>,
    ) -> Result<Response<GetKnownPeersResponse>> {
        let req = request.into_body();
        // Clamp peer-supplied limit: without this a caller could request
        // an unbounded peer dump (response-size amplification).
        let limit = req
            .limit
            .unwrap_or(self.config.max_peers_to_return)
            .min(self.config.max_peers_to_return);

        let state = self.state.read().unwrap();
        
        let own_info = state.our_info.clone();
        
        let known_peers: Vec<SignedNodeInfo> = state
            .known_peers
            .values()
            .take(limit)
            .cloned()
            .collect();

        Ok(Response::new(GetKnownPeersResponse {
            own_info,
            known_peers,
        }))
    }

    async fn push_peer_info(
        &self,
        request: Request<PushPeerInfoRequest>,
    ) -> Result<Response<PushPeerInfoResponse>> {
        let req = request.into_body();
        let info = req.info;

        // Authenticate the advertisement: anemo PeerIds ARE raw ed25519
        // public keys (see anemo crypto.rs peer_id_from_certificate), so the
        // entry must be self-signed by the key matching the claimed peer_id.
        // Previously this was a TODO and ANY peer could inject arbitrary
        // entries — including squatting honest peers' IDs with stale data
        // or flooding the table (unbounded memory growth, eclipse assist).
        let key = match ed25519_consensus::VerificationKey::try_from(info.info.peer_id.0) {
            Ok(k) => k,
            Err(_) => {
                return Ok(Response::new(PushPeerInfoResponse {
                    accepted: false,
                    reason: Some("Invalid peer public key".to_string()),
                }))
            }
        };
        if !info.verify(&key) {
            return Ok(Response::new(PushPeerInfoResponse {
                accepted: false,
                reason: Some("Invalid peer info signature".to_string()),
            }));
        }

        // Validate timestamp is not too old or in the future
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let skew = if info.info.timestamp_ms > now_ms {
            info.info.timestamp_ms - now_ms
        } else {
            now_ms - info.info.timestamp_ms
        };

        if skew > self.config.max_clock_skew_ms {
            return Ok(Response::new(PushPeerInfoResponse {
                accepted: false,
                reason: Some("Timestamp too far from current time".to_string()),
            }));
        }

        // Store the peer info
        {
            let mut state = self.state.write().unwrap();

            // Check if we already have newer info
            if let Some(existing) = state.known_peers.get(&info.info.peer_id) {
                if existing.info.timestamp_ms >= info.info.timestamp_ms {
                    return Ok(Response::new(PushPeerInfoResponse {
                        accepted: false,
                        reason: Some("Have newer info".to_string()),
                    }));
                }
            }

            // Bound the table: without a cap, validly-signed but
            // attacker-generated keypairs allow unbounded memory growth.
            // Evict the stalest entry when full (linear scan, only on the
            // insert-into-full-table path).
            if !state.known_peers.contains_key(&info.info.peer_id)
                && state.known_peers.len() >= self.config.max_known_peers
            {
                if let Some(stalest) = state
                    .known_peers
                    .iter()
                    .min_by_key(|(_, v)| v.info.timestamp_ms)
                    .map(|(k, _)| *k)
                {
                    state.known_peers.remove(&stalest);
                } else {
                    return Ok(Response::new(PushPeerInfoResponse {
                        accepted: false,
                        reason: Some("Peer table full".to_string()),
                    }));
                }
            }

            state.known_peers.insert(info.info.peer_id, info);
        }

        Ok(Response::new(PushPeerInfoResponse {
            accepted: true,
            reason: None,
        }))
    }
}

/// Wrapper to make Server work with anemo's routing
pub struct DiscoveryServer<T> {
    inner: T,
}

impl<T: Discovery> DiscoveryServer<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> T {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{NodeInfo, NodeType};
    use anemo::PeerId;

    fn test_server() -> Server {
        Server {
            state: Arc::new(RwLock::new(State {
                our_info: None,
                connected_peers: Default::default(),
                known_peers: Default::default(),
            })),
            config: DiscoveryConfig {
                max_peers_to_return: 2,
                max_known_peers: 2,
                ..Default::default()
            },
        }
    }

    fn signed_peer(node_id: &str) -> (ed25519_consensus::SigningKey, SignedNodeInfo) {
        let key = ed25519_consensus::SigningKey::new(rand::thread_rng());
        let peer_id = PeerId(key.verification_key().to_bytes());
        let info = NodeInfo::new(
            node_id.to_string(),
            peer_id,
            vec!["127.0.0.1:8000".to_string()],
            NodeType::Validator,
        );
        let signed = info.sign(&key);
        (key, signed)
    }

    #[tokio::test]
    async fn push_accepts_valid_self_signed_info() {
        let server = test_server();
        let (_, signed) = signed_peer("n1");
        let resp = server
            .push_peer_info(Request::new(PushPeerInfoRequest { info: signed }))
            .await
            .unwrap()
            .into_body();
        assert!(resp.accepted, "valid self-signed info must be accepted");
    }

    #[tokio::test]
    async fn push_rejects_spoofed_peer_id() {
        let server = test_server();
        // Sign with key A but claim key B's peer_id: classic ID-squat attempt.
        let key_a = ed25519_consensus::SigningKey::new(rand::thread_rng());
        let key_b = ed25519_consensus::SigningKey::new(rand::thread_rng());
        let peer_id_b = PeerId(key_b.verification_key().to_bytes());
        let info = NodeInfo::new(
            "victim".to_string(),
            peer_id_b,
            vec!["127.0.0.1:9000".to_string()],
            NodeType::Validator,
        );
        let spoofed = info.sign(&key_a);
        let resp = server
            .push_peer_info(Request::new(PushPeerInfoRequest { info: spoofed }))
            .await
            .unwrap()
            .into_body();
        assert!(
            !resp.accepted,
            "signature/key mismatch (peer-ID squat) must be rejected"
        );
    }

    #[tokio::test]
    async fn push_rejects_tampered_payload() {
        let server = test_server();
        let (_, mut signed) = signed_peer("n1");
        // Flip the address after signing: content tamper must invalidate.
        signed.info.addresses = vec!["9.9.9.9:1".to_string()];
        let resp = server
            .push_peer_info(Request::new(PushPeerInfoRequest { info: signed }))
            .await
            .unwrap()
            .into_body();
        assert!(!resp.accepted, "tampered payload must be rejected");
    }

    #[tokio::test]
    async fn peer_table_is_bounded_with_stalest_eviction() {
        let server = test_server(); // max_known_peers = 2
        for id in ["a", "b", "c"] {
            let (_, signed) = signed_peer(id);
            let resp = server
                .push_peer_info(Request::new(PushPeerInfoRequest { info: signed }))
                .await
                .unwrap()
                .into_body();
            assert!(resp.accepted);
        }
        let state = server.state.read().unwrap();
        assert_eq!(
            state.known_peers.len(),
            2,
            "table must stay within max_known_peers"
        );
    }

    #[tokio::test]
    async fn get_known_peers_clamps_attacker_limit() {
        let server = test_server(); // max_peers_to_return = 2
        for id in ["a", "b", "c", "d", "e"] {
            let (_, signed) = signed_peer(id);
            server
                .push_peer_info(Request::new(PushPeerInfoRequest { info: signed }))
                .await
                .unwrap();
        }
        let resp = server
            .get_known_peers(Request::new(GetKnownPeersRequest {
                limit: Some(usize::MAX),
                node_type_filter: None,
            }))
            .await
            .unwrap()
            .into_body();
        assert!(
            resp.known_peers.len() <= 2,
            "peer-supplied limit must be clamped to max_peers_to_return"
        );
    }
}
