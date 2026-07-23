//! Registration handler implementation
//!
//! Implements RegistrationHandler trait for Validator RPC.

use super::service::ValidatorNetworkService;
use super::types::{current_timestamp_millis, current_timestamp_secs, SubnetInfo};
use setu_rpc::{
    GetNodeStatusRequest, GetNodeStatusResponse, GetSolverListRequest, GetSolverListResponse,
    GetValidatorListRequest, GetValidatorListResponse, HeartbeatRequest, HeartbeatResponse,
    GetSubnetListRequest, GetSubnetListResponse,
    NodeType, RegisterSolverRequest, RegisterSolverResponse, RegisterValidatorRequest,
    RegisterValidatorResponse, RegisterSubnetRequest, RegisterSubnetResponse,
    RegistrationHandler, SolverListItem, UnregisterRequest,
    UnregisterResponse,
};
use setu_types::{Event, SolverRegistration};
use setu_types::registration::{SubnetRegistration, SubnetResourceLimits, TokenConfig};
use setu_types::subnet::SubnetType;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Registration handler implementation for Validator
pub struct ValidatorRegistrationHandler {
    pub(crate) service: Arc<ValidatorNetworkService>,
}

fn validate_public_subnet_id(raw: &str) -> Result<String, &'static str> {
    // Grammar lives in types so the D1 resolver and registration cannot drift.
    setu_types::SubnetId::validate_public_id_grammar(raw)?;
    Ok(raw.to_string())
}

fn validate_public_subnet_name(raw: &str) -> Result<String, &'static str> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("Invalid subnet name: must not be empty");
    }
    if value != raw {
        return Err("Invalid subnet name: leading/trailing whitespace is not allowed");
    }
    if raw.chars().count() > 128 {
        return Err("Invalid subnet name: length must be <= 128 characters");
    }
    if raw.chars().any(|ch| ch.is_control()) {
        return Err("Invalid subnet name: control characters are not allowed");
    }

    Ok(raw.to_string())
}

#[async_trait::async_trait]
impl RegistrationHandler for ValidatorRegistrationHandler {
    async fn register_solver(&self, request: RegisterSolverRequest) -> RegisterSolverResponse {
        info!(
            solver_id = %request.solver_id,
            address = %request.address,
            port = request.port,
            account_address = %request.account_address,
            capacity = request.capacity,
            shard_id = ?request.shard_id,
            "Processing solver registration"
        );

        // Check if already registered
        if self
            .service
            .router_manager()
            .get_solver(&request.solver_id)
            .is_some()
        {
            warn!(solver_id = %request.solver_id, "Solver already registered, will update");
        }

        // Create registration event
        let vlc_time = self.service.get_vlc_time();
        let mut vlc = setu_vlc::VectorClock::new();
        vlc.increment(self.service.validator_id());
        let vlc_snapshot = setu_vlc::VLCSnapshot {
            vector_clock: vlc,
            logical_time: vlc_time,
            physical_time: current_timestamp_millis(),
        };

        let registration = SolverRegistration::new(
            request.solver_id.clone(),
            request.address.clone(),
            request.port,
            request.account_address.clone(),
            request.public_key.clone(),
            request.signature.clone(),
        )
        .with_capacity(request.capacity)
        .with_shard(request.shard_id.clone().unwrap_or_default())
        .with_resources(request.resources.clone());

        let mut event = Event::solver_register(
            registration,
            vec![],
            vlc_snapshot,
            request.solver_id.clone(),
        );

        event.set_execution_result(setu_types::event::ExecutionResult {
            success: true,
            message: Some("Solver registration executed".to_string()),
            state_changes: vec![setu_types::event::StateChange {
                key: format!("solver:{}", request.solver_id),
                old_value: None,
                new_value: Some(
                    format!("registered:{}:{}", request.address, request.port).into_bytes(),
                ),
                target_subnet: None,
            }],
        });

        // Add event to DAG (async to support consensus submission)
        let event_id = event.id.clone();
        let submit_response = self.service.add_event_to_dag(event).await;
        if !submit_response.success {
            warn!(
                solver_id = %request.solver_id,
                message = %submit_response.message,
                "Solver registration DAG submission failed"
            );
            return RegisterSolverResponse {
                success: false,
                message: submit_response.message,
                assigned_id: None,
            };
        }

        let _channel = self.service.register_solver_internal(&request);

        info!(
            solver_id = %request.solver_id,
            event_id = %&event_id[..20.min(event_id.len())],
            total_solvers = self.service.solver_count(),
            "Solver registered successfully"
        );

        RegisterSolverResponse {
            success: true,
            message: "Solver registered successfully".to_string(),
            assigned_id: Some(request.solver_id),
        }
    }

