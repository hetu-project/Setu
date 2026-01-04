//! Routing strategies for solver selection
//!
//! MVP provides two strategies:
//! - ConsistentHashStrategy: Deterministic routing based on resource keys (default)
//! - LoadBalancedStrategy: Routes to least loaded solver

use blake3::Hasher;
use parking_lot::RwLock;
use std::collections::BTreeMap;
use tracing::trace;

use crate::error::RouterError;
use crate::solver::SolverInfo;

/// Trait for routing strategies
pub trait RoutingStrategy: Send + Sync {
    /// Select a solver from available solvers based on routing key
    fn select(&self, available: &[SolverInfo], routing_key: &str) -> Result<SolverInfo, RouterError>;
    
    /// Strategy name for logging
    fn name(&self) -> &'static str;
}

/// Consistent hash routing strategy with cached hash ring
///
/// Ensures transactions with the same resources are routed to the same solver,
/// which helps with caching and reduces cross-solver coordination.
pub struct ConsistentHashStrategy {
    /// Number of virtual nodes per solver for better distribution
    virtual_nodes: u32,
    /// Cached hash ring: (solvers_hash, ring)
    /// The ring maps hash values to solver indices
    ring_cache: RwLock<Option<(u64, BTreeMap<u64, usize>)>>,
}

impl ConsistentHashStrategy {
    /// Create a new consistent hash strategy with default 150 virtual nodes
    pub fn new() -> Self {
        Self::with_virtual_nodes(150)
    }

    /// Create with custom virtual node count
    pub fn with_virtual_nodes(virtual_nodes: u32) -> Self {
        Self {
            virtual_nodes,
            ring_cache: RwLock::new(None),
        }
    }

    /// Hash a string key using blake3
    fn hash_key(key: &str) -> u64 {
        let mut hasher = Hasher::new();
        hasher.update(key.as_bytes());
        let hash = hasher.finalize();
        let bytes = hash.as_bytes();
        u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
        ])
    }

    /// Compute a hash of the solver list for cache invalidation
    fn solvers_hash(solvers: &[SolverInfo]) -> u64 {
        let mut hasher = Hasher::new();
        for s in solvers {
            hasher.update(s.id.as_bytes());
        }
        let hash = hasher.finalize();
        let bytes = hash.as_bytes();
        u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
        ])
    }

    /// Get or build the hash ring, using cache if available
    fn get_or_build_ring(&self, solvers: &[SolverInfo]) -> BTreeMap<u64, usize> {
        let current_hash = Self::solvers_hash(solvers);

        // Check if cache is valid
        {
            let cache = self.ring_cache.read();
            if let Some((cached_hash, ring)) = cache.as_ref() {
                if *cached_hash == current_hash {
                    return ring.clone();
                }
            }
        }

        // Build new ring
        let mut ring = BTreeMap::new();
        for (idx, solver) in solvers.iter().enumerate() {
            for vn in 0..self.virtual_nodes {
                let key = format!("{}:{}", solver.id, vn);
                let hash = Self::hash_key(&key);
                ring.insert(hash, idx);
            }
        }

        // Cache the new ring
        *self.ring_cache.write() = Some((current_hash, ring.clone()));
        ring
    }

    /// Find solver index in the ring for a given hash
    fn find_in_ring(ring: &BTreeMap<u64, usize>, hash: u64) -> Option<usize> {
        if ring.is_empty() {
            return None;
        }
        
        // Find the first node >= hash, or wrap around to first
        ring.range(hash..)
            .next()
            .or_else(|| ring.iter().next())
            .map(|(_, &idx)| idx)
    }
}

impl Default for ConsistentHashStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl RoutingStrategy for ConsistentHashStrategy {
    fn select(&self, available: &[SolverInfo], routing_key: &str) -> Result<SolverInfo, RouterError> {
        if available.is_empty() {
            return Err(RouterError::NoSolverAvailable);
        }

        if available.len() == 1 {
            return Ok(available[0].clone());
        }

        let ring = self.get_or_build_ring(available);
        let hash = Self::hash_key(routing_key);
        
        trace!(routing_key = %routing_key, hash = %hash, "Consistent hash lookup");

        let idx = Self::find_in_ring(&ring, hash)
            .ok_or(RouterError::NoSolverAvailable)?;

        Ok(available[idx].clone())
    }

    fn name(&self) -> &'static str {
        "ConsistentHash"
    }
}

