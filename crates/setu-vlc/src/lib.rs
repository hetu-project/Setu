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
    pub fn is_concurrent(&self, other: &VectorClock) -> bool {
        !self.happens_before(other) && !other.happens_before(self) && self != other
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
mod tests {
    use super::*;

    // ----------------------------------------------------------------------
    // VectorClock: construction & basic accessors
    // ----------------------------------------------------------------------

    #[test]
    fn new_clock_is_empty() {
        let vc = VectorClock::new();
        assert!(vc.is_empty());
        assert_eq!(vc.len(), 0);
        assert!(vc.nodes().is_empty());
    }

    #[test]
    fn default_equals_new() {
        assert_eq!(VectorClock::default(), VectorClock::new());
    }

    #[test]
    fn with_node_initializes_to_zero() {
        let vc = VectorClock::with_node("a".to_string());
        assert_eq!(vc.len(), 1);
        assert_eq!(vc.get("a"), 0);
        assert!(!vc.is_empty());
    }

    #[test]
    fn get_unknown_node_returns_zero() {
        let vc = VectorClock::new();
        assert_eq!(vc.get("ghost"), 0);
    }

    // ----------------------------------------------------------------------
    // VectorClock: increment / set
    // ----------------------------------------------------------------------

    #[test]
    fn increment_creates_and_bumps() {
        let mut vc = VectorClock::new();
        assert_eq!(vc.increment("a"), 1); // auto-creates at 0 then +1
        assert_eq!(vc.increment("a"), 2);
        assert_eq!(vc.get("a"), 2);
        assert_eq!(vc.len(), 1);
    }

    #[test]
    fn increment_returns_new_value() {
        let mut vc = VectorClock::new();
        let returned = vc.increment("x");
        assert_eq!(returned, vc.get("x"));
    }

    #[test]
    fn set_overwrites_value() {
        let mut vc = VectorClock::new();
        vc.set("a", 42);
        assert_eq!(vc.get("a"), 42);
        vc.set("a", 7);
        assert_eq!(vc.get("a"), 7);
    }

    // ----------------------------------------------------------------------
    // VectorClock: merge
    // ----------------------------------------------------------------------

    #[test]
    fn merge_takes_elementwise_max_and_unions_nodes() {
        let mut a = VectorClock::new();
        a.set("n1", 5);
        a.set("n2", 2);

        let mut b = VectorClock::new();
        b.set("n1", 3);
        b.set("n2", 9);
        b.set("n3", 1);

        a.merge(&b);
        assert_eq!(a.get("n1"), 5); // max(5,3)
        assert_eq!(a.get("n2"), 9); // max(2,9)
        assert_eq!(a.get("n3"), 1); // added from b
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn merge_is_idempotent() {
        let mut a = VectorClock::new();
        a.set("n1", 5);
        let snapshot = a.clone();
        a.merge(&snapshot);
        assert_eq!(a, snapshot);
    }

    // ----------------------------------------------------------------------
    // VectorClock: causality (happens_before / is_concurrent)
    // ----------------------------------------------------------------------

    #[test]
    fn happens_before_strict_prefix() {
        let mut a = VectorClock::new();
        a.set("n1", 1);
        let mut b = a.clone();
        b.increment("n1"); // b = {n1:2}
        assert!(a.happens_before(&b));
        assert!(!b.happens_before(&a));
    }

    #[test]
    fn happens_before_detects_new_node_in_other() {
        // self = {n1:1}, other = {n1:1, n2:1}  => self < other
        let mut a = VectorClock::new();
        a.set("n1", 1);
        let mut b = a.clone();
        b.set("n2", 1);
        assert!(a.happens_before(&b));
        assert!(!b.happens_before(&a));
    }

    #[test]
    fn equal_clocks_do_not_happen_before_each_other() {
        let mut a = VectorClock::new();
        a.set("n1", 3);
        let b = a.clone();
        assert!(!a.happens_before(&b));
        assert!(!b.happens_before(&a));
        assert!(!a.is_concurrent(&b)); // equal => not concurrent
    }

    #[test]
    fn concurrent_clocks_have_no_ordering() {
        // a = {n1:2, n2:0}, b = {n1:0, n2:2}  -> concurrent
        let mut a = VectorClock::new();
        a.set("n1", 2);
        let mut b = VectorClock::new();
        b.set("n2", 2);
        assert!(!a.happens_before(&b));
        assert!(!b.happens_before(&a));
        assert!(a.is_concurrent(&b));
        assert!(b.is_concurrent(&a));
    }

    #[test]
    fn empty_happens_before_nonempty() {
        let empty = VectorClock::new();
        let mut nonempty = VectorClock::new();
        nonempty.set("n1", 1);
        assert!(empty.happens_before(&nonempty));
        assert!(!nonempty.happens_before(&empty));
    }

    #[test]
    fn two_empty_clocks_are_neither_ordered_nor_concurrent() {
        let a = VectorClock::new();
        let b = VectorClock::new();
        assert!(!a.happens_before(&b));
        assert!(!a.is_concurrent(&b)); // equal (both empty)
    }

    #[test]
    fn happens_before_is_transitive() {
        let mut a = VectorClock::new();
        a.set("n1", 1);
        let mut b = VectorClock::new();
        b.set("n1", 2);
        let mut c = VectorClock::new();
        c.set("n1", 3);
        assert!(a.happens_before(&b));
        assert!(b.happens_before(&c));
        assert!(a.happens_before(&c)); // transitivity
    }

    // ----------------------------------------------------------------------
    // VectorClock: dynamic membership & GC
    // ----------------------------------------------------------------------

    #[test]
    fn remove_node_returns_previous_value() {
        let mut vc = VectorClock::new();
        vc.set("a", 4);
        assert_eq!(vc.remove_node("a"), Some(4));
        assert_eq!(vc.remove_node("a"), None);
        assert!(vc.is_empty());
    }

    #[test]
    fn reset_node_zeroes_existing_only() {
        let mut vc = VectorClock::new();
        vc.set("a", 9);
        vc.reset_node("a");
        assert_eq!(vc.get("a"), 0);
        assert_eq!(vc.len(), 1); // still present
        vc.reset_node("absent"); // no-op, must not create
        assert_eq!(vc.len(), 1);
    }

    #[test]
    fn gc_zero_nodes_removes_zeros_and_counts() {
        let mut vc = VectorClock::new();
        vc.set("a", 0);
        vc.set("b", 3);
        vc.set("c", 0);
        let removed = vc.gc_zero_nodes();
        assert_eq!(removed, 2);
        assert_eq!(vc.len(), 1);
        assert_eq!(vc.get("b"), 3);
    }

    #[test]
    fn retain_active_nodes_drops_the_rest() {
        let mut vc = VectorClock::new();
        vc.set("a", 1);
        vc.set("b", 2);
        vc.set("c", 3);
        let removed = vc.retain_active_nodes(&["a".to_string(), "c".to_string()]);
        assert_eq!(removed, 1);
        assert!(vc.nodes().iter().any(|n| *n == "a"));
        assert!(vc.nodes().iter().any(|n| *n == "c"));
        assert_eq!(vc.get("b"), 0); // gone
    }

    // ----------------------------------------------------------------------
    // VectorClock: serialization round-trip (clocks travel over the wire)
    // ----------------------------------------------------------------------

    #[test]
    fn serde_round_trip_preserves_clock() {
        let mut vc = VectorClock::new();
        vc.set("n1", 7);
        vc.set("n2", 3);
        let json = serde_json::to_string(&vc).unwrap();
        let back: VectorClock = serde_json::from_str(&json).unwrap();
        assert_eq!(vc, back);
    }

    // ----------------------------------------------------------------------
    // VLCSnapshot
    // ----------------------------------------------------------------------

    #[test]
    fn snapshot_new_starts_at_logical_zero() {
        let s = VLCSnapshot::new();
        assert_eq!(s.logical_time, 0);
        assert!(s.vector_clock.is_empty());
    }

    #[test]
    fn snapshot_default_matches_new_shape() {
        let s = VLCSnapshot::default();
        assert_eq!(s.logical_time, 0);
        assert!(s.vector_clock.is_empty());
    }

    #[test]
    fn snapshot_new_with_clock_preserves_clock() {
        let mut vc = VectorClock::new();
        vc.set("a", 5);
        let s = VLCSnapshot::new_with_clock(vc.clone());
        assert_eq!(s.vector_clock, vc);
    }

    #[test]
    fn snapshot_for_node_seeds_node() {
        let s = VLCSnapshot::for_node("a".to_string());
        assert_eq!(s.vector_clock.get("a"), 0);
        assert_eq!(s.vector_clock.len(), 1);
    }

    #[test]
    fn snapshot_increment_bumps_logical_and_vector() {
        let mut s = VLCSnapshot::new();
        s.increment("a");
        assert_eq!(s.logical_time, 1);
        assert_eq!(s.vector_clock.get("a"), 1);
        s.increment("a");
        assert_eq!(s.logical_time, 2);
        assert_eq!(s.vector_clock.get("a"), 2);
    }

    #[test]
    fn snapshot_receive_merges_and_advances_logical_time() {
        // local node "a", remote node "b"
        let mut local = VLCSnapshot::for_node("a".to_string());
        local.increment("a"); // logical_time=1, a=1

        let mut remote = VLCSnapshot::for_node("b".to_string());
        remote.increment("b");
        remote.increment("b"); // remote.logical_time=2, b=2

        local.receive(&remote, "a");

        // logical_time = max(1, 2) + 1 = 3
        assert_eq!(local.logical_time, 3);
        // merged remote's b=2
        assert_eq!(local.vector_clock.get("b"), 2);
        // local node "a" incremented after merge: was 1 -> 2
        assert_eq!(local.vector_clock.get("a"), 2);
    }

    #[test]
    fn snapshot_receive_establishes_causal_order() {
        let mut sender = VLCSnapshot::for_node("s".to_string());
        sender.increment("s");
        let before_send = sender.clone();

        let mut receiver = VLCSnapshot::for_node("r".to_string());
        receiver.receive(&sender, "r");

        // The sender's pre-send state must causally precede the receiver's post-receive state.
        assert!(before_send.happens_before(&receiver));
        assert!(!receiver.happens_before(&before_send));
    }

    #[test]
    fn snapshot_happens_before_uses_logical_time_when_vectors_equal() {
        let vc = VectorClock::new();
        let mut earlier = VLCSnapshot::new_with_clock(vc.clone());
        let mut later = VLCSnapshot::new_with_clock(vc);
        earlier.logical_time = 1;
        later.logical_time = 5;
        // Equal vector clocks -> fall back to logical time comparison.
        assert!(earlier.happens_before(&later));
        assert!(!later.happens_before(&earlier));
    }

    #[test]
    fn snapshot_is_concurrent_delegates_to_vector_clock() {
        let mut a = VLCSnapshot::for_node("a".to_string());
        a.increment("a");
        let mut b = VLCSnapshot::for_node("b".to_string());
        b.increment("b");
        assert!(a.is_concurrent(&b));
        assert!(b.is_concurrent(&a));
    }

    #[test]
    fn snapshot_gc_inactive_nodes_prunes_vector_clock() {
        let mut s = VLCSnapshot::new();
        s.increment("a");
        s.increment("b");
        s.increment("c");
        let removed = s.gc_inactive_nodes(&["a".to_string()]);
        assert_eq!(removed, 2);
        assert_eq!(s.vector_clock.len(), 1);
        assert_eq!(s.vector_clock.get("a"), 1);
    }

    #[test]
    fn snapshot_serde_round_trip() {
        let mut s = VLCSnapshot::for_node("a".to_string());
        s.increment("a");
        let json = serde_json::to_string(&s).unwrap();
        let back: VLCSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