    async fn register_validator(&self, request: RegisterValidatorRequest) -> RegisterValidatorResponse {
        info!(
            validator_id = %request.validator_id,
            address = %request.address,
            port = request.port,
            account_address = %request.account_address,
            stake_amount = request.stake_amount,
            "Processing validator registration"
        );

        // Validator membership changes quorum and leader election. This public
        // endpoint has no authenticated authorization path, and the current
        // registration signature placeholder is not a cryptographic proof.
        // Accepting it would let any HTTP caller alter the local consensus set.
        warn!(
            validator_id = %request.validator_id,
            "Rejecting unauthenticated dynamic validator registration"
        );
        RegisterValidatorResponse {
            success: false,
            message: "Dynamic validator registration is disabled until authenticated governance and finalized epoch activation are implemented".to_string(),
        }
    }

    async fn register_subnet(&self, request: RegisterSubnetRequest) -> RegisterSubnetResponse {
        let subnet_id = match validate_public_subnet_id(&request.subnet_id) {
            Ok(value) => value,
            Err(message) => {
                return RegisterSubnetResponse {
                    success: false,
                    message: message.to_string(),
                    subnet_id: None,
                    event_id: None,
                };
            }
        };

        let subnet_name = match validate_public_subnet_name(&request.name) {
            Ok(value) => value,
            Err(message) => {
                return RegisterSubnetResponse {
                    success: false,
                    message: message.to_string(),
                    subnet_id: None,
                    event_id: None,
                };
            }
        };

        info!(
            subnet_id = %subnet_id,
            name = %subnet_name,
            owner = %request.owner,
            token_symbol = %request.token_symbol,
            "Processing subnet registration"
        );

        // Check if already registered
        if self.service.get_subnet_info(&subnet_id).is_some() {
            warn!(subnet_id = %subnet_id, "Subnet already registered");
            return RegisterSubnetResponse {
                success: false,
                message: format!("Subnet '{}' is already registered", subnet_id),
                subnet_id: Some(subnet_id),
                event_id: None,
            };
        }

        // Validate owner address (must be 0x + 64 hex chars = 66 total)
        if !request.owner.starts_with("0x")
            || request.owner.len() != 66
            || !request.owner[2..].chars().all(|c| c.is_ascii_hexdigit())
        {
            return RegisterSubnetResponse {
                success: false,
                message: "Invalid owner address format (expected 0x + 64 hex chars)".to_string(),
                subnet_id: None,
                event_id: None,
            };
        }

        // Validate token_symbol: 1-10 uppercase alphanumeric characters
        if request.token_symbol.is_empty()
            || request.token_symbol.len() > 10
            || !request.token_symbol.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return RegisterSubnetResponse {
                success: false,
                message: "Invalid token_symbol (must be 1-10 uppercase alphanumeric characters)".to_string(),
                subnet_id: None,
                event_id: None,
            };
        }

        // Parse subnet type
        let subnet_type = match request.subnet_type.as_deref() {
            None => SubnetType::App,
            Some(t) => match t.to_ascii_lowercase().as_str() {
                "app" | "application" => SubnetType::App,
                "organization" | "org" => SubnetType::Organization,
                "personal" => SubnetType::Personal,
                other => {
                    return RegisterSubnetResponse {
                        success: false,
                        message: format!("Unknown subnet type '{}' (valid: app, organization, personal)", other),
                        subnet_id: None,
                        event_id: None,
                    };
                }
            },
        };

        // Build resource limits
        let resource_limits = if request.max_tps.is_some() || request.max_storage_bytes.is_some() {
            let mut limits = SubnetResourceLimits::new();
            if let Some(tps) = request.max_tps {
                limits = limits.with_tps(tps);
            }
            if let Some(storage) = request.max_storage_bytes {
                limits = limits.with_storage(storage);
            }
            Some(limits)
        } else {
            None
        };