/// Load-balanced routing strategy
///
/// Selects the solver with the lowest current load.
/// Falls back to weighted selection if all solvers are above threshold.
pub struct LoadBalancedStrategy {
    /// Load threshold for weighted selection
    load_threshold: f64,
}

impl LoadBalancedStrategy {
    /// Create a new load balanced strategy
    pub fn new() -> Self {
        Self { load_threshold: 0.9 }
    }

    /// Create with custom load threshold
    pub fn with_threshold(threshold: f64) -> Self {
        Self { load_threshold: threshold }
    }
}

impl Default for LoadBalancedStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl RoutingStrategy for LoadBalancedStrategy {
    fn select(&self, available: &[SolverInfo], _routing_key: &str) -> Result<SolverInfo, RouterError> {
        if available.is_empty() {
            return Err(RouterError::NoSolverAvailable);
        }

        // First, try to find solvers below threshold
        let candidates: Vec<_> = available
            .iter()
            .filter(|s| s.load_ratio() < self.load_threshold)
            .collect();

        if !candidates.is_empty() {
            // Select based on weighted capacity: (1 - load) * weight
            let solver = candidates
                .into_iter()
                .max_by(|a, b| {
                    let score_a = (1.0 - a.load_ratio()) * a.weight as f64;
                    let score_b = (1.0 - b.load_ratio()) * b.weight as f64;
                    score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
            
            trace!(solver_id = %solver.id, load = %solver.load_ratio(), "Selected by weighted capacity");
            return Ok(solver.clone());
        }

        // Fallback: select least loaded
        let solver = available
            .iter()
            .min_by(|a, b| {
                a.load_ratio()
                    .partial_cmp(&b.load_ratio())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        
        trace!(solver_id = %solver.id, load = %solver.load_ratio(), "Selected least loaded");
        Ok(solver.clone())
    }

    fn name(&self) -> &'static str {
        "LoadBalanced"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_solvers(count: usize) -> Vec<SolverInfo> {
        (1..=count)
            .map(|i| SolverInfo::new(format!("solver-{}", i), format!("127.0.0.1:{}", 9000 + i)))
            .collect()
    }

    #[test]
    fn test_consistent_hash_deterministic() {
        let strategy = ConsistentHashStrategy::default();
        let solvers = create_test_solvers(6);

        let result1 = strategy.select(&solvers, "account:alice").unwrap();
        let result2 = strategy.select(&solvers, "account:alice").unwrap();

        assert_eq!(result1.id, result2.id, "Same key should route to same solver");
    }

    #[test]
    fn test_consistent_hash_distribution() {
        let strategy = ConsistentHashStrategy::default();
        let solvers = create_test_solvers(6);

        let mut distribution = std::collections::HashMap::new();

        for i in 0..1000 {
            let key = format!("resource:{}", i);
            let result = strategy.select(&solvers, &key).unwrap();
            *distribution.entry(result.id).or_insert(0) += 1;
        }

        // All 6 solvers should receive traffic
        assert_eq!(distribution.len(), 6);

        // Check reasonable distribution (each solver gets roughly 10-30%)
        for count in distribution.values() {
            assert!(*count > 50 && *count < 300, "count={} is outside expected range", count);
        }
    }

    #[test]
    fn test_consistent_hash_cache() {
        let strategy = ConsistentHashStrategy::default();
        let solvers = create_test_solvers(6);

        // First call builds cache
        let _ = strategy.select(&solvers, "key1").unwrap();
        
        // Second call should use cache
        let _ = strategy.select(&solvers, "key2").unwrap();

        // Verify cache exists
        let cache = strategy.ring_cache.read();
        assert!(cache.is_some());
    }

    #[test]
    fn test_load_balanced_selection() {
        let strategy = LoadBalancedStrategy::default();
        let mut solvers = create_test_solvers(3);

        // Set different loads
        solvers[0].pending_load = 100;
        solvers[1].pending_load = 50;  // Lowest load
        solvers[2].pending_load = 200;

        let result = strategy.select(&solvers, "any").unwrap();

        // Should select solver with lowest load
        assert_eq!(result.id, "solver-2");
    }

    #[test]
    fn test_empty_solvers() {
        let strategy = ConsistentHashStrategy::default();
        let result = strategy.select(&[], "key");
        assert!(matches!(result, Err(RouterError::NoSolverAvailable)));
    }

    #[test]
    fn test_single_solver() {
        let strategy = ConsistentHashStrategy::default();
        let solvers = create_test_solvers(1);

        let result = strategy.select(&solvers, "any_key").unwrap();
        assert_eq!(result.id, "solver-1");
    }
}
