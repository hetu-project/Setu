//! Message Router
//!
//! Routes incoming network events to appropriate handlers in the consensus layer.
//! Events and CFs are stored in-memory (DAG) until finalized, then persisted.

use consensus::{CfReceiveOutcome, ConsensusEngine};
use crate::protocol::NetworkEvent;
use setu_storage::{AnchorStoreBackend, CFStoreBackend, EventStoreBackend};
use setu_types::{ConsensusFrame, Event, Vote};
use crate::persistence::FinalizationPersister;
use crate::network_adapter::StateSyncClient;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Trait for handling network events
///
/// This trait allows decoupling the network layer from the consensus layer.
/// Implementations receive network events and route them to appropriate handlers.
#[async_trait::async_trait]
pub trait NetworkEventHandler: Send + Sync {
    /// Handle an incoming event from the network
    async fn handle_event(&self, peer_id: String, event: Event);
    
    /// Handle an incoming CF proposal
    async fn handle_cf_proposal(&self, peer_id: String, cf: ConsensusFrame);
    
    /// Handle an incoming vote
    async fn handle_vote(&self, peer_id: String, vote: Vote);
    
    /// Handle CF finalized notification
    async fn handle_cf_finalized(&self, peer_id: String, cf: ConsensusFrame);
    
    /// Handle peer connected event
    async fn handle_peer_connected(&self, peer_id: String);
    
    /// Handle peer disconnected event
    async fn handle_peer_disconnected(&self, peer_id: String);
}

/// Message router that directs network events to the consensus engine
/// 
/// The router handles:
/// 1. Adding events to the DAG via ConsensusEngine (in-memory until finalized)
/// 2. Routing CF proposals and votes to consensus
/// 3. Persisting finalized data (Anchor + Events) when CF reaches quorum
/// 
/// Note: Events are NOT persisted immediately. Persistence happens when CF is finalized,
/// which batch-persists the Anchor and all Events included in the CF.
pub struct MessageRouter {
    /// The consensus engine to route messages to
    engine: Arc<ConsensusEngine>,
    /// Event store for persisting finalized events
    event_store: Arc<dyn EventStoreBackend>,
    /// Anchor store for persisting finalized anchors
    anchor_store: Arc<dyn AnchorStoreBackend>,
    /// CF store for persisting finalized CF indexes
    cf_store: Arc<dyn CFStoreBackend>,
    /// Per-CF index-persistence retry counter (Layer D, retry-then-escalate).
    /// Initialized empty; entries are added on failure and removed on success.
    cf_index_retries: Arc<parking_lot::Mutex<std::collections::HashMap<setu_types::CFId, u32>>>,
    /// PR-4: optional state-sync client used when `receive_cf` returns
    /// `NeedsCatchUp`. `None` when the validator is single-node or running in
    /// a test harness without a peer mesh; the catch-up branch becomes a no-op
    /// warn in that case.
    state_sync_client: Option<Arc<StateSyncClient>>,
    /// PR-4: serializes concurrent catch-up attempts. At most one catch-up runs
    /// at a time per router; overlapping CFs that trigger NeedsCatchUp while a
    /// catch-up is in flight are dropped (peer will rebroadcast).
    catch_up_in_progress: Arc<AtomicBool>,
}