        // Build token config
        let token_config = TokenConfig {
            decimals: request.token_decimals.unwrap_or(8),
            max_supply: request.token_max_supply,
            mintable: request.token_mintable.unwrap_or(false),
            burnable: request.token_burnable.unwrap_or(true),
        };

        // Build SubnetRegistration
        let mut registration = SubnetRegistration::new(
            subnet_id.clone(),
            subnet_name.clone(),
            request.owner.clone(),
            request.token_symbol.clone(),
        )
        .with_type(subnet_type)
        .with_token_config(token_config);

        if let Some(desc) = &request.description {
            registration = registration.with_description(desc.clone());
        }
        if let Some(parent) = &request.parent_subnet_id {
            registration = registration.with_parent(parent.clone());
        }
        if let Some(max_users) = request.max_users {
            registration = registration.with_max_users(max_users);
        }
        if let Some(limits) = resource_limits {
            registration = registration.with_limits(limits);
        }
        if let Some(supply) = request.initial_token_supply {
            registration = registration.with_initial_supply(supply);
        }
        if let Some(airdrop) = request.user_airdrop_amount {
            registration = registration.with_user_airdrop(airdrop);
        }
        if !request.assigned_solvers.is_empty() {
            registration = registration.with_solvers(request.assigned_solvers.clone());
        }

        // Create VLC snapshot
        let vlc_time = self.service.get_vlc_time();
        let mut vlc = setu_vlc::VectorClock::new();
        vlc.increment(self.service.validator_id());
        let vlc_snapshot = setu_vlc::VLCSnapshot {
            vector_clock: vlc,
            logical_time: vlc_time,
            physical_time: current_timestamp_millis(),
        };

        // Delegate to InfraExecutor (路径 B) — G11-compliant "oid:{hex}" state keys,
        // and mint_tokens() is actually executed when initial_token_supply > 0.
        let event = match self
            .service
            .infra_executor()
            .execute_subnet_register(&registration, vlc_snapshot)
        {
            Ok(event) => event,
            Err(e) => {
                tracing::error!(subnet_id = %subnet_id, error = %e,
                    "InfraExecutor subnet registration failed");
                return RegisterSubnetResponse {
                    success: false,
                    message: format!("Subnet registration failed: {}", e),
                    subnet_id: None,
                    event_id: None,
                };
            }
        };

        let event_id = event.id.clone();

        // Add event to DAG
        let submit_response = self.service.add_event_to_dag(event).await;
        if !submit_response.success {
            warn!(
                subnet_id = %subnet_id,
                message = %submit_response.message,
                "Subnet registration DAG submission failed"
            );
            return RegisterSubnetResponse {
                success: false,
                message: submit_response.message,
                subnet_id: None,
                event_id: None,
            };
        }

        self.service.add_subnet(SubnetInfo {
            canonical_id: setu_types::SubnetId::parse_public_or_hex(&subnet_id)
                .unwrap_or_else(|_| setu_types::SubnetId::from_str_id(&subnet_id)),
            subnet_id: subnet_id.clone(),
            name: subnet_name,
            owner: request.owner.clone(),
            subnet_type: format!("{:?}", registration.subnet_type),
            token_symbol: request.token_symbol.clone(),
            status: "active".to_string(),
            registered_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        });

        info!(
            subnet_id = %subnet_id,
            event_id = %&event_id[..20.min(event_id.len())],
            "Subnet registered successfully"
        );

