//! VLC (Vector Logical Clock) - Hybrid Logical Clock Implementation
//! 
//! This is a standalone, reusable vector logical clock library for tracking causal relationships in distributed systems.
//! 
//! # Design Philosophy
//! 
//! VLC combines three time concepts:
//! - **Vector Clock**: Captures causal relationships of distributed events
//! - **Logical Time**: Monotonically increasing logical timestamp
//! - **Physical Time**: Physical clock (for debugging and monitoring assistance)
//! 
//! # Use Cases
//! 
//! - Distributed event ordering
//! - Causal consistency detection
//! - Conflict detection and resolution
//! - Distributed snapshots
//! 
//! # Example
//! 
//! ```
//! use setu_vlc::{VectorClock, VLCSnapshot};
//! 
//! // Create a vector clock for a node
//! let mut vc = VectorClock::new();
//! vc.increment("node1");
//! 
//! // Create a VLC snapshot
//! let snapshot = VLCSnapshot::new_with_clock(vc);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vector Clock - Captures causal relationships of distributed events
/// 
/// Each node maintains a vector that records the latest logical time it knows about each node.
/// 
/// # Dynamic Node Changes Support
/// 
/// - **Node Join**: New nodes are automatically added through `merge()`
/// - **Node Leave**: Inactive nodes are removed through `remove_node()`
/// - **Node Restart**: Node clocks are reset through `reset_node()`
/// - **Garbage Collection**: Inactive nodes are cleaned up through `gc()` (based on timestamp or explicit marking)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorClock {
    /// Mapping from Node ID to Logical Time
    clocks: HashMap<String, u64>,
}

impl VectorClock {
    /// Create an empty vector clock
    pub fn new() -> Self {
        Self {
            clocks: HashMap::new(),
        }
    }
    
    /// Create a vector clock with specified node
    pub fn with_node(node_id: String) -> Self {
        let mut vc = Self::new();
        vc.clocks.insert(node_id, 0);
        vc
    }
    