impl MessageRouter {
    /// Create a new message router with storage for persistence
    pub fn new(
        engine: Arc<ConsensusEngine>,
        event_store: Arc<dyn EventStoreBackend>,
        anchor_store: Arc<dyn AnchorStoreBackend>,
        cf_store: Arc<dyn CFStoreBackend>,
    ) -> Self {
        Self {
            engine,
            event_store,
            anchor_store,
            cf_store,
            cf_index_retries: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            state_sync_client: None,
            catch_up_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }

    /// PR-4: attach an optional state-sync client so that `receive_cf` results
    /// with [`CfReceiveOutcome::NeedsCatchUp`] can trigger an in-place catch-up.
    pub fn with_state_sync_client(mut self, client: Arc<StateSyncClient>) -> Self {
        self.state_sync_client = Some(client);
        self
    }
    
    /// Start the message router event loop
    ///
    /// This spawns a task that consumes network events and routes them
    /// to the appropriate handlers.
    pub fn start(
        self: Arc<Self>,
        mut event_rx: mpsc::Receiver<NetworkEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!("Message router started");
            
            while let Some(event) = event_rx.recv().await {
                self.route_event(event).await;
            }
            
            info!("Message router stopped");
        })
    }
    
    /// Route a single network event to the appropriate handler
    async fn route_event(&self, event: NetworkEvent) {
        match event {
            NetworkEvent::EventReceived { peer_id, event } => {
                self.handle_event(peer_id, event).await;
            }
            NetworkEvent::CFProposal { peer_id, cf } => {
                self.handle_cf_proposal(peer_id, cf).await;
            }
            NetworkEvent::VoteReceived { peer_id, vote } => {
                self.handle_vote(peer_id, vote).await;
            }
            NetworkEvent::CFFinalized { peer_id, cf } => {
                self.handle_cf_finalized(peer_id, cf).await;
            }
            NetworkEvent::PeerConnected { peer_id, node_info } => {
                debug!(peer = %peer_id, node = ?node_info, "Peer connected");
                self.handle_peer_connected(peer_id).await;
            }
            NetworkEvent::PeerDisconnected { peer_id } => {
                debug!(peer = %peer_id, "Peer disconnected");
                self.handle_peer_disconnected(peer_id).await;
            }
        }
    }

    /// PR-4: in-place catch-up triggered when [`receive_cf`] reports
    /// `NeedsCatchUp`. Spawns a background task that runs the same
    /// `run_startup_catch_up` loop as PR-3 startup, then releases the guard.
    /// Concurrent triggers (overlapping NeedsCatchUp from multiple peers) are
    /// coalesced via [`AtomicBool`] CAS — the loser drops; the leader's
    /// catch-up pass covers the gap on its behalf.
    ///
    /// The original CF is intentionally not re-enqueued: the leader's
    /// heartbeat or next-round proposal will deliver a fresh CF whose round
    /// matches the post-catch-up local view.
    async fn trigger_catch_up_for_cf(&self, cf: ConsensusFrame, up_to_depth: u64) {
        let client = match &self.state_sync_client {
            Some(c) => c.clone(),
            None => {
                warn!(
                    cf_id = %cf.id,
                    cf_round = cf.round,
                    up_to_depth,
                    "NeedsCatchUp returned but no state_sync_client wired; dropping CF"
                );
                return;
            }
        };

        if self
            .catch_up_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            debug!(
                cf_id = %cf.id,
                cf_round = cf.round,
                "catch-up already in progress; dropping NeedsCatchUp trigger"
            );
            return;
        }

        info!(
            cf_id = %cf.id,
            cf_round = cf.round,
            up_to_depth,
            "Triggering in-place catch-up after NeedsCatchUp outcome"
        );

        let engine = self.engine.clone();
        let cf_store = self.cf_store.clone();
        let flag = self.catch_up_in_progress.clone();

        tokio::spawn(async move {
            let source = crate::startup_catchup::StateSyncCatchUpSource::new(
                client.as_ref().clone(),
            );
            let applier = crate::startup_catchup::EngineCatchUpApplier { engine, cf_store };
            let config = crate::startup_catchup::CatchUpConfig::default();
            match crate::startup_catchup::run_startup_catch_up(&source, &applier, config).await {
                Ok(stats) => {
                    info!(
                        cfs_applied = stats.cfs_applied,
                        final_depth = stats.final_depth,
                        peer_highest = stats.peer_highest_seen,
                        elapsed_ms = stats.elapsed.as_millis() as u64,
                        "in-place catch-up completed"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "in-place catch-up failed");
                }
            }
            flag.store(false, Ordering::Release);
        });
    }
}

/// Implement FinalizationPersister for MessageRouter
/// 
/// This provides the shared persist_finalized_anchor() implementation
#[async_trait::async_trait]
impl FinalizationPersister for MessageRouter {
    fn engine(&self) -> &Arc<ConsensusEngine> {
        &self.engine
    }
    