        RegisterSubnetResponse {
            success: true,
            message: "Subnet registered successfully".to_string(),
            subnet_id: Some(subnet_id),
            event_id: Some(event_id),
        }
    }

    async fn unregister(&self, request: UnregisterRequest) -> UnregisterResponse {
        info!(
            node_id = %request.node_id,
            node_type = %request.node_type,
            "Processing unregister request"
        );

        match request.node_type {
            NodeType::Solver => {
                self.service.unregister_solver(&request.node_id);
                UnregisterResponse {
                    success: true,
                    message: "Solver unregistered successfully".to_string(),
                }
            }
            NodeType::Validator => {
                warn!(
                    node_id = %request.node_id,
                    "Rejecting unauthenticated dynamic validator unregistration"
                );
                UnregisterResponse {
                    success: false,
                    message: "Dynamic validator unregistration is disabled until authenticated governance and finalized epoch activation are implemented".to_string(),
                }
            }
        }
    }

    async fn heartbeat(&self, request: HeartbeatRequest) -> HeartbeatResponse {
        debug!(
            node_id = %request.node_id,
            current_load = ?request.current_load,
            "Processing heartbeat"
        );

        // Check if the solver is known to the RouterManager.
        // `acknowledged` means "recognized as locally routable on THIS validator",
        // not "alive". After a validator restart the RouterManager is empty —
        // solvers that were previously registered will send heartbeats but won't
        // be found here. Return acknowledged=false so the solver re-registers.
        // Note: behind a round-robin gateway this can flip per request; solver
        // lifecycle traffic should be pinned to the owner validator (see
        // docs/feat/solver-local-resource-model/design.md).
        let is_known = self.service
            .router_manager()
            .get_solver(&request.node_id)
            .is_some();

        if !is_known {
            info!(
                node_id = %request.node_id,
                "Heartbeat from unknown solver — returning acknowledged=false to trigger re-registration"
            );
            return HeartbeatResponse {
                acknowledged: false,
                server_timestamp: current_timestamp_secs(),
            };
        }

        if let Some(load) = request.current_load {
            self.service
                .router_manager()
                .update_solver_load(&request.node_id, load);
        }

        HeartbeatResponse {
            acknowledged: true,
            server_timestamp: current_timestamp_secs(),
        }
    }

    async fn get_solver_list(&self, request: GetSolverListRequest) -> GetSolverListResponse {
        // Read from solver_info DashMap (includes both live and replayed solvers)
        let solvers = self.service.get_all_solvers();

        let solver_list: Vec<SolverListItem> = solvers
            .into_iter()
            .filter(|s| {
                if let Some(ref shard_id) = request.shard_id {
                    s.shard_id.as_ref() == Some(shard_id)
                } else {
                    true
                }
            })
            .map(|s| {
                // Routability is validator-local: a solver is only routable if it
                // lives in THIS validator's RouterManager. Registry membership
                // (s being present) does not imply routability (e.g. replayed-only
                // entries after restart, before the solver re-registers).
                let routed = self.service.router_manager().get_solver(&s.solver_id);
                let routable = routed.is_some();
                let current_load = routed.map(|r| r.current_load).unwrap_or(0);
                SolverListItem {
                    solver_id: s.solver_id,
                    address: s.address.clone(),
                    port: s.port,
                    account_address: None,
                    capacity: s.capacity,
                    current_load,
                    status: s.status,
                    shard_id: s.shard_id,
                    routable,
                }
            })
            .collect();

        GetSolverListResponse {
            solvers: solver_list,
        }
    }

    async fn get_validator_list(&self, _request: GetValidatorListRequest) -> GetValidatorListResponse {
        GetValidatorListResponse {
            validators: self.service.get_validator_list(),
        }
    }

    async fn get_subnet_list(&self, request: GetSubnetListRequest) -> GetSubnetListResponse {
        let mut subnets = self.service.get_subnet_list();

        if let Some(ref type_filter) = request.type_filter {
            subnets.retain(|s| s.subnet_type.eq_ignore_ascii_case(type_filter));
        }
        if let Some(ref owner_filter) = request.owner_filter {
            subnets.retain(|s| s.owner == *owner_filter);
        }

        GetSubnetListResponse { subnets }
    }

    async fn get_node_status(&self, request: GetNodeStatusRequest) -> GetNodeStatusResponse {
        // Check if it's a solver
        if let Some(solver) = self.service.router_manager().get_solver(&request.node_id) {
            return GetNodeStatusResponse {
                found: true,
                node_id: request.node_id,
                node_type: Some(NodeType::Solver),
                status: Some(format!("{:?}", solver.status)),
                address: Some(solver.address.split(':').next().unwrap_or("").to_string()),
                port: solver
                    .address
                    .split(':')
                    .nth(1)
                    .and_then(|p| p.parse().ok()),
                uptime_seconds: None,
            };
        }

        // Check if it's a validator
        if let Some(uptime) = self.service.get_validator_uptime(&request.node_id) {
            let info = self.service.get_validator_info(&request.node_id);
            return GetNodeStatusResponse {
                found: true,
                node_id: request.node_id,
                node_type: Some(NodeType::Validator),
                status: info.as_ref().map(|v| v.status.clone()),
                address: info.as_ref().map(|v| v.address.clone()),
                port: info.map(|v| v.port),
                uptime_seconds: Some(uptime),
            };
        }

        // Check if it's this validator
        if request.node_id == self.service.validator_id() {
            return GetNodeStatusResponse {
                found: true,
                node_id: request.node_id,
                node_type: Some(NodeType::Validator),
                status: Some("online".to_string()),
                address: None,
                port: None,
                uptime_seconds: Some(current_timestamp_secs() - self.service.start_time()),
            };
        }

        GetNodeStatusResponse {
            found: false,
            node_id: request.node_id,
            node_type: None,
            status: None,
            address: None,
            port: None,
            uptime_seconds: None,
        }
    }
}

