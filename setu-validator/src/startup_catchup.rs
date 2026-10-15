//! Startup catch-up loop (v3 PR-3).
//!
//! Runs after `recover_from_storage()` + broadcaster wiring + seed-peer connect,
//! but before the HTTP ingress server is spawned. Pulls finalized CFs from peers
//! using PR-2's `StateSyncClient` and applies them through the same
//! `engine.receive_finalized_cf` path used by live consensus. Closes the
//! restart-window deadlock where a lagging node would otherwise block HTTP
//! writes indefinitely.
//!
//! Event pre-fetch is intentionally NOT done here. The engine's
//! `ensure_cf_events_available` (invoked inside `receive_finalized_cf`) already
//! fetches any missing events via the now-active broadcaster. See
//! `docs/feat/post-restart-finality-stall-v3/design.md` Revisions §R-PR3-1.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use setu_storage::CFStoreBackend;
use setu_types::ConsensusFrame;

use crate::network_adapter::{StateSyncClient, SyncError};
use consensus::ConsensusEngine;

/// Errors that abort the catch-up loop. On any of these the caller must NOT
/// expose HTTP readiness; operator action is required.
#[derive(Debug, thiserror::Error)]
pub enum CatchUpError {
    #[error("no forward progress within {0:?}")]
    NoProgress(Duration),
    #[error("wall-clock budget exceeded ({0:?})")]
    BudgetExceeded(Duration),
    #[error("apply CF failed: {0}")]
    ApplyFailed(String),
    #[error("cf pull failed: {0}")]
    CfPull(String),
    #[error("store error: {0}")]
    Store(String),
}