    fn event_store(&self) -> &Arc<dyn EventStoreBackend> {
        &self.event_store
    }
    
    fn anchor_store(&self) -> &Arc<dyn AnchorStoreBackend> {
        &self.anchor_store
    }

    fn cf_store(&self) -> &Arc<dyn CFStoreBackend> {
        &self.cf_store
    }

    fn cf_index_retries(&self) -> &Arc<parking_lot::Mutex<std::collections::HashMap<setu_types::CFId, u32>>> {
        &self.cf_index_retries
    }
}

#[async_trait::async_trait]
impl NetworkEventHandler for MessageRouter {
    async fn handle_event(&self, peer_id: String, event: Event) {
        debug!(
            event_id = %event.id,
            from = %peer_id,
            "Routing event to consensus engine"
        );
        
        // Step 1: Verify event ID matches content (anti-tampering)
        if !event.verify_id() {
            warn!(
                event_id = %event.id,
                from = %peer_id,
                "Event ID verification failed - possible tampering, rejecting"
            );
            return;
        }
        
        // Step 2: Add to consensus DAG (in-memory only)
        // Events are persisted later when CF is finalized
        // Note: receive_event_from_network is idempotent - duplicate events return Ok
        match self.engine.receive_event_from_network(event.clone()).await {
            Ok(event_id) => {
                debug!(
                    event_id = %event_id,
                    "Event successfully added to DAG (pending finalization)"
                );
            }
            Err(e) => {
                let error_str = e.to_string();
                
                // Check if this is a MissingParent error
                if error_str.contains("Missing parent") {
                    warn!(
                        event_id = %event.id,
                        from = %peer_id,
                        parents = ?event.parent_ids,
                        "Event has missing parents, attempting sync"
                    );
                    
                    // Attempt to fetch missing parent events from the broadcaster
                    if let Some(broadcaster) = self.engine.get_broadcaster().await {
                        match broadcaster.request_events(&event.parent_ids).await {
                            Ok(fetched_events) => {
                                debug!(
                                    count = fetched_events.len(),
                                    "Fetched missing parent events"
                                );
                                
                                // Add fetched parents to DAG. Each fetched
                                // parent MUST pass verify_id like any
                                // network event: the serving peer is
                                // untrusted, and an ID-mismatched parent
                                // accepted here (but rejected by peers that
                                // got it via broadcast) would split our DAG
                                // from the network's. Reject loudly.
                                for parent_event in fetched_events {
                                    if !parent_event.verify_id() {
                                        warn!(
                                            event_id = %parent_event.id,
                                            "Fetched parent failed ID verification - possible tampering, rejecting"
                                        );
                                        continue;
                                    }
                                    if let Err(e) = self.engine.receive_event_from_network(parent_event).await {
                                        debug!(error = %e, "Failed to add fetched parent (may already exist)");
                                    }
                                }
                                
                                // Retry adding the original event
                                match self.engine.receive_event_from_network(event).await {
                                    Ok(event_id) => {
                                        debug!(
                                            event_id = %event_id,
                                            "Event added after parent sync"
                                        );
                                    }
                                    Err(retry_err) => {
                                        warn!(
                                            error = %retry_err,
                                            "Event still failed after parent sync"
                                        );
                                    }
                                }
                            }
                            Err(fetch_err) => {
                                warn!(
                                    error = %fetch_err,
                                    "Failed to fetch missing parent events"
                                );
                            }
                        }
                    } else {
                        warn!("No broadcaster available for parent sync");
                    }
                } else {
                    warn!(
                        event_id = %event.id,
                        error = %e,
                        "Failed to add event to DAG"
                    );
                }
            }
        }
    }
    