    /// Increment the clock of specified node
    pub fn increment(&mut self, node_id: &str) -> u64 {
        let entry = self.clocks.entry(node_id.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }
    
    /// Get the clock value of specified node
    pub fn get(&self, node_id: &str) -> u64 {
        self.clocks.get(node_id).copied().unwrap_or(0)
    }
    
    /// Set the clock value of specified node
    pub fn set(&mut self, node_id: &str, value: u64) {
        self.clocks.insert(node_id.to_string(), value);
    }
    
    /// Merge two vector clocks (take the maximum value for each node)
    pub fn merge(&mut self, other: &VectorClock) {
        for (node_id, &time) in &other.clocks {
            let entry = self.clocks.entry(node_id.clone()).or_insert(0);
            *entry = (*entry).max(time);
        }
    }
    
    /// Check if this happens before another vector clock (causal order)
    /// 
    /// self < other if and only if:
    /// - For all nodes i: self[i] <= other[i]
    /// - There exists at least one node j: self[j] < other[j]
    pub fn happens_before(&self, other: &VectorClock) -> bool {
        let mut strictly_less = false;
        
        // Check all nodes in self
        for (node_id, &self_time) in &self.clocks {
            let other_time = other.get(node_id);
            if self_time > other_time {
                return false; // self cannot be greater than other
            }
            if self_time < other_time {
                strictly_less = true;
            }
        }
        
        // Check nodes in other but not in self
        for (node_id, &other_time) in &other.clocks {
            if !self.clocks.contains_key(node_id) && other_time > 0 {
                strictly_less = true;
            }
        }
        
        strictly_less
    }
    
    /// Check if two vector clocks are concurrent (no causal relationship)
    ///
    /// Two clocks are concurrent iff neither happens-before the other and they
    /// are not causally equal. This is decided directly by scanning the union of
    /// nodes with implicit-zero semantics (a missing node is treated as `0`),
    /// so clocks that differ only by explicit zero entries (e.g. `{a:0}` vs `{}`)
    /// are correctly treated as equal rather than concurrent.
    pub fn is_concurrent(&self, other: &VectorClock) -> bool {
        let mut self_greater = false;
        let mut other_greater = false;
        for node_id in self.clocks.keys().chain(other.clocks.keys()) {
            let s = self.get(node_id);
            let o = other.get(node_id);
            if s > o {
                self_greater = true;
            } else if s < o {
                other_greater = true;
            }
            if self_greater && other_greater {
                return true;
            }
        }
        self_greater && other_greater
    }
    
    /// Get all node IDs
    pub fn nodes(&self) -> Vec<&String> {
        self.clocks.keys().collect()
    }
    
    /// Get the size of the clock (number of nodes)
    pub fn len(&self) -> usize {
        self.clocks.len()
    }
    
    /// Check if the clock is empty
    pub fn is_empty(&self) -> bool {
        self.clocks.is_empty()
    }
    
    /// Remove specified node (for node leave scenario)
    /// 
    /// # Note
    /// 
    /// Removing a node affects causal relationship judgment. This method should only be called
    /// when it is certain that the node has permanently left and all related events have been processed.
    pub fn remove_node(&mut self, node_id: &str) -> Option<u64> {
        self.clocks.remove(node_id)
    }
    
    /// Reset the clock of specified node (for node restart scenario)
    /// 
    /// # Note
    /// 
    /// This will reset the node's clock to zero while keeping the node in the vector.
    /// Suitable for scenarios where the node restarts and rejoins the system.
    pub fn reset_node(&mut self, node_id: &str) {
        if self.clocks.contains_key(node_id) {
            self.clocks.insert(node_id.to_string(), 0);
        }
    }
    
    /// Garbage collection: remove all nodes with clock value of 0
    /// 
    /// This can be used to clean up nodes that have never been active or have been reset.
    pub fn gc_zero_nodes(&mut self) -> usize {
        let before = self.clocks.len();
        self.clocks.retain(|_, &mut v| v > 0);
        before - self.clocks.len()
    }
    
    /// Retain specified active node set, remove other nodes
    /// 
    /// This is a more aggressive garbage collection strategy for scenarios where
    /// the system maintains a list of known active nodes.
    /// 
    /// # Parameters
    /// 
    /// * `active_nodes` - Set of currently active node IDs
    /// 
    /// # Returns
    /// 
    /// Returns the number of nodes removed
    pub fn retain_active_nodes(&mut self, active_nodes: &[String]) -> usize {
        let before = self.clocks.len();
        let active_set: std::collections::HashSet<_> = active_nodes.iter().collect();
        self.clocks.retain(|k, _| active_set.contains(k));
        before - self.clocks.len()
    }
}

impl Default for VectorClock {
    fn default() -> Self {
        Self::new()
    }
}

/// VLC Snapshot - Snapshot of Hybrid Logical Clock
/// 
/// Contains three types of time information:
/// - Vector Clock: Causal relationships
/// - Logical Time: Monotonic logical time
/// - Physical Time: Physical clock (milliseconds)
/// 
/// # Dynamic Node Changes Handling
/// 
/// VLCSnapshot automatically handles node dynamic changes through its internal VectorClock:
/// - `receive()` operation automatically merges new nodes
/// - `gc_inactive_nodes()` can be used to clean up inactive nodes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VLCSnapshot {
    /// Vector clock
    pub vector_clock: VectorClock,
    
    /// Logical time (monotonically increasing)
    pub logical_time: u64,
    
    /// Physical time (Unix timestamp in milliseconds)
    pub physical_time: u64,
}

impl VLCSnapshot {
    /// Create a new VLC snapshot
    pub fn new() -> Self {
        Self {
            vector_clock: VectorClock::new(),
            logical_time: 0,
            physical_time: Self::current_physical_time(),
        }
    }
    
    /// Create a snapshot using the specified vector clock
    pub fn new_with_clock(vector_clock: VectorClock) -> Self {
        Self {
            vector_clock,
            logical_time: 0,
            physical_time: Self::current_physical_time(),
        }
    }
    
    /// Create a snapshot for the specified node
    pub fn for_node(node_id: String) -> Self {
        Self {
            vector_clock: VectorClock::with_node(node_id),
            logical_time: 0,
            physical_time: Self::current_physical_time(),
        }
    }
    