#[cfg(test)]
mod solver_routable_tests {
    use super::*;
    use setu_api::ValidatorService;
    use setu_rpc::RegistrationHandler;

    fn make_service() -> Arc<ValidatorNetworkService> {
        let router_manager = Arc::new(crate::RouterManager::new());
        let task_preparer =
            Arc::new(crate::TaskPreparer::new_for_testing("test-validator".to_string()));
        let batch_task_preparer =
            Arc::new(crate::BatchTaskPreparer::new_for_testing("test-validator".to_string()));
        let config = crate::NetworkServiceConfig::default();
        Arc::new(ValidatorNetworkService::new(
            "test-validator".to_string(),
            router_manager,
            task_preparer,
            batch_task_preparer,
            config,
        ))
    }

    fn solver_request(id: &str) -> RegisterSolverRequest {
        RegisterSolverRequest {
            solver_id: id.to_string(),
            address: "127.0.0.1".to_string(),
            port: 9000,
            account_address: "0xabc".to_string(),
            public_key: vec![],
            signature: vec![],
            capacity: 8,
            shard_id: None,
            assigned_shard: None,
            resources: vec![],
            permitted_subnets: vec![],
        }
    }

    /// A live-registered solver is both counted as routable and appears with
    /// `routable = true` in the solver list.
    #[tokio::test]
    async fn tc_routable_when_in_router() {
        let service = make_service();
        // register_solver_internal populates BOTH solver_info and RouterManager.
        service.register_solver_internal(&solver_request("solver-A"));

        assert_eq!(service.registered_solver_count(), 1);
        assert_eq!(service.router_manager().solver_count(), 1);

        let handler = ValidatorRegistrationHandler {
            service: Arc::clone(&service),
        };
        let resp = handler
            .get_solver_list(GetSolverListRequest {
                shard_id: None,
                status_filter: None,
            })
            .await;
        assert_eq!(resp.solvers.len(), 1);
        assert!(resp.solvers[0].routable, "live solver must be routable");
    }

    /// A solver present in the registry (`solver_info`) but absent from this
    /// validator's RouterManager — simulating a replayed-only entry after
    /// restart — reports `routable = false` while still being listed.
    #[tokio::test]
    async fn tc_not_routable_when_router_evicted() {
        let service = make_service();
        service.register_solver_internal(&solver_request("solver-B"));

        // Evict only from the routing layer (registry entry survives), mimicking
        // a replayed-only solver before it re-registers.
        service.router_manager().unregister_solver("solver-B");

        assert_eq!(service.registered_solver_count(), 1);
        assert_eq!(service.router_manager().solver_count(), 0);

        let handler = ValidatorRegistrationHandler {
            service: Arc::clone(&service),
        };
        let resp = handler
            .get_solver_list(GetSolverListRequest {
                shard_id: None,
                status_filter: None,
            })
            .await;
        assert_eq!(resp.solvers.len(), 1);
        assert!(
            !resp.solvers[0].routable,
            "registry-only solver must report routable = false"
        );
    }
}