    async fn handle_cf_proposal(&self, peer_id: String, cf: ConsensusFrame) {
        info!(
            cf_id = %cf.id,
            proposer = %cf.proposer,
            from = %peer_id,
            "Routing CF proposal to consensus engine"
        );
        
        let cf_id = cf.id.clone();
        match self.engine.receive_cf(cf).await {
            Ok(CfReceiveOutcome::Accepted { finalized, anchor }) => {
                if finalized {
                    info!(
                        cf_id = %cf_id,
                        anchor = ?anchor.as_ref().map(|a| &a.id),
                        "CF finalized after local vote"
                    );
                    
                    // Persist finalized data (same logic as handle_vote)
                    if let Some(anchor) = anchor {
                        match self.persist_finalized_anchor(&anchor).await {
                            Ok(()) => {
                                if let Err(e) = self.engine.complete_pending_finalizations().await {
                                    warn!(error = %e, "complete_pending_finalizations failed after handle_cf_proposal persist");
                                }
                            }
                            Err(e) => {
                                warn!(
                                    anchor_id = %anchor.id,
                                    error = %e,
                                    "Failed to persist finalized anchor (will retry)"
                                );
                            }
                        }
                    }
                } else {
                    debug!(cf_id = %cf_id, "CF proposal processed, awaiting votes");
                }
            }
            Ok(CfReceiveOutcome::NeedsCatchUp { up_to_depth, cf }) => {
                self.trigger_catch_up_for_cf(cf, up_to_depth).await;
            }
            Ok(CfReceiveOutcome::Stale { cf_round, local_round }) => {
                debug!(
                    cf_id = %cf_id,
                    cf_round,
                    local_round,
                    "Dropping stale CF proposal (round behind local)"
                );
            }
            Err(e) => {
                warn!(
                    cf_id = %cf_id,
                    error = %e,
                    "Failed to process CF proposal"
                );
            }
        }
    }
    