    /// Increment logical time and vector clock
    pub fn increment(&mut self, node_id: &str) {
        self.logical_time += 1;
        self.vector_clock.increment(node_id);
        self.physical_time = Self::current_physical_time();
    }
    
    /// Receive snapshot from another node and update local clock
    /// 
    /// This is the core operation of hybrid logical clock:
    /// - Merge vector clocks
    /// - logical_time = max(local_logical_time, received_logical_time) + 1
    pub fn receive(&mut self, other: &VLCSnapshot, local_node_id: &str) {
        // Merge vector clocks
        self.vector_clock.merge(&other.vector_clock);
        
        // Update logical time
        self.logical_time = self.logical_time.max(other.logical_time) + 1;
        
        // Increment local node's vector clock
        self.vector_clock.increment(local_node_id);
        
        // Update physical time
        self.physical_time = Self::current_physical_time();
    }
    
    /// Check if this happens before another snapshot
    pub fn happens_before(&self, other: &VLCSnapshot) -> bool {
        // First check vector clocks
        if self.vector_clock.happens_before(&other.vector_clock) {
            return true;
        }
        
        // If vector clocks are equal, use logical time
        if self.vector_clock == other.vector_clock {
            return self.logical_time < other.logical_time;
        }
        
        false
    }
    
    /// Check if two snapshots are concurrent
    pub fn is_concurrent(&self, other: &VLCSnapshot) -> bool {
        self.vector_clock.is_concurrent(&other.vector_clock)
    }
    
    /// Garbage collection: remove inactive nodes
    /// 
    /// # Parameters
    /// 
    /// * `active_nodes` - List of currently active node IDs
    /// 
    /// # Returns
    /// 
    /// Returns the number of nodes removed
    pub fn gc_inactive_nodes(&mut self, active_nodes: &[String]) -> usize {
        self.vector_clock.retain_active_nodes(active_nodes)
    }
    
    /// Get current physical time (milliseconds)
    fn current_physical_time() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
}

impl Default for VLCSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod is_concurrent_tests {
    use super::*;

    #[test]
    fn causally_equal_clocks_are_not_concurrent_regression() {
        // Regression for the explicit-zero bug: {a:0} and {} are the same causal
        // state and must NOT be reported as concurrent.
        let seeded = VectorClock::with_node("a".to_string()); // {a:0}
        let empty = VectorClock::new(); // {}
        assert!(!seeded.is_concurrent(&empty));
        assert!(!empty.is_concurrent(&seeded));

        // {n:3} vs {n:3, m:0} — equal up to an implicit zero.
        let mut x = VectorClock::new();
        x.set("n", 3);
        let mut y = VectorClock::new();
        y.set("n", 3);
        y.set("m", 0);
        assert!(!x.is_concurrent(&y));
        assert!(!y.is_concurrent(&x));
    }

    #[test]
    fn genuinely_divergent_clocks_are_still_concurrent() {
        let mut a = VectorClock::new();
        a.set("n1", 2);
        let mut b = VectorClock::new();
        b.set("n2", 2);
        assert!(a.is_concurrent(&b));
        assert!(b.is_concurrent(&a));
    }

    #[test]
    fn ordered_clocks_are_not_concurrent() {
        let mut a = VectorClock::new();
        a.set("n1", 1);
        let mut b = VectorClock::new();
        b.set("n1", 2);
        assert!(!a.is_concurrent(&b));
        assert!(!b.is_concurrent(&a));
    }

    #[test]
    fn is_concurrent_is_symmetric_over_mixed_cases() {
        let mut a = VectorClock::new();
        a.set("x", 5);
        a.set("y", 1);
        let mut b = VectorClock::new();
        b.set("x", 2);
        b.set("y", 4);
        // x: a>b, y: a<b => concurrent, both directions.
        assert_eq!(a.is_concurrent(&b), b.is_concurrent(&a));
        assert!(a.is_concurrent(&b));
    }

    #[test]
    fn empty_clocks_are_not_concurrent() {
        assert!(!VectorClock::new().is_concurrent(&VectorClock::new()));
    }
}