#[derive(Debug, Clone, Default)]
pub struct CatchUpStats {
    pub cfs_applied: u64,
    pub final_depth: u64,
    pub peer_highest_seen: u64,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
pub struct CatchUpConfig {
    /// Abort if no new finalized CF is applied within this window AND the
    /// peer claims to have CFs beyond our local depth.
    pub no_progress_timeout: Duration,
    /// Total wall-clock cap on the loop.
    pub wall_clock_budget: Duration,
    /// Pause between polls when the peer returned empty but we still believe
    /// catch-up should continue (peer claims higher depth than we have).
    pub idle_poll_interval: Duration,
}

impl Default for CatchUpConfig {
    fn default() -> Self {
        Self {
            no_progress_timeout: Duration::from_secs(30),
            wall_clock_budget: Duration::from_secs(60),
            idle_poll_interval: Duration::from_millis(250),
        }
    }
}

#[async_trait]
pub trait CatchUpSource: Send + Sync {
    async fn pull_finalized_after_depth(
        &self,
        after_depth: u64,
    ) -> Result<(Vec<ConsensusFrame>, u64), CatchUpError>;
}

#[async_trait]
pub trait CatchUpApplier: Send + Sync {
    async fn highest_finalized_depth(&self) -> Result<u64, CatchUpError>;
    async fn apply_cf(&self, cf: ConsensusFrame) -> Result<(), CatchUpError>;
}

/// Drive the catch-up loop to completion or error.
pub async fn run_startup_catch_up<S: CatchUpSource, A: CatchUpApplier>(
    source: &S,
    applier: &A,
    config: CatchUpConfig,
) -> Result<CatchUpStats, CatchUpError> {
    let start = Instant::now();
    let mut last_progress = start;
    let mut stats = CatchUpStats::default();

    loop {
        let elapsed = start.elapsed();
        if elapsed > config.wall_clock_budget {
            return Err(CatchUpError::BudgetExceeded(elapsed));
        }

        let local_depth = applier.highest_finalized_depth().await?;
        stats.final_depth = local_depth;

        let (cfs, peer_highest) = source.pull_finalized_after_depth(local_depth).await?;
        stats.peer_highest_seen = stats.peer_highest_seen.max(peer_highest);

        if cfs.is_empty() {
            // Peer at or below us → we are synced w.r.t. this peer set.
            if local_depth >= peer_highest {
                stats.elapsed = start.elapsed();
                info!(
                    cfs_applied = stats.cfs_applied,
                    final_depth = stats.final_depth,
                    peer_highest = stats.peer_highest_seen,
                    elapsed_ms = stats.elapsed.as_millis() as u64,
                    "startup catch-up completed (already in sync)"
                );
                return Ok(stats);
            }
            // Peer claims it has more but didn't send any → could be a
            // transient gap (peer mid-finalization). Wait then retry, but
            // honour the no-progress timeout.
            if last_progress.elapsed() > config.no_progress_timeout {
                return Err(CatchUpError::NoProgress(last_progress.elapsed()));
            }
            tokio::time::sleep(config.idle_poll_interval).await;
            continue;
        }

        // Defensive: server should already sort, but enforce ascending depth
        // because receive_finalized_cf walks the anchor chain root strictly.
        let mut sorted = cfs;
        sorted.sort_by_key(|cf| cf.anchor.depth);

        for cf in sorted {
            applier.apply_cf(cf).await?;
            stats.cfs_applied += 1;
            last_progress = Instant::now();
        }
    }
}

/// Production bridge from `StateSyncClient` (PR-2) to `CatchUpSource`.
pub struct StateSyncCatchUpSource {
    client: StateSyncClient,
}

impl StateSyncCatchUpSource {
    pub fn new(client: StateSyncClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl CatchUpSource for StateSyncCatchUpSource {
    async fn pull_finalized_after_depth(
        &self,
        after_depth: u64,
    ) -> Result<(Vec<ConsensusFrame>, u64), CatchUpError> {
        self.client
            .pull_finalized_after_depth(after_depth)
            .await
            .map_err(map_sync_err)
    }
}

fn map_sync_err(e: SyncError) -> CatchUpError {
    CatchUpError::CfPull(format!("{}", e))
}

/// Production bridge from `ConsensusEngine` + `CFStoreBackend` to `CatchUpApplier`.
pub struct EngineCatchUpApplier {
    pub engine: Arc<ConsensusEngine>,
    pub cf_store: Arc<dyn CFStoreBackend>,
}

#[async_trait]
impl CatchUpApplier for EngineCatchUpApplier {
    async fn highest_finalized_depth(&self) -> Result<u64, CatchUpError> {
        self.cf_store
            .highest_finalized_depth()
            .await
            .map_err(|e| CatchUpError::Store(format!("{:?}", e)))
    }

    async fn apply_cf(&self, cf: ConsensusFrame) -> Result<(), CatchUpError> {
        let cf_id = cf.id.clone();
        let depth = cf.anchor.depth;
        let round = cf.round;
        match self.engine.receive_finalized_cf(cf.clone()).await {
            Ok((applied, _anchor)) => {
                if applied {
                    info!(cf_id = %cf_id, depth, "startup catch-up applied CF");
                } else {
                    // Already finalized in-memory — treat as progress
                    // regardless because the local store now reflects this
                    // depth.
                    warn!(cf_id = %cf_id, depth, "CF already finalized locally during catch-up");
                }
                // Durably persist the CF so `highest_finalized_depth`
                // advances. WITHOUT this, the depth source never moves and
                // the loop re-pulls the same CFs until the wall-clock budget
                // kills catch-up (HTTP never starts) — every lagging node
                // would be stuck after any restart. `store` is idempotent
                // (keyed by cf_id), so re-persisting an already-finalized CF
                // is harmless.
                self.cf_store
                    .store(cf)
                    .await
                    .map_err(|e| CatchUpError::Store(format!("{:?}", e)))?;
                self.cf_store
                    .mark_finalized(&cf_id)
                    .await
                    .map_err(|e| CatchUpError::Store(format!("{:?}", e)))?;
                // Advance the local round to the chain tip. Otherwise the
                // node finishes catch-up at round 0 while the chain is at
                // round N, and every live proposal looks "ahead"
                // (NeedsCatchUp) forever. Monotonic: never move backwards.
                {
                    let vs = self.engine.validator_set_ref().read().await;
                    if round > vs.current_round() {
                        drop(vs);
                        self.engine
                            .validator_set_ref()
                            .write()
                            .await
                            .set_round(round);
                    }
                }
                Ok(())
            }
            Err(e) => Err(CatchUpError::ApplyFailed(format!("{}", e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use setu_types::{Anchor, CFStatus, ConsensusFrame};
    use setu_vlc::VLCSnapshot;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    // ── Test doubles ─────────────────────────────────────────────────────

    #[derive(Default)]
    struct MockSource {
        /// Pre-canned response queue. Each element is (cfs, peer_highest) for
        /// successive `pull_finalized_after_depth` calls.
        responses: Mutex<Vec<Result<(Vec<ConsensusFrame>, u64), CatchUpError>>>,
        calls: Mutex<Vec<u64>>,
    }

    impl MockSource {
        fn push_ok(&self, cfs: Vec<ConsensusFrame>, peer_highest: u64) {
            self.responses.lock().unwrap().push(Ok((cfs, peer_highest)));
        }
        fn push_err(&self, err: CatchUpError) {
            self.responses.lock().unwrap().push(Err(err));
        }
    }

    #[async_trait]
    impl CatchUpSource for MockSource {
        async fn pull_finalized_after_depth(
            &self,
            after_depth: u64,
        ) -> Result<(Vec<ConsensusFrame>, u64), CatchUpError> {
            self.calls.lock().unwrap().push(after_depth);
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                // No more pre-canned responses: assume "peer at our depth".
                return Ok((Vec::new(), after_depth));
            }
            q.remove(0)
        }
    }

    struct MockApplier {
        local_depth: AtomicU64,
        applied: Mutex<Vec<ConsensusFrame>>,
        /// If set, the Nth `apply_cf` call (0-indexed) returns Err.
        fail_at: Option<usize>,
        apply_delay: Duration,
    }

    impl MockApplier {
        fn new(start_depth: u64) -> Self {
            Self {
                local_depth: AtomicU64::new(start_depth),
                applied: Mutex::new(Vec::new()),
                fail_at: None,
                apply_delay: Duration::from_millis(0),
            }
        }
        fn with_fail_at(mut self, idx: usize) -> Self {
            self.fail_at = Some(idx);
            self
        }
        fn with_apply_delay(mut self, d: Duration) -> Self {
            self.apply_delay = d;
            self
        }
        fn applied_count(&self) -> usize {
            self.applied.lock().unwrap().len()
        }
        fn applied_depths(&self) -> Vec<u64> {
            self.applied.lock().unwrap().iter().map(|cf| cf.anchor.depth).collect()
        }
    }

    #[async_trait]
    impl CatchUpApplier for MockApplier {
        async fn highest_finalized_depth(&self) -> Result<u64, CatchUpError> {
            Ok(self.local_depth.load(Ordering::SeqCst))
        }
        async fn apply_cf(&self, cf: ConsensusFrame) -> Result<(), CatchUpError> {
            if !self.apply_delay.is_zero() {
                tokio::time::sleep(self.apply_delay).await;
            }
            let mut applied = self.applied.lock().unwrap();
            let idx = applied.len();
            if Some(idx) == self.fail_at {
                return Err(CatchUpError::ApplyFailed(format!("forced fail at {}", idx)));
            }
            let depth = cf.anchor.depth;
            applied.push(cf);
            self.local_depth.store(depth, Ordering::SeqCst);
            Ok(())
        }
    }

    fn fake_cf(depth: u64) -> ConsensusFrame {
        // Construct a minimally-populated CF. We never call verify_id in tests
        // since MockApplier short-circuits the real engine path.
        let anchor = Anchor {
            id: format!("anchor-{}", depth),
            event_ids: Vec::new(),
            vlc_snapshot: VLCSnapshot::default(),
            state_root: String::new(),
            merkle_roots: None,
            previous_anchor: None,
            depth,
            timestamp: depth * 1000,
        };
        ConsensusFrame {
            id: format!("cf-{}", depth),
            round: 0,
            anchor,
            proposer: "p".to_string(),
            status: CFStatus::Finalized,
            votes: HashMap::new(),
            created_at: depth * 1000,
            finalized_at: Some(depth * 1000 + 1),
        }
    }

    fn cfg_fast() -> CatchUpConfig {
        CatchUpConfig {
            no_progress_timeout: Duration::from_millis(200),
            wall_clock_budget: Duration::from_secs(5),
            idle_poll_interval: Duration::from_millis(20),
        }
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn already_synced_returns_immediately() {
        let src = MockSource::default();
        src.push_ok(Vec::new(), 5);
        let applier = MockApplier::new(5);

        let stats = run_startup_catch_up(&src, &applier, cfg_fast()).await.unwrap();
        assert_eq!(stats.cfs_applied, 0);
        assert_eq!(stats.final_depth, 5);
        assert_eq!(src.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn applies_cfs_in_ascending_depth_order() {
        let src = MockSource::default();
        // Server sent out-of-order: depths 3, 1, 2.
        src.push_ok(vec![fake_cf(3), fake_cf(1), fake_cf(2)], 3);
        // After applying, local_depth=3, next pull sees peer_highest=3 → done.
        src.push_ok(Vec::new(), 3);
        let applier = MockApplier::new(0);

        let stats = run_startup_catch_up(&src, &applier, cfg_fast()).await.unwrap();
        assert_eq!(stats.cfs_applied, 3);
        assert_eq!(applier.applied_depths(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn continues_across_multiple_batches() {
        let src = MockSource::default();
        let batch1: Vec<_> = (1..=64).map(fake_cf).collect();
        let batch2: Vec<_> = (65..=70).map(fake_cf).collect();
        src.push_ok(batch1, 70);
        src.push_ok(batch2, 70);
        src.push_ok(Vec::new(), 70);
        let applier = MockApplier::new(0);

        let stats = run_startup_catch_up(&src, &applier, cfg_fast()).await.unwrap();
        assert_eq!(stats.cfs_applied, 70);
        assert_eq!(stats.final_depth, 70);
        assert_eq!(src.calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn apply_failure_aborts_with_apply_failed() {
        let src = MockSource::default();
        src.push_ok(vec![fake_cf(1), fake_cf(2), fake_cf(3)], 3);
        let applier = MockApplier::new(0).with_fail_at(1);

        let err = run_startup_catch_up(&src, &applier, cfg_fast()).await.unwrap_err();
        assert!(matches!(err, CatchUpError::ApplyFailed(_)));
        assert_eq!(applier.applied_count(), 1);
    }

    #[tokio::test]
    async fn cf_pull_failure_aborts() {
        let src = MockSource::default();
        src.push_err(CatchUpError::CfPull("boom".into()));
        let applier = MockApplier::new(0);

        let err = run_startup_catch_up(&src, &applier, cfg_fast()).await.unwrap_err();
        assert!(matches!(err, CatchUpError::CfPull(_)));
    }

    #[tokio::test]
    async fn no_progress_timeout_triggers() {
        // Peer always claims depth=999 but never sends anything.
        let src = MockSource::default();
        for _ in 0..200 {
            src.push_ok(Vec::new(), 999);
        }
        let applier = MockApplier::new(0);
        let cfg = CatchUpConfig {
            no_progress_timeout: Duration::from_millis(80),
            wall_clock_budget: Duration::from_secs(5),
            idle_poll_interval: Duration::from_millis(15),
        };

        let err = run_startup_catch_up(&src, &applier, cfg).await.unwrap_err();
        assert!(matches!(err, CatchUpError::NoProgress(_)), "got {:?}", err);
    }

    #[tokio::test]
    async fn wall_clock_budget_triggers() {
        // 80 ms apply delay per CF, budget 50 ms → fail before first apply
        // completes the second loop iteration.
        let src = MockSource::default();
        let batch: Vec<_> = (1..=100).map(fake_cf).collect();
        src.push_ok(batch, 100);
        // Provide endless empties just in case.
        for _ in 0..10 {
            src.push_ok(Vec::new(), 100);
        }
        let applier = MockApplier::new(0).with_apply_delay(Duration::from_millis(80));
        let cfg = CatchUpConfig {
            no_progress_timeout: Duration::from_secs(10),
            wall_clock_budget: Duration::from_millis(50),
            idle_poll_interval: Duration::from_millis(5),
        };

        let err = run_startup_catch_up(&src, &applier, cfg).await.unwrap_err();
        assert!(matches!(err, CatchUpError::BudgetExceeded(_)), "got {:?}", err);
    }

    #[tokio::test]
    async fn empty_then_peer_catches_up_retries() {
        let src = MockSource::default();
        // First poll: peer claims higher but sends nothing.
        src.push_ok(Vec::new(), 5);
        // Second poll (after idle_poll): now has the batch.
        src.push_ok((1..=5).map(fake_cf).collect(), 5);
        // Third poll: synced.
        src.push_ok(Vec::new(), 5);
        let applier = MockApplier::new(0);

        let stats = run_startup_catch_up(&src, &applier, cfg_fast()).await.unwrap();
        assert_eq!(stats.cfs_applied, 5);
        assert!(src.calls.lock().unwrap().len() >= 3);
    }

    // Compile-time check: StateSyncCatchUpSource satisfies CatchUpSource.
    #[allow(dead_code)]
    fn _assert_state_sync_source_impls_catch_up_source() {
        fn _check<T: CatchUpSource>() {}
        _check::<StateSyncCatchUpSource>();
    }
}