    async fn handle_vote(&self, peer_id: String, vote: Vote) {
        debug!(
            cf_id = %vote.cf_id,
            voter = %vote.validator_id,
            from = %peer_id,
            "Routing vote to consensus engine"
        );
        
        match self.engine.receive_vote(vote.clone()).await {
            Ok((finalized, anchor)) => {
                if finalized {
                    info!(
                        cf_id = %vote.cf_id,
                        anchor = ?anchor.as_ref().map(|a| &a.id),
                        "CF finalized after vote"
                    );
                    
                    // Persist finalized data
                    if let Some(anchor) = anchor {
                        match self.persist_finalized_anchor(&anchor).await {
                            Ok(()) => {
                                if let Err(e) = self.engine.complete_pending_finalizations().await {
                                    warn!(error = %e, "complete_pending_finalizations failed after handle_vote persist");
                                }
                            }
                            Err(e) => {
                                warn!(
                                    anchor_id = %anchor.id,
                                    error = %e,
                                    "Failed to persist finalized anchor (will retry)"
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!(
                    cf_id = %vote.cf_id,
                    error = %e,
                    "Failed to process vote"
                );
            }
        }
    }
    
    async fn handle_cf_finalized(&self, peer_id: String, cf: ConsensusFrame) {
        info!(
            cf_id = %cf.id,
            from = %peer_id,
            "Received CF finalized notification"
        );
        let cf_id = cf.id.clone();
        match self.engine.receive_finalized_cf(cf).await {
            Ok((finalized, anchor)) => {
                if finalized {
                    info!(
                        cf_id = %cf_id,
                        anchor = ?anchor.as_ref().map(|a| &a.id),
                        "CF finalized after peer notification"
                    );

                    if let Some(anchor) = anchor {
                        match self.persist_finalized_anchor(&anchor).await {
                            Ok(()) => {
                                if let Err(e) = self.engine.complete_pending_finalizations().await {
                                    warn!(error = %e, "complete_pending_finalizations failed after handle_cf_finalized persist");
                                }
                            }
                            Err(e) => {
                                warn!(
                                    anchor_id = %anchor.id,
                                    error = %e,
                                    "Failed to persist finalized anchor from peer notification"
                                );
                            }
                        }
                    }
                } else {
                    debug!(cf_id = %cf_id, "Finalized CF notification was already applied or pending");
                }
            }
            Err(e) => {
                warn!(
                    cf_id = %cf_id,
                    error = %e,
                    "Failed to process finalized CF notification"
                );
            }
        }
    }
    
    async fn handle_peer_connected(&self, peer_id: String) {
        debug!(peer = %peer_id, "New peer connected - may trigger sync");
        // Could trigger state sync with new peer here
    }
    
    async fn handle_peer_disconnected(&self, peer_id: String) {
        debug!(peer = %peer_id, "Peer disconnected");
        // Could update peer tracking state here
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use consensus::{ConsensusEngine, ValidatorSet};
    use setu_storage::{AnchorStore, CFStore, EventStore};
    use setu_types::{ConsensusConfig, ValidatorInfo, NodeInfo, VLCSnapshot};
    use setu_vlc::VectorClock;
    
    fn create_test_stores() -> (Arc<dyn EventStoreBackend>, Arc<dyn AnchorStoreBackend>, Arc<dyn CFStoreBackend>) {
        let event_store: Arc<dyn EventStoreBackend> = Arc::new(EventStore::new());
        let anchor_store: Arc<dyn AnchorStoreBackend> = Arc::new(AnchorStore::new());
        let cf_store: Arc<dyn CFStoreBackend> = Arc::new(CFStore::new());
        (event_store, anchor_store, cf_store)
    }
    
    fn create_test_engine() -> Arc<ConsensusEngine> {
        let config = ConsensusConfig::default();
        let mut validator_set = ValidatorSet::new();
        let node_info = NodeInfo::new_validator("v1".to_string(), "127.0.0.1".to_string(), 8080);
        validator_set.add_validator(ValidatorInfo::new(node_info, false));
        Arc::new(ConsensusEngine::new(
            config,
            "v1".to_string(),
            validator_set,
        ))
    }
    
    fn create_test_event() -> Event {
        Event::genesis(
            "test-creator".to_string(),
            VLCSnapshot {
                vector_clock: VectorClock::new(),
                logical_time: 0,
                physical_time: 0,
            },
        )
    }
    
    #[tokio::test]
    async fn test_router_creation() {
        let engine = create_test_engine();
        let (event_store, anchor_store, cf_store) = create_test_stores();
        let _router = MessageRouter::new(engine, event_store, anchor_store, cf_store);
        // Router created successfully
    }
    
    #[tokio::test]
    async fn test_handle_event_adds_to_dag() {
        let engine = create_test_engine();
        let (event_store, anchor_store, cf_store) = create_test_stores();
        let router = MessageRouter::new(engine.clone(), event_store, anchor_store, cf_store);
        
        let event = create_test_event();
        let event_id = event.id.clone();
        
        // Handle the event - adds to DAG only, not persisted
        router.handle_event("peer-1".to_string(), event).await;
        
        // Verify it was added to the DAG
        let events = engine.get_events_by_ids(&[event_id.clone()]).await;
        assert_eq!(events.len(), 1, "Event should be in DAG");
        assert_eq!(events[0].id, event_id);
    }

    /// Fetched-parent path must enforce the same anti-tampering gate as
    /// direct broadcasts: a tampered event (ID no longer matches content
    /// after the V2 content-binding fix) must never enter the DAG, whether
    /// it arrives via broadcast or via missing-parent sync.
    #[tokio::test]
    async fn test_handle_event_rejects_tampered_event() {
        let engine = create_test_engine();
        let (event_store, anchor_store, cf_store) = create_test_stores();
        let router = MessageRouter::new(engine.clone(), event_store, anchor_store, cf_store);

        let mut event = create_test_event();
        let event_id = event.id.clone();
        // Tamper AFTER sealing: swap the payload without re-sealing.
        event.payload = setu_types::EventPayload::ContractCall {
            target: "0xevil::x::f".to_string(),
            args: vec![],
        };
        assert!(!event.verify_id(), "sanity: tampered event must fail verify");

        router.handle_event("peer-1".to_string(), event).await;

        let events = engine.get_events_by_ids(&[event_id]).await;
        assert!(
            events.is_empty(),
            "Tampered event must be rejected before DAG admission"
        );
    }
}
