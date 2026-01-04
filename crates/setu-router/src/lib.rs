//! Setu Router - Transaction Routing Module
//!
//! Routes transactions to appropriate solvers for execution.
//!
//! # Network Topology (MVP)
//!
//! ```text
//! Light Nodes → Validators (3) → Router → Solvers (6)
//! ```
//!
//! # MVP Stage
//!
//! - Single region with 3 validators
//! - Single shard (default) with 6 solvers
//! - Consistent hash routing based on transaction resources
//!
//! # Example
//!
//! ```rust,ignore
//! use setu_router::Router;
//!
//! let router = Router::new_mvp();
//! let decision = router.route(&transfer)?;
//! println!("Route to solver: {}", decision.solver_id);
//! ```

mod error;
mod shard;
mod solver;
mod router;
mod strategy;

pub use error::RouterError;
pub use shard::{ShardConfig, ShardId, ShardRouter, SingleShardRouter, DEFAULT_SHARD_ID};
pub use solver::{SolverInfo, SolverId, SolverRegistry, SolverStatus};
pub use router::{Router, RouterConfig, RoutingDecision};
pub use strategy::{RoutingStrategy, ConsistentHashStrategy, LoadBalancedStrategy};

#[cfg(test)]
mod tests;

